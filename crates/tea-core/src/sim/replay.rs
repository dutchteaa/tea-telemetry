//! Raw capture of adapter output, and a [`SimSource`] that plays it back.
//! Used for tests, fixtures and the "mock sim" dev mode.
//!
//! A capture is `zstd(bincode(CaptureHeader), bincode(PollResult)*)`. Frames are stored
//! in the channel order named by the header and remapped to the current
//! [`Channel::ALL`] order on replay, so adding, removing or reordering channels keeps old
//! captures readable. Any other change to how `PollResult`, `Frame` or `SessionInfo`
//! serialize changes the format and must bump [`CAPTURE_VERSION`].

use super::{PollResult, Sim, SimSource};
use crate::frame::{Channel, Frame};
use anyhow::Context;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::Duration;

/// v1 (no channel list) captures were never kept, so they aren't supported.
const CAPTURE_VERSION: u32 = 2;
const MAX_REALTIME_GAP_S: f64 = 0.5;
const NON_FRAME_DELAY: Duration = Duration::from_millis(16);
/// A single record bigger than this is corrupt (or hostile): reject it instead of
/// trusting its length prefixes and reading (or allocating for) arbitrarily much data.
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;

/// The exact settings `bincode::serialize_into`/`deserialize_from` use (fixint,
/// little-endian, trailing bytes allowed), plus a size limit. Used for the header and
/// every record, on both the write and the read side, so they can't drift apart.
fn record_options() -> impl bincode::Options {
    bincode::DefaultOptions::new().with_fixint_encoding().allow_trailing_bytes().with_limit(MAX_RECORD_BYTES)
}

#[derive(Serialize, Deserialize)]
struct CaptureHeader {
    version: u32,
    sim: Sim,
    /// Channel names in the order of every Frame's `values` in this file.
    channels: Vec<String>,
}

fn channel_names() -> Vec<String> {
    Channel::ALL.iter().map(|c| c.name().to_string()).collect()
}

pub struct CaptureWriter {
    enc: zstd::stream::write::AutoFinishEncoder<'static, BufWriter<File>>,
}

impl CaptureWriter {
    pub fn create(path: &Path, sim: Sim) -> anyhow::Result<Self> {
        let file = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
        let mut enc = zstd::stream::write::Encoder::new(file, 3)?.auto_finish();
        let header = CaptureHeader { version: CAPTURE_VERSION, sim, channels: channel_names() };
        record_options().serialize_into(&mut enc, &header)?;
        Ok(Self { enc })
    }

    pub fn write(&mut self, ev: &PollResult) -> anyhow::Result<()> {
        record_options().serialize_into(&mut self.enc, ev)?;
        Ok(())
    }
}

/// Moves a capture's frame values into the current channel order.
struct ChannelRemap {
    /// For each channel index in the file, its index now (None: channel no longer exists).
    targets: Vec<Option<usize>>,
}

impl ChannelRemap {
    fn new(file_channels: &[String]) -> Self {
        let targets = file_channels.iter().map(|n| Channel::from_name(n).map(Channel::index)).collect();
        Self { targets }
    }

    fn apply(&self, ev: PollResult) -> PollResult {
        let PollResult::Frame(f) = ev else { return ev };
        let mut values = vec![f32::NAN; Channel::COUNT];
        for (v, target) in f.values.iter().zip(&self.targets) {
            if let Some(i) = target {
                values[*i] = *v;
            }
        }
        PollResult::Frame(Frame { values, ..f })
    }
}

pub struct ReplaySource {
    sim: Sim,
    events: Box<dyn Iterator<Item = PollResult>>,
    realtime: bool,
    last_time_s: Option<f64>,
}

/// A capture that simply ends (normal end, or truncated by a crash) reads as UnexpectedEof.
fn is_clean_end(e: &bincode::Error) -> bool {
    matches!(&**e, bincode::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof)
}

impl ReplaySource {
    pub fn open(path: &Path, realtime: bool) -> anyhow::Result<Self> {
        let file = BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
        let mut dec = zstd::stream::read::Decoder::with_buffer(file)?;
        // The version comes first so an unsupported layout is reported, not misparsed.
        // Bounded the same as records: a corrupt length prefix here (e.g. a channel name)
        // must not make bincode resize a buffer to match an attacker-chosen size.
        let version: u32 = record_options().deserialize_from(&mut dec).context("not a Tea Telemetry capture file")?;
        if version > CAPTURE_VERSION {
            anyhow::bail!("capture was made by a newer version of Tea Telemetry");
        }
        if version < CAPTURE_VERSION {
            anyhow::bail!("capture format version {version} is no longer supported; please record it again");
        }
        let (sim, channels): (Sim, Vec<String>) =
            record_options().deserialize_from(&mut dec).context("not a Tea Telemetry capture file")?;
        let remap = ChannelRemap::new(&channels);
        let path_str = path.display().to_string();
        // A truncated capture (app killed while capturing) simply ends early.
        // Other errors (corruption) are logged before ending.
        let events = std::iter::from_fn(move || {
            match record_options().deserialize_from::<_, PollResult>(&mut dec) {
                Ok(ev) => Some(remap.apply(ev)),
                Err(e) => {
                    if !is_clean_end(&e) {
                        log::warn!("capture {}: stopped reading after a corrupt record: {}", path_str, e);
                    }
                    None
                }
            }
        });
        Ok(Self { sim, events: Box::new(events), realtime, last_time_s: None })
    }

    pub fn from_events(sim: Sim, events: Vec<PollResult>, realtime: bool) -> Self {
        Self { sim, events: Box::new(events.into_iter()), realtime, last_time_s: None }
    }

    fn pace(&mut self, ev: &PollResult) {
        if !self.realtime {
            return;
        }
        match ev {
            PollResult::Frame(f) => {
                if let Some(last) = self.last_time_s {
                    let gap = (f.session_time_s - last).clamp(0.0, MAX_REALTIME_GAP_S);
                    std::thread::sleep(Duration::from_secs_f64(gap));
                }
                self.last_time_s = Some(f.session_time_s);
            }
            _ => std::thread::sleep(NON_FRAME_DELAY),
        }
    }
}

impl SimSource for ReplaySource {
    fn sim(&self) -> Sim {
        self.sim
    }

    fn poll(&mut self) -> PollResult {
        match self.events.next() {
            Some(ev) => {
                self.pace(&ev);
                ev
            }
            None => PollResult::NotConnected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{drive, session};

    fn events() -> Vec<PollResult> {
        let mut ev = vec![PollResult::NotConnected, PollResult::Session(session())];
        ev.extend(drive(1, 0.5553, 1.0).into_iter().map(PollResult::Frame));
        ev.push(PollResult::Paused);
        ev.push(PollResult::Idle);
        ev
    }

    fn describe(ev: &PollResult) -> String {
        match ev {
            PollResult::Frame(f) => {
                let bits: Vec<u32> = f.values.iter().map(|v| v.to_bits()).collect();
                format!("F {} {} {:?} {:?}", f.session_time_s, f.lap, f.sim_last_lap_time_s, bits)
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn clean_end_is_not_reported_as_corruption() {
        // Test that a real EOF error is recognized as clean
        let err: bincode::Error = bincode::deserialize_from::<_, PollResult>(&mut &[][..]).unwrap_err();
        assert!(is_clean_end(&err), "empty input should produce UnexpectedEof");

        // Test that a corruption error is not recognized as clean
        // Use invalid enum tag that produces a non-Io error
        let mut invalid_data = Vec::new();
        invalid_data.extend_from_slice(&9u32.to_le_bytes()); // Invalid enum variant
        invalid_data.extend_from_slice(&[0u8; 12]); // Padding
        let err: bincode::Error = bincode::deserialize_from::<_, PollResult>(&mut &invalid_data[..]).unwrap_err();
        assert!(!is_clean_end(&err), "invalid enum tag should not be UnexpectedEof");
    }

    #[test]
    fn capture_round_trips_through_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.tcap");
        {
            let mut w = CaptureWriter::create(&path, Sim::Iracing).unwrap();
            for ev in events() {
                w.write(&ev).unwrap();
            }
        }
        let mut src = ReplaySource::open(&path, false).unwrap();
        assert_eq!(src.sim(), Sim::Iracing);
        for expected in events() {
            assert_eq!(describe(&src.poll()), describe(&expected));
        }
        assert!(matches!(src.poll(), PollResult::NotConnected));
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn from_events_yields_in_order_then_not_connected() {
        let mut src = ReplaySource::from_events(Sim::Lmu, vec![PollResult::Idle, PollResult::Paused], false);
        assert_eq!(src.sim(), Sim::Lmu);
        assert!(matches!(src.poll(), PollResult::Idle));
        assert!(matches!(src.poll(), PollResult::Paused));
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn open_rejects_non_capture_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.tcap");
        std::fs::write(&path, b"nope").unwrap();
        assert!(ReplaySource::open(&path, false).is_err());
    }

    #[test]
    fn corrupt_record_ends_replay_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.tcap");

        // Build raw uncompressed stream: bincode header + one valid event + corrupt bytes
        let mut raw = Vec::new();
        let header = CaptureHeader { version: CAPTURE_VERSION, sim: Sim::Iracing, channels: channel_names() };
        bincode::serialize_into(&mut raw, &header).unwrap();
        bincode::serialize_into(&mut raw, &PollResult::Idle).unwrap();
        // Append 16 bytes of 0xFF to corrupt the next record
        raw.extend_from_slice(&[0xFF; 16]);

        // Compress with zstd and write to file
        let compressed = zstd::encode_all(raw.as_slice(), 3).unwrap();
        std::fs::write(&path, compressed).unwrap();

        // Open and verify it yields the valid event then stops
        let mut src = ReplaySource::open(&path, false).unwrap();
        assert_eq!(src.sim(), Sim::Iracing);
        assert!(matches!(src.poll(), PollResult::Idle));
        assert!(matches!(src.poll(), PollResult::NotConnected));
        // Should not panic on further polls
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    /// bincode 1.3.3's `IoReader::fill_buffer` resizes a temp buffer to a String's or
    /// `Vec<u8>`'s claimed length *before* reading it, guarded only by the size limit.
    /// Unbounded, a lying length prefix there makes it try to allocate an amount of
    /// memory no allocator can satisfy, which aborts the process (not a catchable panic)
    /// rather than returning an error. These two tests hand-craft such a lie in the
    /// header and in a record and check both come back as an ordinary `Err`/`NotConnected`.
    #[test]
    fn huge_channel_name_length_in_the_header_is_rejected_not_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge-header.tcap");
        let mut raw = Vec::new();
        raw.extend_from_slice(&CAPTURE_VERSION.to_le_bytes()); // version: u32
        raw.extend_from_slice(&0u32.to_le_bytes()); // sim: Sim::Iracing
        raw.extend_from_slice(&1u64.to_le_bytes()); // channels: Vec<String>, one entry
        raw.extend_from_slice(&(1u64 << 62).to_le_bytes()); // that entry's String length: a lie
        // No name bytes follow: the file simply doesn't have them.
        let compressed = zstd::encode_all(raw.as_slice(), 3).unwrap();
        std::fs::write(&path, compressed).unwrap();

        assert!(ReplaySource::open(&path, false).is_err());
    }

    #[test]
    fn huge_string_length_in_a_session_record_is_rejected_not_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge-session.tcap");
        let header = CaptureHeader { version: CAPTURE_VERSION, sim: Sim::Iracing, channels: channel_names() };
        let mut raw = Vec::new();
        bincode::serialize_into(&mut raw, &header).unwrap();
        raw.extend_from_slice(&3u32.to_le_bytes()); // PollResult::Session variant index
        raw.extend_from_slice(&0u32.to_le_bytes()); // SessionInfo.sim: Sim::Iracing
        raw.extend_from_slice(&(1u64 << 62).to_le_bytes()); // SessionInfo.track_id length: a lie
        let compressed = zstd::encode_all(raw.as_slice(), 3).unwrap();
        std::fs::write(&path, compressed).unwrap();

        let mut src = ReplaySource::open(&path, false).unwrap();
        assert!(matches!(src.poll(), PollResult::NotConnected));
        assert!(matches!(src.poll(), PollResult::NotConnected));
    }

    /// Write a capture by hand, as an older or differently-built app might have.
    fn write_raw_capture(path: &Path, header: &impl Serialize, events: &[PollResult]) {
        let mut raw = Vec::new();
        bincode::serialize_into(&mut raw, header).unwrap();
        for ev in events {
            bincode::serialize_into(&mut raw, ev).unwrap();
        }
        std::fs::write(path, zstd::encode_all(raw.as_slice(), 3).unwrap()).unwrap();
    }

    #[test]
    fn frames_are_remapped_from_the_capture_channel_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other-order.tcap");
        let header = CaptureHeader {
            version: CAPTURE_VERSION,
            sim: Sim::Iracing,
            channels: vec!["speed".into(), "retired_channel".into(), "lap_dist_pct".into()],
        };
        let mut f = Frame::new(12.5, 4);
        f.values = vec![55.0, 9.0, 0.25];
        write_raw_capture(&path, &header, &[PollResult::Frame(f)]);

        let mut src = ReplaySource::open(&path, false).unwrap();
        match src.poll() {
            PollResult::Frame(f) => {
                assert_eq!(f.values.len(), Channel::COUNT);
                assert_eq!(f.get(Channel::SpeedMs), 55.0);
                assert_eq!(f.get(Channel::LapDistPct), 0.25);
                assert!(f.get(Channel::LapDistM).is_nan());
                assert_eq!(Channel::ALL.iter().filter(|&&c| !f.get(c).is_nan()).count(), 2);
                assert_eq!((f.session_time_s, f.lap), (12.5, 4));
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn version_1_captures_are_refused_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.tcap");
        write_raw_capture(&path, &(1u32, Sim::Iracing), &[PollResult::Idle]);
        let err = ReplaySource::open(&path, false).err().expect("v1 must be refused").to_string();
        assert!(err.contains("version 1"), "{err}");
    }
}
