//! Lap-boundary and sector timing from raw samples.

use crate::frame::{Channel, Frame};

pub fn to_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

fn pct(f: &Frame) -> f64 {
    f.get(Channel::LapDistPct) as f64
}

/// Session time at which the car crossed the start/finish line between consecutive
/// frames `a` (lap_dist_pct near 1.0) and `b` (near 0.0), linearly interpolated.
/// Falls back to `b`'s time when the pair doesn't look like a wrap.
pub fn line_crossing_time(a: &Frame, b: &Frame) -> f64 {
    let (pa, pb) = (pct(a), pct(b));
    if !(pa.is_finite() && pb.is_finite()) || pa < 0.5 || pb > 0.5 {
        return b.session_time_s;
    }
    let before = 1.0 - pa;
    let total = before + pb;
    if total <= 0.0 {
        return b.session_time_s;
    }
    a.session_time_s + (before / total) * (b.session_time_s - a.session_time_s)
}

/// Session time at which the car passed `target` between `a` and `b` (no wrap), if it did.
fn pct_crossing_time(a: &Frame, b: &Frame, target: f64) -> Option<f64> {
    let (pa, pb) = (pct(a), pct(b));
    if pa.is_finite() && pb.is_finite() && pb > pa && pa < target && target <= pb {
        Some(a.session_time_s + (target - pa) / (pb - pa) * (b.session_time_s - a.session_time_s))
    } else {
        None
    }
}

/// Sector durations in ms for one lap.
///
/// `samples` may include the boundary frames either side of the line; `lap_start_s` and
/// `lap_end_s` are the line-crossing times. Returns an empty Vec when sectors are unknown
/// or any split wasn't observed (e.g. after a reset).
pub fn sector_times_ms(
    samples: &[Frame],
    sector_start_pcts: &[f32],
    lap_start_s: f64,
    lap_end_s: f64,
) -> Vec<i64> {
    if sector_start_pcts.is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<f64> = sector_start_pcts
        .iter()
        .map(|p| *p as f64)
        .filter(|p| *p > 0.0)
        .collect();
    starts.sort_by(|a, b| a.total_cmp(b));

    let mut splits = vec![lap_start_s];
    for target in starts {
        match samples.windows(2).find_map(|w| pct_crossing_time(&w[0], &w[1], target)) {
            Some(t) => splits.push(t),
            None => return Vec::new(),
        }
    }
    splits.push(lap_end_s);
    splits.windows(2).map(|w| to_ms(w[1] - w[0])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Channel, Frame};

    fn f(t: f64, pct: f32) -> Frame {
        let mut fr = Frame::new(t, 1);
        fr.set(Channel::LapDistPct, pct);
        fr
    }

    #[test]
    fn crossing_is_interpolated_between_samples() {
        // 0.0007 of a lap to go before the line, 0.0003 after it: crossing is 70% of the way.
        let t = line_crossing_time(&f(10.0, 0.9993), &f(10.1, 0.0003));
        assert!((t - 10.07).abs() < 1e-6, "got {t}");
    }

    #[test]
    fn crossing_falls_back_to_later_sample_when_not_a_wrap() {
        assert_eq!(line_crossing_time(&f(10.0, 0.4), &f(10.1, 0.6)), 10.1);
        assert_eq!(line_crossing_time(&f(10.0, f32::NAN), &f(10.1, 0.01)), 10.1);
    }

    #[test]
    fn sector_times_from_interpolated_splits() {
        // A 100 s lap at constant speed sampled at 10 Hz, from the line to the line.
        let mut samples = vec![f(-0.05, 0.9995)];
        for i in 0..1000 {
            let t = i as f64 * 0.1 + 0.05;
            samples.push(f(t, (t / 100.0) as f32));
        }
        samples.push(f(100.05, 0.0005));
        let sectors = sector_times_ms(&samples, &[0.0, 0.3, 0.7], 0.0, 100.0);
        assert_eq!(sectors.len(), 3);
        assert!((sectors[0] - 30_000).abs() <= 1, "{sectors:?}");
        assert!((sectors[1] - 40_000).abs() <= 1, "{sectors:?}");
        assert!((sectors[2] - 30_000).abs() <= 1, "{sectors:?}");
    }

    #[test]
    fn sector_times_empty_when_unknown_or_missing() {
        let samples = vec![f(0.0, 0.1), f(1.0, 0.2)];
        assert!(sector_times_ms(&samples, &[], 0.0, 1.0).is_empty());
        // The 0.5 split is never reached in these samples.
        assert!(sector_times_ms(&samples, &[0.0, 0.5], 0.0, 1.0).is_empty());
    }

    #[test]
    fn to_ms_rounds() {
        assert_eq!(to_ms(100.0004), 100_000);
        assert_eq!(to_ms(100.0006), 100_001);
    }
}
