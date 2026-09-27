//! iRacing [`SimSource`]: turns shared-memory snapshots into [`PollResult`]s.

use super::layout::{parse_var_headers, IrHeader, HEADER_LEN, VAR_HEADER_LEN};
use super::mapping::VarMap;
use super::yaml::{decode_cp1252, session_from_yaml, ParsedSession};
use crate::sim::{PollResult, SessionInfo, Sim, SimSource};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Max time one poll blocks waiting for the sim to publish a sample.
const WAIT_MS: u32 = 100;
/// A connected sim that publishes no new tick for this long has crashed or hung.
const STALE_AFTER: Duration = Duration::from_secs(10);

/// Read access to the sim's shared memory (real on Windows, fake in tests).
pub trait SharedMem {
    /// Copy `len` bytes at `offset`, or None if the range is out of bounds.
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>>;
    /// Block until the sim signals new data or `timeout_ms` elapses.
    fn wait_for_data(&self, timeout_ms: u32);
}

struct Disconnected;

/// Debug dump of the raw bytes behind a connection, for building parser fixtures:
/// `header-vars-<conn>.bin` (header + var headers, once) and `session-<conn>-<update>.yaml`
/// (each distinct session-info update, as the sim wrote it).
struct RawDump {
    dir: PathBuf,
    connection: u32,
    layout_written: bool,
}

impl RawDump {
    fn write(&self, name: &str, bytes: &[u8]) {
        let result = std::fs::create_dir_all(&self.dir).and_then(|()| std::fs::write(self.dir.join(name), bytes));
        if let Err(e) = result {
            log::warn!("raw dump: couldn't write {name}: {e}");
        }
    }
}

struct Connection<M> {
    mem: M,
    dump: Option<RawDump>,
    vars: VarMap,
    var_layout: Option<(i32, i32)>,
    /// Tick of the newest sample already delivered.
    last_tick: Option<i32>,
    /// Session time of the newest Frame already delivered in the current session.
    last_frame_time_s: Option<f64>,
    /// Newest tick in the header, and when we first saw it (for staleness).
    seen_tick: Option<i32>,
    seen_at: Instant,
    /// The tick a previous connection went stale on; while the header still shows it,
    /// the sim is still frozen and this connection counts as disconnected.
    frozen_tick: Option<i32>,
    session_info_update: Option<i32>,
    session_num: Option<i32>,
    session_key: Option<String>,
    /// A new session identity read without a SessionNum change. It only becomes the
    /// session if the next read agrees, so one garbled read can't split the session.
    candidate_key: Option<String>,
}

impl<M: SharedMem> Connection<M> {
    fn new(mem: M, dump: Option<RawDump>, now: Instant, frozen_tick: Option<i32>) -> Self {
        Self {
            mem,
            dump,
            vars: VarMap::default(),
            var_layout: None,
            last_tick: None,
            last_frame_time_s: None,
            seen_tick: None,
            seen_at: now,
            frozen_tick,
            session_info_update: None,
            session_num: None,
            session_key: None,
            candidate_key: None,
        }
    }

    fn header(&self) -> Result<IrHeader, Disconnected> {
        let bytes = self.mem.read(0, HEADER_LEN).ok_or(Disconnected)?;
        IrHeader::parse(&bytes).map_err(|_| Disconnected)
    }

    fn poll(&mut self, now: Instant) -> Result<PollResult, Disconnected> {
        self.mem.wait_for_data(WAIT_MS);
        let header = self.header()?;
        if !header.is_connected() {
            return Err(Disconnected);
        }

        let newest = header.latest_buf().map(|b| b.tick_count);
        if newest.is_some() && newest == self.frozen_tick {
            return Err(Disconnected);
        }
        self.frozen_tick = None;
        if newest != self.seen_tick {
            self.seen_tick = newest;
            self.seen_at = now;
        } else if now.saturating_duration_since(self.seen_at) >= STALE_AFTER {
            // The sim crashed or hung but its memory map still says "connected".
            self.frozen_tick = newest;
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
            if let Some(dump) = self.dump.as_mut().filter(|d| !d.layout_written) {
                let mut raw = self.mem.read(0, HEADER_LEN).ok_or(Disconnected)?;
                raw.extend_from_slice(&bytes);
                dump.write(&format!("header-vars-{}.bin", dump.connection), &raw);
                dump.layout_written = true;
            }
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
            || self.candidate_key.is_some()
        {
            let Some(yaml) = self.read_session_yaml(&header)? else {
                // Torn copy: retry this sample (and the YAML) on the next poll.
                self.last_tick = None;
                return Ok(PollResult::Paused);
            };
            let parsed = session_from_yaml(&decode_cp1252(&yaml), control.session_num);
            if let Some(info) = self.on_session_info(parsed, control.session_num, header.session_info_update) {
                // Re-read the newest sample on the next poll so it follows the Session.
                self.last_tick = None;
                self.last_frame_time_s = None;
                return Ok(PollResult::Session(info));
            }
        }

        if self.session_key.is_none() || control.is_replay || !control.is_on_track {
            return Ok(PollResult::Idle);
        }
        // A tick with no forward progress in SessionTime (e.g. a rotated buffer while the
        // sim itself is paused) isn't a new sample.
        if self.last_frame_time_s.is_some_and(|last| frame.session_time_s <= last) {
            return Ok(PollResult::Paused);
        }
        self.last_frame_time_s = Some(frame.session_time_s);
        Ok(PollResult::Frame(frame))
    }

    /// Copy the session-info YAML (up to its NUL), or None if the sim rewrote it while
    /// we copied it.
    fn read_session_yaml(&self, header: &IrHeader) -> Result<Option<Vec<u8>>, Disconnected> {
        let mut raw = self
            .mem
            .read(header.session_info_offset.max(0) as usize, header.session_info_len.max(0) as usize)
            .ok_or(Disconnected)?;
        if self.header()?.session_info_update != header.session_info_update {
            return Ok(None);
        }
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        raw.truncate(end);
        if let Some(dump) = &self.dump {
            if self.session_info_update != Some(header.session_info_update) {
                dump.write(&format!("session-{}-{}.yaml", dump.connection, header.session_info_update), &raw);
            }
        }
        Ok(Some(raw))
    }

    /// Record a freshly parsed session info; returns the SessionInfo to announce if it
    /// starts a new session.
    fn on_session_info(&mut self, parsed: ParsedSession, session_num: i32, update: i32) -> Option<SessionInfo> {
        let num_changed = self.session_num != Some(session_num);
        self.session_info_update = Some(update);
        self.session_num = Some(session_num);
        if self.session_key.as_deref() == Some(parsed.key.as_str()) {
            self.candidate_key = None;
            return None;
        }
        let identified = !parsed.info.track_id.is_empty() && !parsed.info.car_id.is_empty();
        if self.session_key.is_some() && !num_changed {
            // Same session number but a different identity: most likely a garbled
            // read. Believe it only when the next read says the same.
            if !identified {
                self.candidate_key = None;
                return None;
            }
            if self.candidate_key.as_deref() != Some(parsed.key.as_str()) {
                self.candidate_key = Some(parsed.key);
                return None;
            }
        } else if !identified {
            // We can't tell which track or car this is: no session to record into.
            self.session_key = None;
            self.candidate_key = None;
            return None;
        }
        self.session_key = Some(parsed.key);
        self.candidate_key = None;
        Some(parsed.info)
    }
}

pub struct IracingSource<M: SharedMem> {
    connect: Box<dyn FnMut() -> Option<M>>,
    conn: Option<Connection<M>>,
    /// Carried from a connection that went stale to the next one.
    frozen_tick: Option<i32>,
    clock: Box<dyn Fn() -> Instant>,
    raw_dump_dir: Option<PathBuf>,
    connections: u32,
}

impl<M: SharedMem> IracingSource<M> {
    pub fn new(connect: Box<dyn FnMut() -> Option<M>>) -> Self {
        Self {
            connect,
            conn: None,
            frozen_tick: None,
            clock: Box::new(Instant::now),
            raw_dump_dir: None,
            connections: 0,
        }
    }

    /// Debug: also dump the raw shared-memory layout and every session-info YAML into
    /// `dir`, so real data can become parser test fixtures.
    pub fn with_raw_dump(mut self, dir: PathBuf) -> Self {
        self.raw_dump_dir = Some(dir);
        self
    }

    /// Replace the wall clock used for staleness (tests).
    #[cfg(test)]
    fn with_clock(mut self, clock: Box<dyn Fn() -> Instant>) -> Self {
        self.clock = clock;
        self
    }
}

impl<M: SharedMem> SimSource for IracingSource<M> {
    fn sim(&self) -> Sim {
        Sim::Iracing
    }

    fn poll(&mut self) -> PollResult {
        let now = (self.clock)();
        if self.conn.is_none() {
            match (self.connect)() {
                Some(mem) => {
                    self.connections += 1;
                    let dump = self.raw_dump_dir.clone().map(|dir| RawDump {
                        dir,
                        connection: self.connections,
                        layout_written: false,
                    });
                    self.conn = Some(Connection::new(mem, dump, now, self.frozen_tick));
                }
                None => return PollResult::NotConnected,
            }
        }
        let conn = self.conn.as_mut().expect("connected");
        match conn.poll(now) {
            Ok(result) => result,
            Err(Disconnected) => {
                self.frozen_tick = conn.frozen_tick;
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
    use crate::sim::SessionInfo;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// Called after every read with (offset, image); lets a test mutate memory mid-poll.
    type ReadHook = Rc<RefCell<Option<Box<dyn FnMut(usize, &mut Vec<u8>)>>>>;

    struct FakeMem {
        img: Rc<RefCell<Vec<u8>>>,
        hook: ReadHook,
    }

    impl SharedMem for FakeMem {
        fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
            let out = self.img.borrow().get(offset..offset + len).map(|s| s.to_vec());
            if let Some(hook) = self.hook.borrow_mut().as_mut() {
                hook(offset, &mut self.img.borrow_mut());
            }
            out
        }
        fn wait_for_data(&self, _timeout_ms: u32) {}
    }

    fn get_i32(img: &[u8], off: usize) -> i32 {
        i32::from_le_bytes(img[off..off + 4].try_into().unwrap())
    }

    struct Rig {
        src: IracingSource<FakeMem>,
        b: ImageBuilder,
        img: Rc<RefCell<Vec<u8>>>,
        running: Rc<Cell<bool>>,
        hook: ReadHook,
        clock: Rc<Cell<Instant>>,
        /// SessionTime as of the last `tick()`, to tell an explicit `set` from time that
        /// simply hasn't been touched since.
        last_session_time: f64,
    }

    impl Rig {
        fn new() -> Rig {
            Rig::build(None)
        }

        fn build(raw_dump: Option<PathBuf>) -> Rig {
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
            let hook: ReadHook = Rc::new(RefCell::new(None));
            let clock = Rc::new(Cell::new(Instant::now()));
            let (i2, r2, h2, c2) = (img.clone(), running.clone(), hook.clone(), clock.clone());
            let src = IracingSource::new(Box::new(move || {
                r2.get().then(|| FakeMem { img: i2.clone(), hook: h2.clone() })
            }))
            .with_clock(Box::new(move || c2.get()));
            let src = match raw_dump {
                Some(dir) => src.with_raw_dump(dir),
                None => src,
            };
            Rig { src, b, img, running, hook, clock, last_session_time: 1234.5 }
        }

        /// Publish the builder's current state as a new tick, advancing SessionTime by a
        /// realistic step unless the test already set a specific value for this tick.
        fn tick(&mut self) {
            let current = self.b.get("SessionTime");
            if current == self.last_session_time {
                let advanced = current + 0.1;
                self.b.set("SessionTime", advanced);
                self.last_session_time = advanced;
            } else {
                self.last_session_time = current;
            }
            self.tick_stalled();
        }

        /// Publish a new tick without advancing SessionTime, as if the buffer rotated
        /// while the sim itself made no progress.
        fn tick_stalled(&mut self) {
            self.b.tick += 1;
            *self.img.borrow_mut() = self.b.build();
        }

        fn advance(&self, d: Duration) {
            self.clock.set(self.clock.get() + d);
        }

        /// Publish a new session-info YAML (bumping its update counter) with a new tick.
        fn publish_yaml(&mut self, yaml: String) {
            self.b.yaml = yaml;
            self.b.session_info_update += 1;
            self.tick();
        }

        /// The next copy of the session-info YAML races with the sim rewriting it.
        fn tear_next_yaml_read(&self) {
            let mut armed = true;
            *self.hook.borrow_mut() = Some(Box::new(move |offset, img| {
                if armed && offset == get_i32(img, 20) as usize {
                    armed = false;
                    let bumped = get_i32(img, 12) + 1;
                    img[12..16].copy_from_slice(&bumped.to_le_bytes());
                }
            }));
        }
    }

    fn expect_session(r: PollResult) -> SessionInfo {
        match r {
            PollResult::Session(info) => info,
            other => panic!("expected Session, got {other:?}"),
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
    fn stalled_session_time_is_paused_despite_new_tick() {
        let mut rig = Rig::new();
        rig.src.poll(); // Session
        expect_frame(rig.src.poll());
        // A new tick is published (e.g. a rotated buffer) but SessionTime hasn't moved.
        rig.tick_stalled();
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        // Session time going backwards is treated the same way.
        rig.b.set("SessionTime", 1234.4);
        rig.tick_stalled();
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        // Once it actually advances, frames resume.
        rig.b.set("SessionTime", 1234.6);
        rig.tick_stalled();
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.session_time_s, 1234.6);
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
    fn garbled_driver_info_with_same_session_num_is_not_a_new_session() {
        let mut rig = Rig::new();
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
        // A read that caught DriverInfo half-written: a different car id...
        rig.publish_yaml(SAMPLE_YAML.replace("CarID: 170", "CarID: 1"));
        expect_frame(rig.src.poll());
        // ...then the real data again on the next read.
        rig.publish_yaml(SAMPLE_YAML.to_string());
        expect_frame(rig.src.poll());
        // An empty car id is never a session, even if it repeats.
        rig.publish_yaml(SAMPLE_YAML.replace("CarID: 170", "CarID:"));
        expect_frame(rig.src.poll());
        rig.tick();
        expect_frame(rig.src.poll());
        rig.publish_yaml(SAMPLE_YAML.replace("CarID: 170", "CarID:"));
        expect_frame(rig.src.poll());
    }

    #[test]
    fn car_change_seen_on_two_reads_is_a_new_session() {
        let mut rig = Rig::new();
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
        let other_car = SAMPLE_YAML
            .replace("CarID: 170", "CarID: 171")
            .replace("CarScreenName: Porsche 963 GTP", "CarScreenName: BMW M Hybrid V8");
        rig.publish_yaml(other_car);
        expect_frame(rig.src.poll()); // first sighting: only a candidate
        rig.tick();
        let info = expect_session(rig.src.poll()); // confirmed by the next read
        assert_eq!(info.car_id, "171");
        assert_eq!(info.car_name, "BMW M Hybrid V8");
        expect_frame(rig.src.poll());
        rig.tick();
        expect_frame(rig.src.poll());
    }

    #[test]
    fn empty_track_or_car_id_never_starts_a_session() {
        let mut rig = Rig::new();
        rig.b.yaml = SAMPLE_YAML.replace("TrackID: 525", "TrackID:");
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
        rig.publish_yaml(SAMPLE_YAML.to_string());
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
    }

    #[test]
    fn yaml_rewritten_during_copy_is_paused_and_retried() {
        let mut rig = Rig::new();
        rig.tear_next_yaml_read();
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
        // Mid-session, racing a session change.
        rig.b.set("SessionNum", 1.0);
        rig.b.session_info_update += 1;
        rig.tick();
        rig.tear_next_yaml_read();
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        assert_eq!(expect_session(rig.src.poll()).session_type, "Race");
        expect_frame(rig.src.poll());
    }

    #[test]
    fn session_info_is_decoded_as_cp1252() {
        let mut rig = Rig::new();
        rig.b.yaml = SAMPLE_YAML.replace("Circuit de Spa-Francorchamps", "N\u{fc}rburgring");
        let mut img = rig.b.build();
        // ImageBuilder writes UTF-8; re-encode "ü" (C3 BC) as cp1252 (FC) like iRacing does.
        let yaml_end = get_i32(&img, 20) as usize + rig.b.yaml.len();
        let at = img.windows(2).position(|w| w == [0xC3, 0xBC]).unwrap();
        img[at] = 0xFC;
        img.copy_within(at + 2..yaml_end, at + 1);
        img[yaml_end - 1] = 0;
        *rig.img.borrow_mut() = img;
        assert_eq!(expect_session(rig.src.poll()).track_name, "N\u{fc}rburgring");
    }

    #[test]
    fn no_new_tick_for_10s_is_not_connected_until_the_sim_ticks_again() {
        let mut rig = Rig::new();
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
        rig.advance(Duration::from_secs(9));
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        rig.advance(Duration::from_secs(1));
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        // The map is still there but frozen: stay disconnected, don't re-announce.
        rig.advance(Duration::from_secs(1));
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.tick();
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
    }

    #[test]
    fn missing_session_time_var_yields_idle() {
        // A buffer that doesn't publish SessionTime at all (e.g. a layout we don't
        // recognize yet): there's nothing to build a Frame or announce a Session from.
        let mut b = ImageBuilder::new().var("Lap", VarType::Int).var("LapDistPct", VarType::Float);
        b.yaml = SAMPLE_YAML.to_string();
        b.set("Lap", 3.0).set("LapDistPct", 0.25);
        let img = Rc::new(RefCell::new(b.build()));
        let hook: ReadHook = Rc::new(RefCell::new(None));
        let (i2, h2) = (img.clone(), hook.clone());
        let mut src = IracingSource::new(Box::new(move || Some(FakeMem { img: i2.clone(), hook: h2.clone() })));
        let r = src.poll();
        assert!(matches!(r, PollResult::Idle), "{r:?}");
        // A later tick, still with no SessionTime var, stays Idle rather than a Frame.
        b.tick += 1;
        *img.borrow_mut() = b.build();
        let r = src.poll();
        assert!(matches!(r, PollResult::Idle), "{r:?}");
    }

    #[test]
    fn var_layout_change_reresolves_the_var_map() {
        let mut rig = Rig::new();
        expect_session(rig.src.poll());
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.get(Channel::SpeedMs), 55.5);

        // A different var layout (e.g. after a car swap): a new var is inserted ahead of
        // the others, so num_vars changes and every later var's data offset shifts too.
        let mut b2 = ImageBuilder::new()
            .var("SessionTime", VarType::Double)
            .var("ExtraVar", VarType::Float)
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
        b2.yaml = SAMPLE_YAML.to_string();
        b2.tick = rig.b.tick + 1;
        b2.session_info_update = rig.b.session_info_update;
        b2.set("SessionTime", 1234.6)
            .set("ExtraVar", 999.0)
            .set("Lap", 3.0)
            .set("LapLastLapTime", -1.0)
            .set("LapDistPct", 0.25)
            .set("Speed", 77.7)
            .set("Clutch", 0.25)
            .set("PlayerTrackSurface", 3.0)
            .set("IsOnTrack", 1.0);
        *rig.img.borrow_mut() = b2.build();

        let f2 = expect_frame(rig.src.poll());
        assert_eq!(f2.session_time_s, 1234.6);
        assert_eq!(f2.get(Channel::SpeedMs), 77.7, "must re-resolve offsets for the new layout");
    }

    #[test]
    fn raw_dump_writes_layout_once_and_each_session_info_update() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("captures").join("123");
        let mut rig = Rig::build(Some(dump.clone()));
        let first_img = rig.img.borrow().clone();
        expect_session(rig.src.poll());
        expect_frame(rig.src.poll());
        rig.tick();
        expect_frame(rig.src.poll()); // same YAML: nothing new to dump
        rig.publish_yaml(SAMPLE_YAML.replace("TrackAirTemp: 22.40 C", "TrackAirTemp: 23.00 C"));
        expect_frame(rig.src.poll());

        let layout_len = HEADER_LEN + 11 * VAR_HEADER_LEN;
        assert!(std::fs::read(dump.join("header-vars-1.bin")).unwrap() == first_img[..layout_len]);
        assert_eq!(std::fs::read(dump.join("session-1-1.yaml")).unwrap(), SAMPLE_YAML.as_bytes());
        let second = std::fs::read_to_string(dump.join("session-1-2.yaml")).unwrap();
        assert!(second.contains("TrackAirTemp: 23.00 C"));
        assert_eq!(std::fs::read_dir(&dump).unwrap().count(), 3);

        // A reconnect gets its own files instead of overwriting the first connection's.
        rig.running.set(false);
        rig.b.status = 0;
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.running.set(true);
        rig.b.status = 1;
        rig.tick();
        expect_session(rig.src.poll());
        assert!(dump.join("header-vars-2.bin").exists());
        assert!(dump.join("session-2-2.yaml").exists());
    }

    #[test]
    fn truncated_memory_is_not_connected_not_a_panic() {
        let mut rig = Rig::new();
        rig.img.borrow_mut().truncate(50);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
    }
}
