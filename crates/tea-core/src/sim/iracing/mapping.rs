//! Maps irsdk telemetry variables onto our [`Channel`]s.

use super::layout::{read_value, VarHeader};
use crate::frame::{Channel, Frame};

/// irsdk_TrkLoc::irsdk_OffTrack
const TRKLOC_OFF_TRACK: i32 = 0;

#[derive(Clone, Copy, Debug)]
enum Transform {
    Raw,
    /// iRacing's Clutch is 1 = fully engaged; we store pedal travel (1 = pressed).
    OneMinus,
}

use Channel::*;
use Transform::*;

/// Channel ← irsdk var name. Vars that don't exist in live telemetry are simply skipped.
const CHANNEL_VARS: &[(Channel, &str, Transform)] = &[
    (LapDistM, "LapDist", Raw),
    (LapDistPct, "LapDistPct", Raw),
    (SpeedMs, "Speed", Raw),
    (Throttle, "Throttle", Raw),
    (Brake, "Brake", Raw),
    (Clutch, "Clutch", OneMinus),
    (SteeringRad, "SteeringWheelAngle", Raw),
    (Gear, "Gear", Raw),
    (Rpm, "RPM", Raw),
    (YawRad, "Yaw", Raw),
    (VelLongMs, "VelocityX", Raw),
    (VelLatMs, "VelocityY", Raw),
    (OnPitRoad, "OnPitRoad", Raw),
    (FuelL, "FuelLevel", Raw),
    (TyreTempLfL, "LFtempCL", Raw),
    (TyreTempLfM, "LFtempCM", Raw),
    (TyreTempLfR, "LFtempCR", Raw),
    (TyreTempRfL, "RFtempCL", Raw),
    (TyreTempRfM, "RFtempCM", Raw),
    (TyreTempRfR, "RFtempCR", Raw),
    (TyreTempLrL, "LRtempCL", Raw),
    (TyreTempLrM, "LRtempCM", Raw),
    (TyreTempLrR, "LRtempCR", Raw),
    (TyreTempRrL, "RRtempCL", Raw),
    (TyreTempRrM, "RRtempCM", Raw),
    (TyreTempRrR, "RRtempCR", Raw),
    (TyrePressLf, "LFpressure", Raw),
    (TyrePressRf, "RFpressure", Raw),
    (TyrePressLr, "LRpressure", Raw),
    (TyrePressRr, "RRpressure", Raw),
    (TyreWearLf, "LFwearM", Raw),
    (TyreWearRf, "RFwearM", Raw),
    (TyreWearLr, "LRwearM", Raw),
    (TyreWearRr, "RRwearM", Raw),
    (RideHeightLf, "LFrideHeight", Raw),
    (RideHeightRf, "RFrideHeight", Raw),
    (RideHeightLr, "LRrideHeight", Raw),
    (RideHeightRr, "RRrideHeight", Raw),
    (TrackTempC, "TrackTempCrew", Raw),
    (AirTempC, "AirTemp", Raw),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    pub is_replay: bool,
    pub is_on_track: bool,
    pub session_num: i32,
}

#[derive(Default)]
pub struct VarMap {
    session_time: Option<VarHeader>,
    lap: Option<VarHeader>,
    last_lap_time: Option<VarHeader>,
    is_replay: Option<VarHeader>,
    is_on_track: Option<VarHeader>,
    session_num: Option<VarHeader>,
    track_surface: Option<VarHeader>,
    channels: Vec<(Channel, VarHeader, Transform)>,
}

impl VarMap {
    pub fn resolve(vars: &[VarHeader]) -> VarMap {
        let find = |name: &str| vars.iter().find(|v| v.name == name).cloned();
        VarMap {
            session_time: find("SessionTime"),
            lap: find("Lap"),
            last_lap_time: find("LapLastLapTime"),
            is_replay: find("IsReplayPlaying"),
            is_on_track: find("IsOnTrack"),
            session_num: find("SessionNum"),
            track_surface: find("PlayerTrackSurface"),
            channels: CHANNEL_VARS
                .iter()
                .filter_map(|(ch, name, tr)| find(name).map(|v| (*ch, v, *tr)))
                .collect(),
        }
    }

    pub fn read(&self, buf: &[u8]) -> Option<(Frame, Control)> {
        let value = |v: &Option<VarHeader>| v.as_ref().and_then(|v| read_value(buf, v, 0));
        let session_time = value(&self.session_time)?;
        let mut frame = Frame::new(session_time, value(&self.lap).unwrap_or(0.0) as i32);
        frame.sim_last_lap_time_s = value(&self.last_lap_time).filter(|t| *t > 0.0).map(|t| t as f32);
        for (ch, var, transform) in &self.channels {
            if let Some(v) = read_value(buf, var, 0) {
                frame.set(*ch, match transform {
                    Raw => v as f32,
                    OneMinus => (1.0 - v) as f32,
                });
            }
        }
        if let Some(surface) = value(&self.track_surface) {
            frame.set(OffTrack, if surface as i32 == TRKLOC_OFF_TRACK { 1.0 } else { 0.0 });
        }
        let control = Control {
            is_replay: value(&self.is_replay).is_some_and(|v| v != 0.0),
            is_on_track: value(&self.is_on_track).map_or(true, |v| v != 0.0),
            session_num: value(&self.session_num).unwrap_or(0.0) as i32,
        };
        Some((frame, control))
    }
}
