//! Turns a stream of [`PollResult`]s into completed laps.

use crate::frame::{Channel, Frame};
use crate::laptime::{lap_end_time, lap_start_time, sector_times_ms, to_ms};
use crate::sim::{PollResult, SessionInfo};

/// Frames to wait after a lap closes for the sim to publish its official lap time.
pub const SIM_LAP_TIME_WAIT_FRAMES: u32 = 90;
/// If the sim's lap time disagrees with ours by more than this, it's stale; use ours.
const SIM_LAP_TIME_TOLERANCE_S: f64 = 1.0;
/// A jump in lap_dist_pct bigger than this in one frame (without a line crossing) is a reset.
const TELEPORT_PCT: f32 = 0.05;
/// A lap in progress this long (60 Hz for 30 minutes) never crossed the line again and is
/// abandoned, e.g. crossed into pit lane then sat in the stall.
pub const MAX_LAP_SAMPLES: usize = 60 * 30 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidReason {
    Reset,
    OutLap,
    InLap,
    OffTrack,
}

impl InvalidReason {
    pub fn as_str(self) -> &'static str {
        match self {
            InvalidReason::Reset => "reset",
            InvalidReason::OutLap => "out_lap",
            InvalidReason::InLap => "in_lap",
            InvalidReason::OffTrack => "off_track",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletedLap {
    pub lap_id: String,
    pub session_id: String,
    pub session_started_at_ms: i64,
    pub session: SessionInfo,
    pub lap_number: i32,
    pub lap_time_ms: i64,
    /// Session time of the line crossing that began this lap.
    pub lap_start_s: f64,
    pub invalid_reason: Option<InvalidReason>,
    pub fuel_start_l: Option<f32>,
    pub fuel_used_l: Option<f32>,
    pub sector_times_ms: Vec<i64>,
    /// [last frame before the start line, the lap's frames..., first frame after the finish line]
    pub samples: Vec<Frame>,
}

impl CompletedLap {
    pub fn valid(&self) -> bool {
        self.invalid_reason.is_none()
    }
}

struct ActiveSession {
    id: String,
    started_at_ms: i64,
    info: SessionInfo,
}

struct LapInProgress {
    lap_number: i32,
    samples: Vec<Frame>,
    reset: bool,
}

impl LapInProgress {
    fn starting(lap_number: i32, before_line: Frame, after_line: Frame) -> Self {
        Self { lap_number, samples: vec![before_line, after_line], reset: false }
    }
}

/// A closed lap waiting (briefly) for the sim's official time.
struct PendingLap {
    lap: CompletedLap,
    computed_s: f64,
    sim_time_before: Option<f32>,
    frames_waited: u32,
}

pub struct Recorder {
    session: Option<ActiveSession>,
    lap: Option<LapInProgress>,
    prev: Option<Frame>,
    pending: Option<PendingLap>,
    max_lap_samples: usize,
}

impl Default for Recorder {
    fn default() -> Self {
        Self { session: None, lap: None, prev: None, pending: None, max_lap_samples: MAX_LAP_SAMPLES }
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A `Recorder` with a lower lap-sample cap, so tests can exercise the cap without
    /// generating tens of thousands of frames.
    #[cfg(test)]
    fn with_max_lap_samples(max_lap_samples: usize) -> Self {
        Self { max_lap_samples, ..Self::default() }
    }

    pub fn current_lap(&self) -> Option<i32> {
        self.lap.as_ref().map(|l| l.lap_number)
    }

    pub fn is_recording(&self) -> bool {
        self.lap.is_some()
    }

    pub fn handle(&mut self, event: PollResult, now_ms: i64) -> Vec<CompletedLap> {
        let mut out = Vec::new();
        match event {
            PollResult::Paused => {}
            PollResult::Idle => self.abandon_lap(&mut out),
            PollResult::NotConnected => {
                self.abandon_lap(&mut out);
                self.session = None;
            }
            PollResult::Session(info) => {
                self.abandon_lap(&mut out);
                self.session = Some(ActiveSession {
                    id: uuid::Uuid::new_v4().to_string(),
                    started_at_ms: now_ms,
                    info,
                });
            }
            PollResult::Frame(f) => self.on_frame(f, &mut out),
        }
        out
    }

    fn abandon_lap(&mut self, out: &mut Vec<CompletedLap>) {
        self.flush_pending(out);
        self.lap = None;
        self.prev = None;
    }

    fn on_frame(&mut self, f: Frame, out: &mut Vec<CompletedLap>) {
        if self.session.is_none() {
            return;
        }
        if let Some(p) = self.pending.as_mut() {
            p.frames_waited += 1;
        }
        self.try_resolve_pending(&f, out);

        let prev = self.prev.take();
        let lap_number = self.lap.as_ref().map(|l| l.lap_number);
        match (lap_number, prev) {
            (Some(n), Some(prev)) if f.lap == n + 1 => {
                let mut finished = self.lap.take().expect("lap in progress");
                finished.samples.push(f.clone());
                self.close_lap(finished, out);
                self.try_resolve_pending(&f, out);
                self.lap = Some(LapInProgress::starting(f.lap, prev, f.clone()));
            }
            // Lap counter jumped or went backwards: the session was reset. Drop the lap.
            (Some(n), _) if f.lap != n => self.lap = None,
            (Some(_), prev) => {
                let lap = self.lap.as_mut().expect("lap in progress");
                if let Some(prev) = &prev {
                    // Distance around the lap circle, so a wrap a frame before or after
                    // the Lap counter changes isn't mistaken for a teleport.
                    let d = (f.get(Channel::LapDistPct) - prev.get(Channel::LapDistPct)).abs();
                    let jump = d.min(1.0 - d);
                    if jump > TELEPORT_PCT {
                        lap.reset = true;
                    }
                }
                lap.samples.push(f.clone());
                if lap.samples.len() > self.max_lap_samples {
                    log::debug!(
                        "lap {}: {} samples exceeds the cap of {}; abandoning (stuck without crossing the line?)",
                        lap.lap_number,
                        lap.samples.len(),
                        self.max_lap_samples
                    );
                    self.lap = None;
                }
            }
            // First line crossing we've seen: a lap starts here.
            (None, Some(prev)) if f.lap == prev.lap + 1 => {
                self.lap = Some(LapInProgress::starting(f.lap, prev, f.clone()));
            }
            (None, _) => {}
        }
        self.prev = Some(f);
    }

    fn close_lap(&mut self, lap: LapInProgress, out: &mut Vec<CompletedLap>) {
        // Only one lap can wait for the sim's time; an older one gets our computed time.
        self.flush_pending(out);
        let Some(session) = self.session.as_ref() else { return };
        let s = &lap.samples;
        let n = s.len();
        if n < 3 {
            return;
        }
        let start = lap_start_time(s);
        let end = lap_end_time(s);
        let computed = end - start;
        if !(computed > 0.0) {
            return;
        }
        let in_lap = &s[1..n - 1];
        let invalid_reason = if lap.reset {
            Some(InvalidReason::Reset)
        } else if in_lap[0].flag(Channel::OnPitRoad) {
            Some(InvalidReason::OutLap)
        } else if in_lap.iter().any(|f| f.flag(Channel::OnPitRoad)) {
            Some(InvalidReason::InLap)
        } else if in_lap.iter().any(|f| f.flag(Channel::OffTrack)) {
            Some(InvalidReason::OffTrack)
        } else {
            None
        };
        let finite = |v: f32| v.is_finite().then_some(v);
        let fuel_start_l = finite(in_lap[0].get(Channel::FuelL));
        let fuel_end = finite(in_lap[in_lap.len() - 1].get(Channel::FuelL));
        let fuel_used_l = match (fuel_start_l, fuel_end) {
            (Some(a), Some(b)) if a >= b => Some(a - b),
            _ => None, // unknown, or refuelled during the lap
        };
        let sector_times_ms = sector_times_ms(s, &session.info.sector_start_pcts, start, end);
        let sim_time_before = s[n - 2].sim_last_lap_time_s;
        let completed = CompletedLap {
            lap_id: uuid::Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            session_started_at_ms: session.started_at_ms,
            session: session.info.clone(),
            lap_number: lap.lap_number,
            lap_time_ms: to_ms(computed),
            lap_start_s: start,
            invalid_reason,
            fuel_start_l,
            fuel_used_l,
            sector_times_ms,
            samples: lap.samples,
        };
        self.pending = Some(PendingLap { lap: completed, computed_s: computed, sim_time_before, frames_waited: 0 });
    }

    fn try_resolve_pending(&mut self, f: &Frame, out: &mut Vec<CompletedLap>) {
        let Some(p) = self.pending.as_mut() else { return };
        let sim_time = f.sim_last_lap_time_s.filter(|t| {
            *t > 0.0
                && p.sim_time_before != Some(*t)
                && ((*t as f64) - p.computed_s).abs() <= SIM_LAP_TIME_TOLERANCE_S
        });
        let timed_out = p.frames_waited >= SIM_LAP_TIME_WAIT_FRAMES;
        if let Some(t) = sim_time {
            let mut p = self.pending.take().expect("pending lap");
            let new_time_ms = to_ms(t as f64);
            let diff = new_time_ms - p.lap.lap_time_ms;
            p.lap.lap_time_ms = new_time_ms;
            if let Some(last) = p.lap.sector_times_ms.last_mut() {
                *last += diff;
            }
            out.push(p.lap);
        } else if timed_out {
            self.flush_pending(out);
        }
    }

    fn flush_pending(&mut self, out: &mut Vec<CompletedLap>) {
        if let Some(p) = self.pending.take() {
            out.push(p.lap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{drive, session, T0};

    fn run(rec: &mut Recorder, events: Vec<PollResult>) -> Vec<CompletedLap> {
        events.into_iter().flat_map(|e| rec.handle(e, 42)).collect()
    }

    fn with_session(frames: Vec<Frame>, tail: PollResult) -> Vec<PollResult> {
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(frames.into_iter().map(PollResult::Frame));
        ev.push(tail);
        ev
    }

    #[test]
    fn records_full_laps_and_discards_partial_first_lap() {
        // Joins lap 1 at 55.53% (never saw its start) and crosses the line at
        // t=1044.47, 1144.47 and 1244.47; goes idle at t=1250 in the middle of lap 4.
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 250.0), PollResult::Idle));
        let numbers: Vec<i32> = laps.iter().map(|l| l.lap_number).collect();
        assert_eq!(numbers, vec![2, 3]);
        for lap in &laps {
            assert_eq!(lap.lap_time_ms, 100_000, "lap {}", lap.lap_number);
            assert!(lap.valid());
            assert_eq!(lap.session_started_at_ms, 42);
            assert_eq!(lap.session.track_name, "Circuit de Spa-Francorchamps");
        }
        assert!((laps[0].lap_start_s - (T0 + 44.47)).abs() < 1e-3);
        assert_ne!(laps[0].lap_id, laps[1].lap_id);
        assert_eq!(laps[0].session_id, laps[1].session_id);
    }

    #[test]
    fn samples_include_boundary_frames() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 160.0), PollResult::Idle));
        let s = &laps[0].samples;
        assert!(s.first().unwrap().get(Channel::LapDistPct) > 0.99);
        assert!(s.last().unwrap().get(Channel::LapDistPct) < 0.01);
        assert_eq!(s.len(), 1002); // 1000 in-lap frames + 2 boundary frames
    }

    #[test]
    fn computes_fuel_and_sectors() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 160.0), PollResult::Idle));
        let lap = &laps[0];
        assert_eq!(lap.sector_times_ms, vec![30_000, 40_000, 30_000]);
        assert!((lap.fuel_used_l.unwrap() - 2.0).abs() < 0.01, "{:?}", lap.fuel_used_l);
        assert!(lap.fuel_start_l.unwrap() > 59.0);
    }

    #[test]
    fn uses_sim_lap_time_when_published() {
        let mut frames = drive(1, 0.5553, 160.0);
        for f in &mut frames {
            // The old value until 0.5 s after the crossing at t=1144.47, then the new one.
            f.sim_last_lap_time_s = Some(if f.session_time_s >= T0 + 145.0 { 100.2 } else { 99.0 });
        }
        let laps = run(&mut Recorder::new(), with_session(frames, PollResult::Idle));
        assert_eq!(laps[0].lap_time_ms, 100_200);
        let sectors = &laps[0].sector_times_ms;
        assert_eq!(sectors, &vec![30_000, 40_000, 30_200], "last sector absorbs the diff");
        assert_eq!(sectors.iter().sum::<i64>(), laps[0].lap_time_ms);
    }

    #[test]
    fn ignores_sim_lap_time_far_from_computed() {
        let mut frames = drive(1, 0.5553, 160.0);
        for f in &mut frames {
            f.sim_last_lap_time_s = Some(if f.session_time_s >= T0 + 145.0 { 150.0 } else { 99.0 });
        }
        let laps = run(&mut Recorder::new(), with_session(frames, PollResult::Idle));
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn waits_at_most_90_frames_for_sim_time() {
        // No Idle at the end: lap 2 must still be emitted once the wait expires.
        let frames = drive(1, 0.5553, 144.47 + 0.1 * (SIM_LAP_TIME_WAIT_FRAMES as f64 + 2.0));
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(frames.into_iter().map(PollResult::Frame));
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_number, 2);
    }

    fn lap2_with(edit: impl Fn(&mut Frame)) -> CompletedLap {
        let mut frames = drive(1, 0.5553, 160.0);
        frames.iter_mut().for_each(|f| edit(f));
        run(&mut Recorder::new(), with_session(frames, PollResult::Idle)).remove(0)
    }

    fn at(f: &Frame, from: f64, to: f64) -> bool {
        f.session_time_s >= T0 + from && f.session_time_s <= T0 + to
    }

    #[test]
    fn flags_out_lap() {
        let lap = lap2_with(|f| if at(f, 44.4, 50.0) { f.set(Channel::OnPitRoad, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::OutLap));
        assert!(!lap.valid());
    }

    #[test]
    fn flags_in_lap() {
        let lap = lap2_with(|f| if at(f, 140.0, 144.4) { f.set(Channel::OnPitRoad, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::InLap));
    }

    #[test]
    fn flags_off_track() {
        let lap = lap2_with(|f| if at(f, 79.95, 80.05) { f.set(Channel::OffTrack, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::OffTrack));
    }

    #[test]
    fn flags_reset_on_teleport() {
        let lap = lap2_with(|f| {
            if at(f, 79.95, 80.05) {
                let p = f.get(Channel::LapDistPct);
                f.set(Channel::LapDistPct, p + 0.2);
            }
        });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::Reset));
    }

    #[test]
    fn pct_wrap_a_frame_off_the_lap_increment_is_not_a_reset() {
        // The line is crossed at t=1144.47: the frame at 1144.4 has pct 0.9947 and the
        // frame at 1144.5 has pct 0.0047. The sim may bump Lap one frame late or early.
        let run_with = |edit: &dyn Fn(&mut Frame)| {
            let mut frames = drive(1, 0.5553, 250.0);
            frames.iter_mut().for_each(|f| edit(f));
            run(&mut Recorder::new(), with_session(frames, PollResult::Idle))
        };
        // Late: pct wraps while Lap still says 2.
        let laps = run_with(&|f| if at(f, 144.45, 144.55) { f.lap = 2 });
        let lap2 = laps.iter().find(|l| l.lap_number == 2).expect("lap 2");
        assert_eq!(lap2.invalid_reason, None);
        assert_eq!(lap2.lap_time_ms, 100_000);
        // Early: Lap says 3 while pct is still 0.9947.
        let laps = run_with(&|f| if at(f, 144.35, 144.45) { f.lap = 3 });
        let lap3 = laps.iter().find(|l| l.lap_number == 3).expect("lap 3");
        assert_eq!(lap3.invalid_reason, None);
        assert_eq!(lap3.lap_time_ms, 100_000);
    }

    #[test]
    fn idle_mid_lap_discards_it() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 100.0), PollResult::Idle));
        assert!(laps.is_empty());
    }

    #[test]
    fn paused_does_not_split_or_drop_the_lap() {
        let mut ev = vec![PollResult::Session(session())];
        for (i, f) in drive(1, 0.5553, 160.0).into_iter().enumerate() {
            if i % 10 == 0 {
                ev.push(PollResult::Paused);
            }
            ev.push(PollResult::Frame(f));
        }
        ev.push(PollResult::Idle);
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn session_change_discards_lap_in_progress() {
        let frames = drive(1, 0.5553, 250.0);
        let (first, second) = frames.split_at(1000); // split at t=1100, mid lap 2
        let mut race = session();
        race.session_type = "Race".into();
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(first.iter().cloned().map(PollResult::Frame));
        ev.push(PollResult::Session(race));
        ev.extend(second.iter().cloned().map(PollResult::Frame));
        ev.push(PollResult::Idle);
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_number, 3);
        assert_eq!(laps[0].session.session_type, "Race");
    }

    #[test]
    fn frames_without_a_session_are_ignored() {
        let ev: Vec<PollResult> = drive(1, 0.5553, 160.0).into_iter().map(PollResult::Frame).collect();
        assert!(run(&mut Recorder::new(), ev).is_empty());
    }

    #[test]
    fn caps_lap_growth_when_a_lap_never_crosses_the_line() {
        // e.g. crossed the line into pit lane, then sat in the stall for a long time.
        let mut rec = Recorder::with_max_lap_samples(5);
        let laps = run(&mut rec, with_session(drive(1, 0.5553, 160.0), PollResult::Idle));
        assert!(laps.is_empty(), "every lap exceeds the 5-sample cap before it can close");
        assert!(!rec.is_recording());
    }

    #[test]
    fn reports_current_lap() {
        let mut rec = Recorder::new();
        run(&mut rec, with_session(drive(1, 0.5553, 60.0), PollResult::Paused));
        assert!(rec.is_recording());
        assert_eq!(rec.current_lap(), Some(2));
        rec.handle(PollResult::Idle, 0);
        assert!(!rec.is_recording());
    }
}
