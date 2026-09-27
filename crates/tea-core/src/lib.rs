//! Tea Telemetry core: sim adapters, lap recording, storage and analysis.

pub mod frame;
pub mod sim;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
