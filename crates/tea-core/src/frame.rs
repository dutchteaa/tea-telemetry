//! The channel registry and the normalized per-sample [`Frame`] every sim adapter produces.

use serde::{Deserialize, Serialize};

macro_rules! channels {
    ($($variant:ident => ($name:literal, $unit:literal)),+ $(,)?) => {
        /// Every telemetry channel the app knows about. Sims fill what they have;
        /// anything a sim doesn't provide stays NaN in a [`Frame`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Channel { $($variant),+ }

        impl Channel {
            pub const ALL: &'static [Channel] = &[$(Channel::$variant),+];
            pub const COUNT: usize = Channel::ALL.len();

            pub fn name(self) -> &'static str {
                match self { $(Channel::$variant => $name),+ }
            }

            pub fn unit(self) -> &'static str {
                match self { $(Channel::$variant => $unit),+ }
            }

            pub fn from_name(name: &str) -> Option<Channel> {
                match name { $($name => Some(Channel::$variant),)+ _ => None }
            }

            pub fn index(self) -> usize {
                self as usize
            }
        }
    };
}

channels! {
    LapDistM => ("lap_dist_m", "m"),
    LapDistPct => ("lap_dist_pct", "frac"),
    SpeedMs => ("speed", "m/s"),
    Throttle => ("throttle", "frac"),
    Brake => ("brake", "frac"),
    Clutch => ("clutch", "frac"),
    SteeringRad => ("steering", "rad"),
    Gear => ("gear", ""),
    Rpm => ("rpm", "rpm"),
    PosX => ("pos_x", "m"),
    PosZ => ("pos_z", "m"),
    YawRad => ("yaw", "rad"),
    VelLongMs => ("vel_long", "m/s"),
    VelLatMs => ("vel_lat", "m/s"),
    OnPitRoad => ("on_pit_road", "bool"),
    OffTrack => ("off_track", "bool"),
    FuelL => ("fuel", "l"),
    TyreTempLfL => ("tyre_temp_lf_l", "C"),
    TyreTempLfM => ("tyre_temp_lf_m", "C"),
    TyreTempLfR => ("tyre_temp_lf_r", "C"),
    TyreTempRfL => ("tyre_temp_rf_l", "C"),
    TyreTempRfM => ("tyre_temp_rf_m", "C"),
    TyreTempRfR => ("tyre_temp_rf_r", "C"),
    TyreTempLrL => ("tyre_temp_lr_l", "C"),
    TyreTempLrM => ("tyre_temp_lr_m", "C"),
    TyreTempLrR => ("tyre_temp_lr_r", "C"),
    TyreTempRrL => ("tyre_temp_rr_l", "C"),
    TyreTempRrM => ("tyre_temp_rr_m", "C"),
    TyreTempRrR => ("tyre_temp_rr_r", "C"),
    TyrePressLf => ("tyre_press_lf", "kPa"),
    TyrePressRf => ("tyre_press_rf", "kPa"),
    TyrePressLr => ("tyre_press_lr", "kPa"),
    TyrePressRr => ("tyre_press_rr", "kPa"),
    TyreWearLf => ("tyre_wear_lf", "frac"),
    TyreWearRf => ("tyre_wear_rf", "frac"),
    TyreWearLr => ("tyre_wear_lr", "frac"),
    TyreWearRr => ("tyre_wear_rr", "frac"),
    BrakeTempLf => ("brake_temp_lf", "C"),
    BrakeTempRf => ("brake_temp_rf", "C"),
    BrakeTempLr => ("brake_temp_lr", "C"),
    BrakeTempRr => ("brake_temp_rr", "C"),
    RideHeightLf => ("ride_height_lf", "m"),
    RideHeightRf => ("ride_height_rf", "m"),
    RideHeightLr => ("ride_height_lr", "m"),
    RideHeightRr => ("ride_height_rr", "m"),
    TrackTempC => ("track_temp", "C"),
    AirTempC => ("air_temp", "C"),
}

/// One telemetry sample, normalized across sims.
///
/// `values` is indexed by [`Channel::index`] and always has [`Channel::COUNT`] entries;
/// NaN means "this sim doesn't provide that channel".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub session_time_s: f64,
    /// The lap the car is currently on, as counted by the sim.
    pub lap: i32,
    /// The sim's own time for the last completed lap, if it publishes one.
    pub sim_last_lap_time_s: Option<f32>,
    pub values: Vec<f32>,
}

impl Frame {
    pub fn new(session_time_s: f64, lap: i32) -> Self {
        Self {
            session_time_s,
            lap,
            sim_last_lap_time_s: None,
            values: vec![f32::NAN; Channel::COUNT],
        }
    }

    pub fn get(&self, ch: Channel) -> f32 {
        self.values[ch.index()]
    }

    pub fn set(&mut self, ch: Channel, v: f32) {
        self.values[ch.index()] = v;
    }

    /// Boolean channels are stored as 0/1; absent (NaN) reads as false.
    pub fn flag(&self, ch: Channel) -> bool {
        self.get(ch) > 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn channel_names_are_unique_and_round_trip() {
        let names: HashSet<_> = Channel::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Channel::COUNT);
        for &c in Channel::ALL {
            assert_eq!(Channel::from_name(c.name()), Some(c));
            assert_eq!(Channel::ALL[c.index()], c);
        }
        assert_eq!(Channel::from_name("nope"), None);
        assert_eq!(Channel::SpeedMs.name(), "speed");
        assert_eq!(Channel::SpeedMs.unit(), "m/s");
    }

    #[test]
    fn new_frame_has_every_channel_absent() {
        let f = Frame::new(12.5, 3);
        assert_eq!(f.values.len(), Channel::COUNT);
        assert!(Channel::ALL.iter().all(|&c| f.get(c).is_nan()));
        assert_eq!(f.lap, 3);
        assert_eq!(f.sim_last_lap_time_s, None);
    }

    #[test]
    fn set_get_and_flag() {
        let mut f = Frame::new(0.0, 1);
        f.set(Channel::Throttle, 0.75);
        f.set(Channel::OnPitRoad, 1.0);
        assert_eq!(f.get(Channel::Throttle), 0.75);
        assert!(f.flag(Channel::OnPitRoad));
        assert!(!f.flag(Channel::OffTrack)); // NaN counts as false
    }
}
