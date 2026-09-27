//! Parsing of the irsdk shared-memory layout (see irsdk_defines.h). Pure byte-slice code.

pub const HEADER_LEN: usize = 112;
pub const VAR_HEADER_LEN: usize = 144;
pub const MAX_BUFS: usize = 4;
const STATUS_CONNECTED: i32 = 1;
const NAME_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarType {
    Char,
    Bool,
    Int,
    BitField,
    Float,
    Double,
}

impl VarType {
    pub fn from_code(code: i32) -> Option<VarType> {
        Some(match code {
            0 => VarType::Char,
            1 => VarType::Bool,
            2 => VarType::Int,
            3 => VarType::BitField,
            4 => VarType::Float,
            5 => VarType::Double,
            _ => return None,
        })
    }

    pub fn code(self) -> i32 {
        match self {
            VarType::Char => 0,
            VarType::Bool => 1,
            VarType::Int => 2,
            VarType::BitField => 3,
            VarType::Float => 4,
            VarType::Double => 5,
        }
    }

    pub fn size(self) -> usize {
        match self {
            VarType::Char | VarType::Bool => 1,
            VarType::Int | VarType::BitField | VarType::Float => 4,
            VarType::Double => 8,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("buffer too short: need {need} bytes, have {have}")]
    TooShort { need: usize, have: usize },
    #[error("unknown irsdk var type {0}")]
    BadType(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VarBuf {
    pub tick_count: i32,
    pub buf_offset: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrHeader {
    pub ver: i32,
    pub status: i32,
    pub tick_rate: i32,
    pub session_info_update: i32,
    pub session_info_len: i32,
    pub session_info_offset: i32,
    pub num_vars: i32,
    pub var_header_offset: i32,
    pub num_buf: i32,
    pub buf_len: i32,
    pub var_bufs: [VarBuf; MAX_BUFS],
}

fn need(b: &[u8], len: usize) -> Result<(), LayoutError> {
    if b.len() < len {
        Err(LayoutError::TooShort { need: len, have: b.len() })
    } else {
        Ok(())
    }
}

fn i32_at(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

impl IrHeader {
    pub fn parse(b: &[u8]) -> Result<IrHeader, LayoutError> {
        need(b, HEADER_LEN)?;
        let mut var_bufs = [VarBuf::default(); MAX_BUFS];
        for (i, vb) in var_bufs.iter_mut().enumerate() {
            let base = 48 + i * 16;
            *vb = VarBuf { tick_count: i32_at(b, base), buf_offset: i32_at(b, base + 4) };
        }
        Ok(IrHeader {
            ver: i32_at(b, 0),
            status: i32_at(b, 4),
            tick_rate: i32_at(b, 8),
            session_info_update: i32_at(b, 12),
            session_info_len: i32_at(b, 16),
            session_info_offset: i32_at(b, 20),
            num_vars: i32_at(b, 24),
            var_header_offset: i32_at(b, 28),
            num_buf: i32_at(b, 32),
            buf_len: i32_at(b, 36),
            var_bufs,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.status & STATUS_CONNECTED != 0
    }

    /// The buffer slot holding the newest sample.
    pub fn latest_buf(&self) -> Option<VarBuf> {
        let used = self.num_buf.clamp(0, MAX_BUFS as i32) as usize;
        self.var_bufs[..used].iter().copied().max_by_key(|b| b.tick_count)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarHeader {
    pub var_type: VarType,
    pub offset: usize,
    pub count: usize,
    pub name: String,
    pub unit: String,
}

pub fn parse_var_headers(b: &[u8], num_vars: usize) -> Result<Vec<VarHeader>, LayoutError> {
    need(b, num_vars * VAR_HEADER_LEN)?;
    (0..num_vars)
        .map(|i| {
            let h = &b[i * VAR_HEADER_LEN..(i + 1) * VAR_HEADER_LEN];
            let code = i32_at(h, 0);
            Ok(VarHeader {
                var_type: VarType::from_code(code).ok_or(LayoutError::BadType(code))?,
                offset: i32_at(h, 4).max(0) as usize,
                count: i32_at(h, 8).max(0) as usize,
                name: cstr(&h[16..16 + NAME_LEN]),
                unit: cstr(&h[112..112 + NAME_LEN]),
            })
        })
        .collect()
}

/// Read element `index` of `var` from one telemetry buffer, as f64.
pub fn read_value(buf: &[u8], var: &VarHeader, index: usize) -> Option<f64> {
    if index >= var.count {
        return None;
    }
    let size = var.var_type.size();
    let off = var.offset + index * size;
    let bytes = buf.get(off..off + size)?;
    Some(match var.var_type {
        VarType::Char => bytes[0] as f64,
        VarType::Bool => (bytes[0] != 0) as u8 as f64,
        VarType::Int | VarType::BitField => i32::from_le_bytes(bytes.try_into().ok()?) as f64,
        VarType::Float => f32::from_le_bytes(bytes.try_into().ok()?) as f64,
        VarType::Double => f64::from_le_bytes(bytes.try_into().ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::iracing::testimage::ImageBuilder;

    fn image() -> Vec<u8> {
        let mut b = ImageBuilder::new()
            .var("SessionTime", VarType::Double)
            .var("Lap", VarType::Int)
            .var("Speed", VarType::Float)
            .var("OnPitRoad", VarType::Bool);
        b.yaml = "---\nWeekendInfo:\n TrackID: 1\n".into();
        b.tick = 500;
        b.set("SessionTime", 1234.5).set("Lap", 7.0).set("Speed", 55.5).set("OnPitRoad", 1.0);
        b.build()
    }

    #[test]
    fn parses_header() {
        let img = image();
        let h = IrHeader::parse(&img).unwrap();
        assert_eq!(h.ver, 2);
        assert!(h.is_connected());
        assert_eq!(h.num_vars, 4);
        assert_eq!(h.var_header_offset as usize, HEADER_LEN);
        assert_eq!(h.num_buf, 3);
        assert_eq!(h.buf_len, 8 + 4 + 4 + 1);
        assert_eq!(h.latest_buf().unwrap().tick_count, 500);
    }

    #[test]
    fn latest_buf_ignores_unused_slots() {
        let mut img = image();
        // A stale-but-huge tick in slot 3, which is beyond num_buf = 3.
        img[48 + 3 * 16..48 + 3 * 16 + 4].copy_from_slice(&9999i32.to_le_bytes());
        assert_eq!(IrHeader::parse(&img).unwrap().latest_buf().unwrap().tick_count, 500);
    }

    #[test]
    fn parses_var_headers_and_reads_values() {
        let img = image();
        let h = IrHeader::parse(&img).unwrap();
        let start = h.var_header_offset as usize;
        let vars = parse_var_headers(&img[start..], h.num_vars as usize).unwrap();
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["SessionTime", "Lap", "Speed", "OnPitRoad"]);
        assert_eq!(vars[0].var_type, VarType::Double);

        let latest = h.latest_buf().unwrap();
        let buf = &img[latest.buf_offset as usize..latest.buf_offset as usize + h.buf_len as usize];
        assert_eq!(read_value(buf, &vars[0], 0), Some(1234.5));
        assert_eq!(read_value(buf, &vars[1], 0), Some(7.0));
        assert_eq!(read_value(buf, &vars[2], 0), Some(55.5));
        assert_eq!(read_value(buf, &vars[3], 0), Some(1.0));
        assert_eq!(read_value(buf, &vars[2], 1), None, "index beyond count");
        assert_eq!(read_value(&buf[..2], &vars[2], 0), None, "buffer too short");
    }

    #[test]
    fn short_input_is_an_error_not_a_panic() {
        assert!(matches!(IrHeader::parse(&[0u8; 10]), Err(LayoutError::TooShort { .. })));
        assert!(matches!(parse_var_headers(&[0u8; 10], 1), Err(LayoutError::TooShort { .. })));
        let mut bad = vec![0u8; VAR_HEADER_LEN];
        bad[0..4].copy_from_slice(&42i32.to_le_bytes());
        assert_eq!(parse_var_headers(&bad, 1), Err(LayoutError::BadType(42)));
    }
}
