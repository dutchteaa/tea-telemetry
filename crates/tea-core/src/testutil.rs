//! Synthetic sessions and laps shared by unit tests.

use crate::frame::{Channel, Frame};
use crate::recorder::{CompletedLap, Recorder};
use crate::sim::{PollResult, SessionInfo, Sim};
use std::collections::BTreeMap;

pub const LAP_TIME_S: f64 = 100.0;
pub const DT: f64 = 0.1;
pub const T0: f64 = 1000.0;
const TRACK_M: f64 = 7004.0;

pub fn session() -> SessionInfo {
    SessionInfo {
        sim: Sim::Iracing,
        track_id: "525".into(),
        track_name: "Circuit de Spa-Francorchamps".into(),
        track_config: "Grand Prix".into(),
        track_length_m: TRACK_M as f32,
        car_id: "170".into(),
        car_name: "Porsche 963 GTP".into(),
        session_type: "Practice".into(),
        sector_start_pcts: vec![0.0, 0.3, 0.7],
        conditions: BTreeMap::from([("air_temp".to_string(), "22.40 C".to_string())]),
    }
}

/// Frames at 10 Hz for a car lapping in exactly 100 s at constant speed, starting at
/// `start_pct` of lap `start_lap` at session time 1000 s, for `duration_s` seconds.
/// Fuel drops 0.02 L/s. With `start_pct = 0.5553` no sample lands exactly on the line.
pub fn drive(start_lap: i32, start_pct: f64, duration_s: f64) -> Vec<Frame> {
    let n = (duration_s / DT).round() as usize;
    (0..=n)
        .map(|i| {
            let t = i as f64 * DT;
            let dist = start_pct + t / LAP_TIME_S;
            let mut f = Frame::new(T0 + t, start_lap + dist.floor() as i32);
            f.set(Channel::LapDistPct, dist.fract() as f32);
            f.set(Channel::LapDistM, (dist.fract() * TRACK_M) as f32);
            f.set(Channel::SpeedMs, 70.0);
            f.set(Channel::FuelL, (60.0 - t * 0.02) as f32);
            f.set(Channel::OnPitRoad, 0.0);
            f.set(Channel::OffTrack, 0.0);
            f
        })
        .collect()
}

/// One clean 100 s lap (lap 2 of a Spa practice session) produced by the real recorder.
///
/// Not used by this crate's own tests yet; reserved for later tasks (store, recorder
/// service) that reuse this fixture.
#[allow(dead_code)]
pub fn completed_lap() -> CompletedLap {
    let mut rec = Recorder::new();
    let mut events = vec![PollResult::Session(session())];
    events.extend(drive(1, 0.5553, 160.0).into_iter().map(PollResult::Frame));
    events.push(PollResult::Idle);
    let mut laps: Vec<CompletedLap> = events
        .into_iter()
        .flat_map(|e| rec.handle(e, 1_700_000_000_000))
        .collect();
    assert_eq!(laps.len(), 1, "fixture should yield exactly one lap");
    laps.remove(0)
}
