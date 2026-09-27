//! Writes a synthetic 5-lap iRacing-like capture for UI work without a sim:
//!   cargo run -p tea-core --example make_mock_capture -- mock.tcap
//! then run the app with TEA_MOCK=mock.tcap.

use std::collections::BTreeMap;
use tea_core::frame::{Channel, Frame};
use tea_core::sim::replay::CaptureWriter;
use tea_core::sim::{PollResult, SessionInfo, Sim};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "mock.tcap".into());
    let mut w = CaptureWriter::create(std::path::Path::new(&path), Sim::Iracing)?;
    w.write(&PollResult::Session(SessionInfo {
        sim: Sim::Iracing,
        track_id: "0".into(),
        track_name: "Mock Oval".into(),
        track_config: "".into(),
        track_length_m: 4000.0,
        car_id: "0".into(),
        car_name: "Mock Car".into(),
        session_type: "Practice".into(),
        sector_start_pcts: vec![0.0, 0.33, 0.66],
        conditions: BTreeMap::new(),
    }))?;
    let dt: f64 = 1.0 / 60.0;
    let mut dist: f64 = 0.8; // start mid-lap, like leaving the pits
    let mut t: f64 = 0.0;
    while dist < 5.99 {
        // Each lap is a little different so the laps aren't identical.
        let lap_time = 60.0 + (dist.floor() * 0.37) % 1.2;
        let pct: f64 = dist.fract();
        let slow = (pct * std::f64::consts::TAU * 2.0).cos().max(0.0);
        let mut f = Frame::new(t, 1 + dist.floor() as i32);
        f.set(Channel::LapDistPct, pct as f32);
        f.set(Channel::LapDistM, (pct * 4000.0) as f32);
        f.set(Channel::SpeedMs, (75.0 - 30.0 * slow) as f32);
        f.set(Channel::Throttle, (1.0 - slow) as f32);
        f.set(Channel::Brake, if slow > 0.7 { 0.8 } else { 0.0 });
        f.set(Channel::Gear, if slow > 0.5 { 3.0 } else { 5.0 });
        f.set(Channel::FuelL, (40.0 - t * 0.03) as f32);
        f.set(Channel::OnPitRoad, 0.0);
        f.set(Channel::OffTrack, 0.0);
        w.write(&PollResult::Frame(f))?;
        dist += dt / lap_time;
        t += dt;
    }
    w.write(&PollResult::Idle)?;
    println!("wrote {path}");
    Ok(())
}
