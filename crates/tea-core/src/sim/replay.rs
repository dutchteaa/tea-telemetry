//! Raw capture of adapter output, and a [`SimSource`] that plays it back.
//! Used for tests, fixtures and the "mock sim" dev mode.

use super::{PollResult, Sim, SimSource};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::Duration;

const CAPTURE_VERSION: u32 = 1;
const MAX_REALTIME_GAP_S: f64 = 0.5;
const NON_FRAME_DELAY: Duration = Duration::from_millis(16);

#[derive(Serialize, Deserialize)]
struct CaptureHeader {
    version: u32,
    sim: Sim,
}

pub struct CaptureWriter {
    enc: zstd::stream::write::AutoFinishEncoder<'static, BufWriter<File>>,
}

impl CaptureWriter {
    pub fn create(path: &Path, sim: Sim) -> anyhow::Result<Self> {
        let file = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
        let mut enc = zstd::stream::write::Encoder::new(file, 3)?.auto_finish();
        bincode::serialize_into(&mut enc, &CaptureHeader { version: CAPTURE_VERSION, sim })?;
        Ok(Self { enc })
    }

    pub fn write(&mut self, ev: &PollResult) -> anyhow::Result<()> {
        bincode::serialize_into(&mut self.enc, ev)?;
        Ok(())
    }
}

pub struct ReplaySource {
    sim: Sim,
    events: Box<dyn Iterator<Item = PollResult>>,
    realtime: bool,
    last_time_s: Option<f64>,
}

impl ReplaySource {
    pub fn open(path: &Path, realtime: bool) -> anyhow::Result<Self> {
        let file = BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
        let mut dec = zstd::stream::read::Decoder::with_buffer(file)?;
        let header: CaptureHeader =
            bincode::deserialize_from(&mut dec).context("not a Tea Telemetry capture file")?;
        if header.version > CAPTURE_VERSION {
            anyhow::bail!("capture was made by a newer version of Tea Telemetry");
        }
        // A truncated capture (app killed while capturing) simply ends early.
        let events = std::iter::from_fn(move || bincode::deserialize_from::<_, PollResult>(&mut dec).ok());
        Ok(Self { sim: header.sim, events: Box::new(events), realtime, last_time_s: None })
    }

    pub fn from_events(sim: Sim, events: Vec<PollResult>, realtime: bool) -> Self {
        Self { sim, events: Box::new(events.into_iter()), realtime, last_time_s: None }
    }

    fn pace(&mut self, ev: &PollResult) {
        if !self.realtime {
            return;
        }
        match ev {
            PollResult::Frame(f) => {
                if let Some(last) = self.last_time_s {
                    let gap = (f.session_time_s - last).clamp(0.0, MAX_REALTIME_GAP_S);
                    std::thread::sleep(Duration::from_secs_f64(gap));
                }
                self.last_time_s = Some(f.session_time_s);
            }
            _ => std::thread::sleep(NON_FRAME_DELAY),
        }
    }
}

impl SimSource for ReplaySource {
    fn sim(&self) -> Sim {
        self.sim
    }

    fn poll(&mut self) -> PollResult {
        match self.events.next() {
            Some(ev) => {
                self.pace(&ev);
                ev
            }
            None => PollResult::NotConnected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{drive, session};

    fn events() -> Vec<PollResult> {
        let mut ev = vec![PollResult::NotConnected, PollResult::Session(session())];
        ev.extend(drive(1, 0.5553, 1.0).into_iter().map(PollResult::Frame));
        ev.push(PollResult::Paused);
        ev.push(PollResult::Idle);
        ev
    }

    fn describe(ev: &PollResult) -> String {
        match ev {
            PollResult::Frame(f) => {
                let bits: Vec<u32> = f.values.iter().map(|v| v.to_bits()).collect();
                format!("F {} {} {:?} {:?}", f.session_time_s, f.lap, f.sim_last_lap_time_s, bits)
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn capture_round_trips_through_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.tcap");
        {
            let mut w = CaptureWriter::create(&path, Sim::Iracing).unwrap();
            for ev in events() {
                w.write(&ev).unwrap();
            }
        }
        let mut src = ReplaySource::open(&path, false).unwrap();
        assert_eq!(src.sim(), Sim::Iracing);
        for expected in events() {
            assert_eq!(describe(&src.poll()), describe(&expected));
        }
        assert!(matches!(src.poll(), PollResult::NotConnected));
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn from_events_yields_in_order_then_not_connected() {
        let mut src = ReplaySource::from_events(Sim::Lmu, vec![PollResult::Idle, PollResult::Paused], false);
        assert_eq!(src.sim(), Sim::Lmu);
        assert!(matches!(src.poll(), PollResult::Idle));
        assert!(matches!(src.poll(), PollResult::Paused));
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn open_rejects_non_capture_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.tcap");
        std::fs::write(&path, b"nope").unwrap();
        assert!(ReplaySource::open(&path, false).is_err());
    }
}
