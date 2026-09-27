//! iRacing [`SimSource`]: turns shared-memory snapshots into [`PollResult`]s.

use super::layout::{parse_var_headers, IrHeader, HEADER_LEN, VAR_HEADER_LEN};
use super::mapping::VarMap;
use super::yaml::session_from_yaml;
use crate::sim::{PollResult, Sim, SimSource};

/// Max time one poll blocks waiting for the sim to publish a sample.
const WAIT_MS: u32 = 100;

/// Read access to the sim's shared memory (real on Windows, fake in tests).
pub trait SharedMem {
    /// Copy `len` bytes at `offset`, or None if the range is out of bounds.
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>>;
    /// Block until the sim signals new data or `timeout_ms` elapses.
    fn wait_for_data(&self, timeout_ms: u32);
}

struct Disconnected;

struct Connection<M> {
    mem: M,
    vars: VarMap,
    var_layout: Option<(i32, i32)>,
    last_tick: Option<i32>,
    session_info_update: Option<i32>,
    session_num: Option<i32>,
    session_key: Option<String>,
}

impl<M: SharedMem> Connection<M> {
    fn new(mem: M) -> Self {
        Self {
            mem,
            vars: VarMap::default(),
            var_layout: None,
            last_tick: None,
            session_info_update: None,
            session_num: None,
            session_key: None,
        }
    }

    fn header(&self) -> Result<IrHeader, Disconnected> {
        let bytes = self.mem.read(0, HEADER_LEN).ok_or(Disconnected)?;
        IrHeader::parse(&bytes).map_err(|_| Disconnected)
    }

    fn poll(&mut self) -> Result<PollResult, Disconnected> {
        self.mem.wait_for_data(WAIT_MS);
        let header = self.header()?;
        if !header.is_connected() {
            return Err(Disconnected);
        }

        let layout = (header.num_vars, header.var_header_offset);
        if self.var_layout != Some(layout) {
            let n = header.num_vars.max(0) as usize;
            let bytes = self
                .mem
                .read(header.var_header_offset.max(0) as usize, n * VAR_HEADER_LEN)
                .ok_or(Disconnected)?;
            let vars = parse_var_headers(&bytes, n).map_err(|_| Disconnected)?;
            self.vars = VarMap::resolve(&vars);
            self.var_layout = Some(layout);
        }

        let Some(latest) = header.latest_buf() else { return Ok(PollResult::Paused) };
        if self.last_tick == Some(latest.tick_count) {
            return Ok(PollResult::Paused);
        }
        let buf = self
            .mem
            .read(latest.buf_offset.max(0) as usize, header.buf_len.max(0) as usize)
            .ok_or(Disconnected)?;
        // If the sim rewrote this slot while we copied it, skip this sample.
        let still_same = self
            .header()?
            .var_bufs
            .iter()
            .any(|b| b.buf_offset == latest.buf_offset && b.tick_count == latest.tick_count);
        if !still_same {
            return Ok(PollResult::Paused);
        }
        self.last_tick = Some(latest.tick_count);

        let Some((frame, control)) = self.vars.read(&buf) else { return Ok(PollResult::Idle) };

        if self.session_info_update != Some(header.session_info_update)
            || self.session_num != Some(control.session_num)
        {
            let raw = self
                .mem
                .read(header.session_info_offset.max(0) as usize, header.session_info_len.max(0) as usize)
                .ok_or(Disconnected)?;
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            let parsed = session_from_yaml(&String::from_utf8_lossy(&raw[..end]), control.session_num);
            self.session_info_update = Some(header.session_info_update);
            self.session_num = Some(control.session_num);
            if self.session_key.as_deref() != Some(parsed.key.as_str()) {
                self.session_key = Some(parsed.key);
                self.last_tick = None; // deliver this same sample on the next poll
                return Ok(PollResult::Session(parsed.info));
            }
        }

        if control.is_replay || !control.is_on_track {
            return Ok(PollResult::Idle);
        }
        Ok(PollResult::Frame(frame))
    }
}

pub struct IracingSource<M: SharedMem> {
    connect: Box<dyn FnMut() -> Option<M>>,
    conn: Option<Connection<M>>,
}

impl<M: SharedMem> IracingSource<M> {
    pub fn new(connect: Box<dyn FnMut() -> Option<M>>) -> Self {
        Self { connect, conn: None }
    }
}

impl<M: SharedMem> SimSource for IracingSource<M> {
    fn sim(&self) -> Sim {
        Sim::Iracing
    }

    fn poll(&mut self) -> PollResult {
        if self.conn.is_none() {
            match (self.connect)() {
                Some(mem) => self.conn = Some(Connection::new(mem)),
                None => return PollResult::NotConnected,
            }
        }
        let conn = self.conn.as_mut().expect("connected");
        match conn.poll() {
            Ok(result) => result,
            Err(Disconnected) => {
                self.conn = None;
                PollResult::NotConnected
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Channel;
    use crate::sim::iracing::layout::VarType;
    use crate::sim::iracing::testimage::{ImageBuilder, SAMPLE_YAML};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct FakeMem(Rc<RefCell<Vec<u8>>>);

    impl SharedMem for FakeMem {
        fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
            self.0.borrow().get(offset..offset + len).map(|s| s.to_vec())
        }
        fn wait_for_data(&self, _timeout_ms: u32) {}
    }

    struct Rig {
        src: IracingSource<FakeMem>,
        b: ImageBuilder,
        img: Rc<RefCell<Vec<u8>>>,
        running: Rc<Cell<bool>>,
    }

    impl Rig {
        fn new() -> Rig {
            let mut b = ImageBuilder::new()
                .var("SessionTime", VarType::Double)
                .var("Lap", VarType::Int)
                .var("LapLastLapTime", VarType::Float)
                .var("LapDistPct", VarType::Float)
                .var("Speed", VarType::Float)
                .var("Clutch", VarType::Float)
                .var("PlayerTrackSurface", VarType::Int)
                .var("IsReplayPlaying", VarType::Bool)
                .var("IsOnTrack", VarType::Bool)
                .var("SessionNum", VarType::Int)
                .var("OnPitRoad", VarType::Bool);
            b.yaml = SAMPLE_YAML.to_string();
            b.set("SessionTime", 1234.5)
                .set("Lap", 3.0)
                .set("LapLastLapTime", -1.0)
                .set("LapDistPct", 0.25)
                .set("Speed", 55.5)
                .set("Clutch", 0.25)
                .set("PlayerTrackSurface", 3.0) // OnTrack
                .set("IsOnTrack", 1.0);
            let img = Rc::new(RefCell::new(b.build()));
            let running = Rc::new(Cell::new(true));
            let (i2, r2) = (img.clone(), running.clone());
            let src = IracingSource::new(Box::new(move || r2.get().then(|| FakeMem(i2.clone()))));
            Rig { src, b, img, running }
        }

        /// Publish the builder's current state as a new tick.
        fn tick(&mut self) {
            self.b.tick += 1;
            *self.img.borrow_mut() = self.b.build();
        }
    }

    fn expect_frame(r: PollResult) -> crate::frame::Frame {
        match r {
            PollResult::Frame(f) => f,
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn not_running_reports_not_connected() {
        let mut rig = Rig::new();
        rig.running.set(false);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn first_poll_announces_session_then_frames() {
        let mut rig = Rig::new();
        match rig.src.poll() {
            PollResult::Session(info) => {
                assert_eq!(info.track_name, "Circuit de Spa-Francorchamps");
                assert_eq!(info.car_name, "Porsche 963 GTP");
                assert_eq!(info.session_type, "Practice");
            }
            other => panic!("expected Session, got {other:?}"),
        }
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.session_time_s, 1234.5);
        assert_eq!(f.lap, 3);
        assert_eq!(f.sim_last_lap_time_s, None);
        assert_eq!(f.get(Channel::SpeedMs), 55.5);
        assert_eq!(f.get(Channel::Clutch), 0.75);
        assert_eq!(f.get(Channel::OffTrack), 0.0);
        assert_eq!(f.get(Channel::OnPitRoad), 0.0);
        assert!(f.get(Channel::PosX).is_nan());
    }

    #[test]
    fn same_tick_is_paused_new_tick_is_frame() {
        let mut rig = Rig::new();
        rig.src.poll(); // Session
        expect_frame(rig.src.poll());
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        rig.b.set("SessionTime", 1234.6).set("LapLastLapTime", 101.25);
        rig.tick();
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.session_time_s, 1234.6);
        assert_eq!(f.sim_last_lap_time_s, Some(101.25));
    }

    #[test]
    fn replay_or_off_car_is_idle() {
        let mut rig = Rig::new();
        rig.src.poll();
        rig.b.set("IsReplayPlaying", 1.0);
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
        rig.b.set("IsReplayPlaying", 0.0).set("IsOnTrack", 0.0);
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
    }

    #[test]
    fn off_track_surface_sets_flag() {
        let mut rig = Rig::new();
        rig.src.poll();
        rig.b.set("PlayerTrackSurface", 0.0);
        rig.tick();
        assert_eq!(expect_frame(rig.src.poll()).get(Channel::OffTrack), 1.0);
    }

    #[test]
    fn session_num_change_emits_new_session() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.set("SessionNum", 1.0);
        rig.tick();
        match rig.src.poll() {
            PollResult::Session(info) => assert_eq!(info.session_type, "Race"),
            other => panic!("expected Session, got {other:?}"),
        }
        expect_frame(rig.src.poll()); // the same sample follows
    }

    #[test]
    fn yaml_update_with_same_identity_is_not_a_new_session() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.session_info_update += 1; // e.g. results changed
        rig.tick();
        expect_frame(rig.src.poll());
    }

    #[test]
    fn sim_exit_then_reconnect() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.status = 0; // sim shutting down
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.running.set(false);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.running.set(true);
        rig.b.status = 1;
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Session(_)));
        expect_frame(rig.src.poll());
    }

    #[test]
    fn truncated_memory_is_not_connected_not_a_panic() {
        let mut rig = Rig::new();
        rig.img.borrow_mut().truncate(50);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
    }
}
