//! Sim adapters and the types they share.

pub mod iracing;
pub mod replay;

use crate::frame::Frame;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sim {
    Iracing,
    Lmu,
}

impl Sim {
    pub fn as_str(self) -> &'static str {
        match self {
            Sim::Iracing => "iracing",
            Sim::Lmu => "lmu",
        }
    }

    pub fn parse(s: &str) -> Option<Sim> {
        match s {
            "iracing" => Some(Sim::Iracing),
            "lmu" => Some(Sim::Lmu),
            _ => None,
        }
    }
}

/// Identity and static facts about the session being driven.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub sim: Sim,
    pub track_id: String,
    pub track_name: String,
    pub track_config: String,
    pub track_length_m: f32,
    pub car_id: String,
    pub car_name: String,
    pub session_type: String,
    /// Sorted lap fractions where each official sector starts (first is 0.0). Empty if unknown.
    pub sector_start_pcts: Vec<f32>,
    /// Free-form conditions such as `air_temp` or `track_temp`, stored as the sim reports them.
    pub conditions: BTreeMap<String, String>,
}

/// What a sim adapter saw on one poll.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PollResult {
    /// The sim isn't running (or just exited).
    NotConnected,
    /// The sim is running but the player isn't driving: menus, garage, replay, towed.
    /// A lap in progress is abandoned.
    Idle,
    /// No new data (the sim is paused). A lap in progress continues when data resumes.
    Paused,
    /// A new session started (first connection, session change, car/track change).
    Session(SessionInfo),
    /// A fresh telemetry sample while the player is driving.
    Frame(Frame),
}

/// A source of telemetry. `poll` may block briefly (≤ ~100 ms) waiting for data.
pub trait SimSource {
    fn sim(&self) -> Sim;
    fn poll(&mut self) -> PollResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_round_trips_through_str() {
        for s in [Sim::Iracing, Sim::Lmu] {
            assert_eq!(Sim::parse(s.as_str()), Some(s));
        }
        assert_eq!(serde_json::to_string(&Sim::Iracing).unwrap(), "\"iracing\"");
    }
}
