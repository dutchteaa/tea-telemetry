//! Tea Telemetry core: sim adapters, lap recording, storage and analysis.

pub mod frame;
pub mod laptime;
pub mod recorder;
pub mod sim;
pub mod store;
pub mod tlap;

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
