//! The background recorder: polls a sim, splits laps, saves them. Survives panics.

use crate::recorder::Recorder;
use crate::sim::replay::CaptureWriter;
use crate::sim::{PollResult, Sim, SimSource};
use crate::store::Store;
use serde::Serialize;
use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RESTART_DELAY: Duration = Duration::from_secs(1);

pub type SourceFactory = Box<dyn Fn() -> Box<dyn SimSource> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecState {
    #[default]
    NoSim,
    Idle,
    Recording,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
pub struct RecorderStatus {
    pub state: RecState,
    pub sim: Option<Sim>,
    pub lap: Option<i32>,
    pub laps_saved: u32,
    pub last_error: Option<String>,
}

pub struct ServiceConfig {
    pub data_root: PathBuf,
    /// Debug: dump every PollResult to `data_root/captures/` for building fixtures.
    pub capture_raw: bool,
    /// How long to wait before re-checking for the sim when it isn't running.
    pub not_connected_backoff: Duration,
}

pub struct RecorderService {
    status: Arc<Mutex<RecorderStatus>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

fn lock(status: &Mutex<RecorderStatus>) -> MutexGuard<'_, RecorderStatus> {
    status.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl RecorderService {
    pub fn start(factory: SourceFactory, config: ServiceConfig) -> anyhow::Result<Self> {
        let mut store = Store::open(&config.data_root)?;
        let report = store.reconcile()?;
        log::info!("store reconciled: {report:?}");
        let status = Arc::new(Mutex::new(RecorderStatus::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_status, thread_stop) = (status.clone(), stop.clone());
        let handle = thread::Builder::new()
            .name("recorder".into())
            .spawn(move || supervise(factory, config, store, thread_status, thread_stop))?;
        Ok(Self { status, stop, handle: Some(handle) })
    }

    pub fn status(&self) -> RecorderStatus {
        lock(&self.status).clone()
    }
}

impl Drop for RecorderService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let until = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10).min(d));
    }
}

fn supervise(
    factory: SourceFactory,
    config: ServiceConfig,
    mut store: Store,
    status: Arc<Mutex<RecorderStatus>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let result = panic::catch_unwind(AssertUnwindSafe(|| run(&factory, &config, &mut store, &status, &stop)));
        let error = match result {
            Ok(Ok(())) => break, // stop requested
            Ok(Err(e)) => format!("{e:#}"),
            Err(p) => format!("recorder crashed: {}", panic_message(p.as_ref())),
        };
        log::error!("{error}; restarting recorder");
        {
            let mut s = lock(&status);
            s.last_error = Some(error);
            s.state = RecState::NoSim;
            s.sim = None;
            s.lap = None;
        }
        sleep_unless_stopped(RESTART_DELAY, &stop);
    }
}

fn capture_path(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = root.join("captures");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("capture-{}.tcap", now_ms())))
}

fn run(
    factory: &SourceFactory,
    config: &ServiceConfig,
    store: &mut Store,
    status: &Mutex<RecorderStatus>,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let mut source = factory();
    let mut recorder = Recorder::new();
    let mut capture = if config.capture_raw {
        Some(CaptureWriter::create(&capture_path(&config.data_root)?, source.sim())?)
    } else {
        None
    };
    while !stop.load(Ordering::SeqCst) {
        let event = source.poll();
        if let Some(c) = capture.as_mut() {
            c.write(&event)?;
        }
        let not_connected = matches!(event, PollResult::NotConnected);
        for lap in recorder.handle(event, now_ms()) {
            match store.save_lap(&lap, now_ms()) {
                Ok(()) => {
                    log::info!("saved lap {} ({} ms)", lap.lap_number, lap.lap_time_ms);
                    lock(status).laps_saved += 1;
                }
                Err(e) => {
                    log::error!("failed to save lap {}: {e:#}", lap.lap_number);
                    lock(status).last_error = Some(format!("failed to save lap: {e:#}"));
                }
            }
        }
        {
            let mut s = lock(status);
            s.sim = (!not_connected).then(|| source.sim());
            s.state = if not_connected {
                RecState::NoSim
            } else if recorder.is_recording() {
                RecState::Recording
            } else {
                RecState::Idle
            };
            s.lap = recorder.current_lap();
        }
        if not_connected {
            sleep_unless_stopped(config.not_connected_backoff, stop);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::replay::ReplaySource;
    use crate::testutil::{drive, session};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    fn two_lap_events() -> Vec<PollResult> {
        let mut ev = vec![PollResult::NotConnected, PollResult::Session(session())];
        ev.extend(drive(1, 0.5553, 250.0).into_iter().map(PollResult::Frame));
        ev
    }

    fn config(dir: &std::path::Path) -> ServiceConfig {
        ServiceConfig {
            data_root: dir.to_path_buf(),
            capture_raw: false,
            not_connected_backoff: Duration::from_millis(10),
        }
    }

    fn wait_for(svc: &RecorderService, pred: impl Fn(&RecorderStatus) -> bool) -> RecorderStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = svc.status();
            if pred(&s) || Instant::now() > deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn records_laps_from_replay() {
        let dir = tempfile::tempdir().unwrap();
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
        });
        let svc = RecorderService::start(factory, config(dir.path())).unwrap();
        // The replay ends in NotConnected, which flushes the pending lap 3.
        let status = wait_for(&svc, |s| s.laps_saved >= 2);
        assert_eq!(status.laps_saved, 2);
        let status = wait_for(&svc, |s| s.state == RecState::NoSim);
        assert_eq!(status.state, RecState::NoSim);
        drop(svc);

        let store = Store::open(dir.path()).unwrap();
        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 2);
        assert!(laps.iter().all(|l| l.lap_time_ms == 100_000));
    }

    #[test]
    fn recorder_restarts_after_panic() {
        struct Boom;
        impl SimSource for Boom {
            fn sim(&self) -> Sim {
                Sim::Iracing
            }
            fn poll(&mut self) -> PollResult {
                panic!("boom: malformed shared memory")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Box::new(Boom)
            } else {
                Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
            }
        });
        let svc = RecorderService::start(factory, config(dir.path())).unwrap();
        let status = wait_for(&svc, |s| s.laps_saved >= 2);
        assert_eq!(status.laps_saved, 2, "recorder must recover and keep recording");
        assert!(status.last_error.unwrap_or_default().contains("boom"));
    }

    #[test]
    fn capture_raw_writes_a_replayable_file() {
        let dir = tempfile::tempdir().unwrap();
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
        });
        let mut cfg = config(dir.path());
        cfg.capture_raw = true;
        let svc = RecorderService::start(factory, cfg).unwrap();
        wait_for(&svc, |s| s.laps_saved >= 2);
        drop(svc); // finishes the capture file

        let capture = std::fs::read_dir(dir.path().join("captures"))
            .unwrap()
            .next()
            .expect("a capture file")
            .unwrap()
            .path();
        let mut replay = ReplaySource::open(&capture, false).unwrap();
        assert!(matches!(replay.poll(), PollResult::NotConnected));
        assert!(matches!(replay.poll(), PollResult::Session(_)));
        assert!(matches!(replay.poll(), PollResult::Frame(_)));
    }
}
