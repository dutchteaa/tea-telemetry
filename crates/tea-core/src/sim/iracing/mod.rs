//! iRacing adapter: reads the irsdk shared-memory telemetry.

pub mod layout;
pub mod mapping;
pub mod source;
#[cfg(test)]
pub(crate) mod testimage;
#[cfg(windows)]
pub mod win;
pub mod yaml;

pub use source::{IracingSource, SharedMem};

/// The real iRacing source, reading the sim's shared memory.
#[cfg(windows)]
pub fn live_source() -> IracingSource<win::WinSharedMem> {
    IracingSource::new(Box::new(win::connect))
}
