//! `.tlap`: one lap's telemetry in a portable, self-describing, compressed file.

use crate::sim::SessionInfo;
use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"TLAP";
const PREFIX_LEN: usize = 12;
/// zstd frame magic number, little-endian.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelEntry {
    pub name: String,
    pub unit: String,
    /// Byte offset of this channel's data, relative to the start of the data section.
    pub offset: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TlapHeader {
    pub format_version: u32,
    pub lap_id: String,
    pub session_id: String,
    pub session_started_at_ms: i64,
    pub session: SessionInfo,
    pub lap_number: i32,
    pub lap_time_ms: i64,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub fuel_start_l: Option<f32>,
    pub fuel_used_l: Option<f32>,
    pub sector_times_ms: Vec<i64>,
    pub created_at_ms: i64,
    pub sample_count: u64,
    pub channels: Vec<ChannelEntry>,
}

#[derive(Clone, Debug)]
pub struct TlapFile {
    pub header: TlapHeader,
    /// `data[i]` holds the samples of `header.channels[i]`.
    pub data: Vec<Vec<f32>>,
}

#[derive(Debug, thiserror::Error)]
pub enum TlapError {
    #[error("not a .tlap file")]
    BadMagic,
    #[error("this lap was made by a newer version of Tea Telemetry (format {found}; this version supports up to {supported})")]
    NewerVersion { found: u32, supported: u32 },
    #[error("corrupt .tlap file: {0}")]
    Corrupt(String),
    #[error("invalid lap data: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Serialize a lap. Fills in `format_version`, `sample_count` and each channel's `offset`.
pub fn encode(header: &mut TlapHeader, data: &[Vec<f32>]) -> Result<Vec<u8>, TlapError> {
    if data.len() != header.channels.len() {
        return Err(TlapError::Invalid(format!(
            "{} channels declared but {} provided",
            header.channels.len(),
            data.len()
        )));
    }
    let n = data.first().map_or(0, |c| c.len());
    if data.iter().any(|c| c.len() != n) {
        return Err(TlapError::Invalid("channels have different lengths".into()));
    }
    header.format_version = FORMAT_VERSION;
    header.sample_count = n as u64;
    for (i, ch) in header.channels.iter_mut().enumerate() {
        ch.offset = (i * n * 4) as u64;
    }
    let json = serde_json::to_vec(header)?;
    let mut raw = Vec::with_capacity(PREFIX_LEN + json.len() + data.len() * n * 4);
    raw.extend_from_slice(MAGIC);
    raw.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    raw.extend_from_slice(&(json.len() as u32).to_le_bytes());
    raw.extend_from_slice(&json);
    for channel in data {
        for v in channel {
            raw.extend_from_slice(&v.to_le_bytes());
        }
    }
    Ok(zstd::encode_all(&raw[..], 3)?)
}

pub fn decode(bytes: &[u8]) -> Result<TlapFile, TlapError> {
    let raw = zstd::decode_all(bytes).map_err(|e| {
        if bytes.starts_with(&ZSTD_MAGIC) {
            // It really is (the start of) a .tlap file; it just didn't survive intact.
            TlapError::Corrupt(format!("couldn't decompress: {e}"))
        } else {
            TlapError::BadMagic
        }
    })?;
    if raw.len() < PREFIX_LEN || &raw[0..4] != MAGIC {
        return Err(TlapError::BadMagic);
    }
    let u32_at = |off: usize| u32::from_le_bytes(raw[off..off + 4].try_into().expect("4 bytes"));
    let version = u32_at(4);
    if version > FORMAT_VERSION {
        return Err(TlapError::NewerVersion { found: version, supported: FORMAT_VERSION });
    }
    let header_len = u32_at(8) as usize;
    let json = raw
        .get(PREFIX_LEN..PREFIX_LEN + header_len)
        .ok_or_else(|| TlapError::Corrupt("header truncated".into()))?;
    let header: TlapHeader = serde_json::from_slice(json)?;

    let data_start = PREFIX_LEN + header_len;
    let bytes_per_channel = (header.sample_count as usize)
        .checked_mul(4)
        .ok_or_else(|| TlapError::Corrupt("sample count too large".into()))?;
    let mut data = Vec::with_capacity(header.channels.len());
    for ch in &header.channels {
        let start = data_start + ch.offset as usize;
        let slice = raw
            .get(start..start + bytes_per_channel)
            .ok_or_else(|| TlapError::Corrupt(format!("channel {} truncated", ch.name)))?;
        data.push(
            slice
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().expect("4 bytes")))
                .collect(),
        );
    }
    Ok(TlapFile { header, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::session;

    fn header() -> TlapHeader {
        TlapHeader {
            format_version: 0,
            lap_id: "lap-1".into(),
            session_id: "sess-1".into(),
            session_started_at_ms: 1,
            session: session(),
            lap_number: 2,
            lap_time_ms: 100_000,
            valid: true,
            invalid_reason: None,
            fuel_start_l: Some(59.1),
            fuel_used_l: Some(2.0),
            sector_times_ms: vec![30_000, 40_000, 30_000],
            created_at_ms: 5,
            sample_count: 0,
            channels: vec![
                ChannelEntry { name: "t_s".into(), unit: "s".into(), offset: 0 },
                ChannelEntry { name: "speed".into(), unit: "m/s".into(), offset: 0 },
            ],
        }
    }

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    #[test]
    fn round_trips_header_and_data_including_nan() {
        let mut h = header();
        let data = vec![vec![0.0, 0.1, 0.2], vec![70.0, f32::NAN, 71.5]];
        let bytes = encode(&mut h, &data).unwrap();
        assert_eq!(h.format_version, FORMAT_VERSION);
        assert_eq!(h.sample_count, 3);
        assert_eq!(h.channels[1].offset, 12);

        let file = decode(&bytes).unwrap();
        assert_eq!(file.header, h);
        assert_eq!(file.data.len(), 2);
        assert_eq!(bits(&file.data[0]), bits(&data[0]));
        assert_eq!(bits(&file.data[1]), bits(&data[1]));
    }

    #[test]
    fn rejects_mismatched_channel_lengths() {
        let mut h = header();
        let err = encode(&mut h, &[vec![0.0, 1.0], vec![1.0]]).unwrap_err();
        assert!(matches!(err, TlapError::Invalid(_)));
        let err = encode(&mut h, &[vec![0.0]]).unwrap_err();
        assert!(matches!(err, TlapError::Invalid(_)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(decode(b"definitely not a lap"), Err(TlapError::BadMagic)));
        let not_tlap = zstd::encode_all(&b"JUNKxxxxxxxxxxxx"[..], 3).unwrap();
        assert!(matches!(decode(&not_tlap), Err(TlapError::BadMagic)));
    }

    #[test]
    fn refuses_newer_format_with_clear_message() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"TLAP");
        raw.extend_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.extend_from_slice(b"{}");
        let bytes = zstd::encode_all(&raw[..], 3).unwrap();
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, TlapError::NewerVersion { found: 2, supported: 1 }));
        assert!(err.to_string().contains("newer version"));
    }

    #[test]
    fn truncated_zstd_frame_reports_corrupt_not_bad_magic() {
        let mut h = header();
        let data = vec![vec![0.0, 0.1, 0.2], vec![70.0, 71.0, 71.5]];
        let bytes = encode(&mut h, &data).unwrap();
        // A real zstd frame (the magic is intact) cut off partway through: the sim or
        // disk died mid-write, not a file that was never a .tlap at all.
        let truncated = &bytes[..bytes.len() - 4];
        assert!(matches!(decode(truncated), Err(TlapError::Corrupt(_))));
    }

    #[test]
    fn detects_truncated_channel_data() {
        let mut h = header();
        h.sample_count = 1000; // lie about the size
        h.format_version = FORMAT_VERSION;
        let json = serde_json::to_vec(&h).unwrap();
        let mut raw = Vec::new();
        raw.extend_from_slice(b"TLAP");
        raw.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        raw.extend_from_slice(&(json.len() as u32).to_le_bytes());
        raw.extend_from_slice(&json);
        raw.extend_from_slice(&[0u8; 8]);
        let bytes = zstd::encode_all(&raw[..], 3).unwrap();
        assert!(matches!(decode(&bytes), Err(TlapError::Corrupt(_))));
    }
}
