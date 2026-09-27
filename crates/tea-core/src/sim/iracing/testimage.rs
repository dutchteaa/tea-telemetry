//! Builds synthetic irsdk shared-memory images for tests.

use super::layout::{VarType, HEADER_LEN, VAR_HEADER_LEN};
use std::collections::HashMap;

pub struct ImageBuilder {
    vars: Vec<(String, VarType)>,
    values: HashMap<String, f64>,
    pub yaml: String,
    pub tick: i32,
    pub status: i32,
    pub session_info_update: i32,
}

fn put_i32(img: &mut [u8], off: usize, v: i32) {
    img[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

impl ImageBuilder {
    pub fn new() -> Self {
        Self {
            vars: Vec::new(),
            values: HashMap::new(),
            yaml: String::new(),
            tick: 100,
            status: 1,
            session_info_update: 1,
        }
    }

    pub fn var(mut self, name: &str, ty: VarType) -> Self {
        self.vars.push((name.to_string(), ty));
        self
    }

    pub fn set(&mut self, name: &str, v: f64) -> &mut Self {
        self.values.insert(name.to_string(), v);
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let num_vars = self.vars.len();
        let var_header_offset = HEADER_LEN;
        let yaml_offset = var_header_offset + num_vars * VAR_HEADER_LEN;
        let yaml_len = self.yaml.len() + 1; // NUL-terminated
        let mut offsets = Vec::with_capacity(num_vars);
        let mut buf_len = 0usize;
        for (_, ty) in &self.vars {
            offsets.push(buf_len);
            buf_len += ty.size();
        }
        let bufs_start = (yaml_offset + yaml_len + 15) / 16 * 16;
        let num_buf = 3usize;
        let mut img = vec![0u8; bufs_start + num_buf * buf_len];

        put_i32(&mut img, 0, 2);
        put_i32(&mut img, 4, self.status);
        put_i32(&mut img, 8, 60);
        put_i32(&mut img, 12, self.session_info_update);
        put_i32(&mut img, 16, yaml_len as i32);
        put_i32(&mut img, 20, yaml_offset as i32);
        put_i32(&mut img, 24, num_vars as i32);
        put_i32(&mut img, 28, var_header_offset as i32);
        put_i32(&mut img, 32, num_buf as i32);
        put_i32(&mut img, 36, buf_len as i32);
        // Slot 1 holds the newest tick (and the real data); the others are older.
        let ticks = [self.tick - 2, self.tick, self.tick - 1];
        for (slot, tick) in ticks.iter().enumerate() {
            let base = 48 + slot * 16;
            put_i32(&mut img, base, *tick);
            put_i32(&mut img, base + 4, (bufs_start + slot * buf_len) as i32);
        }
        for (i, (name, ty)) in self.vars.iter().enumerate() {
            let base = var_header_offset + i * VAR_HEADER_LEN;
            put_i32(&mut img, base, ty.code());
            put_i32(&mut img, base + 4, offsets[i] as i32);
            put_i32(&mut img, base + 8, 1);
            img[base + 16..base + 16 + name.len()].copy_from_slice(name.as_bytes());
        }
        img[yaml_offset..yaml_offset + self.yaml.len()].copy_from_slice(self.yaml.as_bytes());

        let data = bufs_start + buf_len; // slot 1
        for (i, (name, ty)) in self.vars.iter().enumerate() {
            let v = self.values.get(name).copied().unwrap_or(0.0);
            let off = data + offsets[i];
            match ty {
                VarType::Char | VarType::Bool => img[off] = v as u8,
                VarType::Int | VarType::BitField => put_i32(&mut img, off, v as i32),
                VarType::Float => img[off..off + 4].copy_from_slice(&(v as f32).to_le_bytes()),
                VarType::Double => img[off..off + 8].copy_from_slice(&v.to_le_bytes()),
            }
        }
        img
    }
}

pub const SAMPLE_YAML: &str = "---
WeekendInfo:
 TrackName: spa 2024 up
 TrackID: 525
 TrackLength: 6.93 km
 TrackDisplayName: Circuit de Spa-Francorchamps
 TrackConfigName: Grand Prix
 TrackSkies: Partly Cloudy
 TrackSurfaceTemp: 31.20 C
 TrackAirTemp: 22.40 C
 SubSessionID: 0
 WeekendOptions:
  NumStarters: 0
  StartingGrid: single file
SessionInfo:
 Sessions:
 - SessionNum: 0
   SessionLaps: unlimited
   SessionTime: 7200.0000 sec
   SessionType: Practice
   ResultsPositions:
   - Position: 1
     CarIdx: 0
   - Position: 2
     CarIdx: 1
 - SessionNum: 1
   SessionType: Race
DriverInfo:
 DriverCarIdx: 1
 Drivers:
 - CarIdx: 0
   UserName: Pace Car
   CarID: 11
   CarScreenName: safety pcporsche911cup
 - CarIdx: 1
   UserName: \"Test Driver: One\"
   CarID: 170
   CarScreenName: Porsche 963 GTP
SplitTimeInfo:
 Sectors:
 - SectorNum: 0
   SectorStartPct: 0.000000
 - SectorNum: 1
   SectorStartPct: 0.330000
 - SectorNum: 2
   SectorStartPct: 0.660000
...
";
