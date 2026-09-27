# Tea Telemetry Phase 1 (Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Tauri desktop app that runs in the background, auto-detects iRacing, and records every completed lap to `%APPDATA%\tea-telemetry` (SQLite index + `.tlap` file per lap), with a minimal status + recent-laps screen.

**Architecture:** A pure-Rust library crate `tea-core` holds everything testable: the channel registry, sim adapters behind a `SimSource` trait, the lap-splitting `Recorder` state machine, the `.tlap` format, the SQLite `Store`, and a `RecorderService` thread that ties them together. The Tauri app in `src-tauri` is a thin shell that starts the service and exposes two commands to a Svelte page. iRacing's shared memory is read through a `SharedMem` trait, so all adapter logic is tested against synthetic memory images; only a ~100-line Win32 file touches the real sim.

**Tech Stack:** Rust (stable, MSVC), Tauri 2, SvelteKit (Svelte 5, TypeScript) via create-tauri-app, rusqlite (bundled SQLite), zstd, serde/serde_json/bincode 1.3, uuid, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-27-tea-telemetry-design.md` (this plan implements build-order step 1: scaffold, `Frame`/channel registry, iRacing adapter, recorder, store. It also adds the `ReplaySource`, raw capture, and mock-sim mode from spec §9, because later phases depend on them.)

**Later phases (separate plans):** 2 Analysis · 3 Library UI · 4 Compare UI · 5 LMU adapter · 6 Tray, settings, `.tlap` export/import.

## Global Constraints

- Platform: Windows 10/11 only. Rust stable **MSVC** toolchain. Tauri **2**. Svelte **5** + TypeScript.
- Data root: `%APPDATA%\tea-telemetry\` containing `index.db`, `laps/<uuid>.tlap`, `logs/`, and (debug only) `captures/`.
- `.tlap` `format_version` = **1**; SQLite `schema_version` = **1**. A newer version of either is refused with a clear message, never misread.
- Samples are stored at the sim's native rate; **no resampling at record time**.
- Units are SI throughout: m, m/s, rad, L, °C, kPa, fractions 0–1. Channel names are exactly those in `frame.rs` (Task 2).
- Recording must never slow the sim or the UI. The recorder runs on its own thread, and a panic there must not take down the app.
- Nothing leaves the PC: no network calls anywhere.
- Laps get a random UUID v4 `lap_id`; sessions get a UUID v4 `session_id`.
- Commit at the end of every task. Every commit message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Naming refinement vs. spec §4: the spec's `vel_x`/`vel_z` are named `vel_long`/`vel_lat` (car-local longitudinal/lateral), and tyre temps are `_l/_m/_r` (left/middle/right across the tread as seen from behind), because that is what both sims actually expose.

## Review Focus

1. **Sim not running at launch, or closed and reopened mid-session.** The app keeps running, shows "no sim", and reconnects when the sim comes back. Pinned by the Task 10 test `sim_exit_then_reconnect` and the Task 11 test `records_laps_from_replay`.
2. **Joining mid-lap / leaving the pits mid-track.** A lap whose start line was never seen must not be saved with a bogus time. Pinned by the Task 4 test `records_full_laps_and_discards_partial_first_lap`.
3. **Recorder thread panics** (malformed shared memory, bug). The recorder restarts, the error is visible in status, and the app survives. Pinned by the Task 11 test `recorder_restarts_after_panic`.
4. **App killed mid-write.** No half-written lap is ever indexed; orphaned files are re-indexed and `.tmp` files are cleaned on next start. Pinned by the Task 6 tests `reconcile_reindexes_orphan_files` and `reconcile_cleans_temp_and_flags_missing`.
5. **Unexpected iRacing session YAML** (missing sections, colons or quotes in names, nested result lists). No panic; sensible defaults. Pinned by the Task 9 tests `missing_sections_give_defaults` and `values_with_colons_and_quotes`.

---

## File Structure

```
Cargo.toml                                 workspace: crates/tea-core + src-tauri
.gitignore
package.json, svelte.config.js, vite.config.js, tsconfig.json   (from scaffold)
src/routes/+page.svelte                    status pill + recent laps (Task 12)
src/lib/format.ts, src/lib/format.test.ts  lap-time formatting (Task 12)
src-tauri/                                 Tauri shell (scaffold; lib.rs rewritten in Task 12)
crates/tea-core/
  Cargo.toml
  examples/make_mock_capture.rs            synthetic capture for mock-sim mode (Task 12)
  src/lib.rs                               module list
  src/frame.rs                             Channel registry + Frame (Task 2)
  src/sim/mod.rs                           Sim, SessionInfo, PollResult, SimSource (Task 2)
  src/laptime.rs                           line-crossing + sector-time math (Task 3)
  src/testutil.rs                          synthetic session/frames for tests (Task 4)
  src/recorder.rs                          lap-splitting state machine (Task 4)
  src/tlap.rs                              .tlap encode/decode (Task 5)
  src/store.rs                             SQLite index, crash-safe save, reconcile (Task 6)
  src/sim/replay.rs                        capture writer + ReplaySource (Task 7)
  src/sim/iracing/mod.rs                   module list + live_source() (Tasks 8–10)
  src/sim/iracing/layout.rs                irsdk header/var-header parsing (Task 8)
  src/sim/iracing/testimage.rs             synthetic shared-memory image builder (Task 8)
  src/sim/iracing/yaml.rs                  session-info YAML subset parser (Task 9)
  src/sim/iracing/mapping.rs               irsdk var → Channel mapping (Task 10)
  src/sim/iracing/source.rs                IracingSource poll logic over SharedMem (Task 10)
  src/sim/iracing/win.rs                   Win32 shared-memory + event FFI (Task 10)
  src/service.rs                           RecorderService thread + supervisor (Task 11)
```

---

### Task 1: Toolchain and workspace scaffold

**Files:**
- Create: everything from the create-tauri-app scaffold, `Cargo.toml` (workspace), `crates/tea-core/Cargo.toml`, `crates/tea-core/src/lib.rs`
- Modify: `.gitignore`, `src-tauri/Cargo.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: a Cargo workspace with members `crates/tea-core` (lib crate `tea_core`) and `src-tauri` (package `tea-telemetry`, lib `tea_telemetry_lib`). `npm run tauri dev` opens a window.

- [ ] **Step 1: Install the Visual Studio C++ build tools** (needed by Rust MSVC, zstd and bundled SQLite). This triggers a UAC prompt the user must accept.

Run (PowerShell):
```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```
Expected: exits 0 (or reports already installed).

- [ ] **Step 2: Install Rust**

Run (PowerShell):
```powershell
winget install --id Rustlang.Rustup --accept-package-agreements --accept-source-agreements
```
Then in Git Bash (new shells pick up PATH automatically; this makes the current one work):
```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustup default stable-msvc
cargo --version
```
Expected: `cargo 1.x.y ...`.

- [ ] **Step 3: Scaffold the Tauri app in a temp dir and copy it in**

```bash
mkdir -p "$TEMP/tea-scaffold" && cd "$TEMP/tea-scaffold"
npm create tauri-app@latest tea-telemetry -- --template svelte-ts --manager npm --identifier com.teatelemetry.app --yes
cd /c/Users/aiden/Documents/Projects/tea-telemetry
cp -r "$TEMP/tea-scaffold/tea-telemetry/." .
ls src-tauri src-tauri/src src/routes
```
Expected: `src-tauri` contains `Cargo.toml build.rs tauri.conf.json capabilities icons src`, `src-tauri/src` contains `lib.rs main.rs`, and `src/routes` contains `+page.svelte`. Open `src-tauri/Cargo.toml`: `[package] name = "tea-telemetry"` and `[lib] name = "tea_telemetry_lib"`. If the names differ, use the actual names in later steps.

- [ ] **Step 4: Replace `.gitignore`** (the scaffold overwrote ours)

```gitignore
node_modules/
/build
/.svelte-kit
/package
.env
.env.*
!.env.example
vite.config.js.timestamp-*
vite.config.ts.timestamp-*
target/
.superpowers/
*.tcap
```

- [ ] **Step 5: Create the workspace `Cargo.toml`** at the repo root

```toml
[workspace]
resolver = "2"
members = ["crates/tea-core", "src-tauri"]
```

- [ ] **Step 6: Create `crates/tea-core/Cargo.toml`**

```toml
[package]
name = "tea-core"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bincode = "1.3"
zstd = "0.13"
rusqlite = { version = "0.32", features = ["bundled"] }
uuid = { version = "1", features = ["v4"] }
log = "0.4"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 7: Create `crates/tea-core/src/lib.rs`** with a smoke test

```rust
//! Tea Telemetry core: sim adapters, lap recording, storage and analysis.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 8: Depend on tea-core from the app.** In `src-tauri/Cargo.toml` under `[dependencies]` add:

```toml
tea-core = { path = "../crates/tea-core" }
log = "0.4"
```

- [ ] **Step 9: Install and verify**

```bash
npm install
cargo test -p tea-core
cargo build -p tea-telemetry
```
Expected: the test `crate_builds ... ok`; the app builds with no errors. The first build takes several minutes.

- [ ] **Step 10: Check the window opens.** Run `npm run tauri dev` in the background, wait for `Running target\debug\tea-telemetry.exe` (or a window appearing) in its output, then stop it.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "chore: scaffold Tauri + SvelteKit app and tea-core workspace crate

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Channel registry, Frame, and sim types

**Files:**
- Create: `crates/tea-core/src/frame.rs`, `crates/tea-core/src/sim/mod.rs`
- Modify: `crates/tea-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `frame::Channel` (enum; `Channel::ALL: &[Channel]`, `Channel::COUNT: usize`, `name(self) -> &'static str`, `unit(self) -> &'static str`, `from_name(&str) -> Option<Channel>`, `index(self) -> usize`)
  - `frame::Frame { session_time_s: f64, lap: i32, sim_last_lap_time_s: Option<f32>, values: Vec<f32> }` with `Frame::new(f64, i32)`, `get(Channel) -> f32` (NaN = absent), `set(Channel, f32)`, `flag(Channel) -> bool`
  - `sim::Sim { Iracing, Lmu }` (serde lowercase; `as_str()`, `parse(&str)`)
  - `sim::SessionInfo { sim, track_id, track_name, track_config, track_length_m: f32, car_id, car_name, session_type, sector_start_pcts: Vec<f32>, conditions: BTreeMap<String, String> }`
  - `sim::PollResult { NotConnected, Idle, Paused, Session(SessionInfo), Frame(Frame) }`
  - `sim::SimSource` trait: `fn sim(&self) -> Sim; fn poll(&mut self) -> PollResult;`

- [ ] **Step 1: Write the failing tests** at the bottom of `crates/tea-core/src/frame.rs` (create the file with only this for now)

```rust
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
```

Add to `lib.rs` (above the existing test module): `pub mod frame;`

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core frame`
Expected: compile errors, `cannot find type Channel` / `Frame`.

- [ ] **Step 3: Implement `frame.rs`** (above the test module)

```rust
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
```

- [ ] **Step 4: Create `crates/tea-core/src/sim/mod.rs`**

```rust
//! Sim adapters and the types they share.

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
```

Add to `lib.rs`: `pub mod sim;`

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tea-core`
Expected: all pass (`channel_names_are_unique_and_round_trip`, `new_frame_has_every_channel_absent`, `set_get_and_flag`, `sim_round_trips_through_str`, `crate_builds`).

- [ ] **Step 6: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): channel registry, Frame and sim types

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Lap-time math (line crossing and sectors)

**Files:**
- Create: `crates/tea-core/src/laptime.rs`
- Modify: `crates/tea-core/src/lib.rs`

**Interfaces:**
- Consumes: `Frame`, `Channel::LapDistPct` (Task 2).
- Produces:
  - `laptime::line_crossing_time(a: &Frame, b: &Frame) -> f64`: session time at which the car crossed start/finish between consecutive frames `a` (pct ≈ 1) and `b` (pct ≈ 0).
  - `laptime::sector_times_ms(samples: &[Frame], sector_start_pcts: &[f32], lap_start_s: f64, lap_end_s: f64) -> Vec<i64>`: sector durations; empty when sectors are unknown or a split wasn't observed.
  - `laptime::to_ms(seconds: f64) -> i64`

- [ ] **Step 1: Write the failing tests** in a new `crates/tea-core/src/laptime.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Channel, Frame};

    fn f(t: f64, pct: f32) -> Frame {
        let mut fr = Frame::new(t, 1);
        fr.set(Channel::LapDistPct, pct);
        fr
    }

    #[test]
    fn crossing_is_interpolated_between_samples() {
        // 0.0007 of a lap to go before the line, 0.0003 after it: crossing is 70% of the way.
        let t = line_crossing_time(&f(10.0, 0.9993), &f(10.1, 0.0003));
        assert!((t - 10.07).abs() < 1e-6, "got {t}");
    }

    #[test]
    fn crossing_falls_back_to_later_sample_when_not_a_wrap() {
        assert_eq!(line_crossing_time(&f(10.0, 0.4), &f(10.1, 0.6)), 10.1);
        assert_eq!(line_crossing_time(&f(10.0, f32::NAN), &f(10.1, 0.01)), 10.1);
    }

    #[test]
    fn sector_times_from_interpolated_splits() {
        // A 100 s lap at constant speed sampled at 10 Hz, from the line to the line.
        let mut samples = vec![f(-0.05, 0.9995)];
        for i in 0..1000 {
            let t = i as f64 * 0.1 + 0.05;
            samples.push(f(t, (t / 100.0) as f32));
        }
        samples.push(f(100.05, 0.0005));
        let sectors = sector_times_ms(&samples, &[0.0, 0.3, 0.7], 0.0, 100.0);
        assert_eq!(sectors.len(), 3);
        assert!((sectors[0] - 30_000).abs() <= 1, "{sectors:?}");
        assert!((sectors[1] - 40_000).abs() <= 1, "{sectors:?}");
        assert!((sectors[2] - 30_000).abs() <= 1, "{sectors:?}");
    }

    #[test]
    fn sector_times_empty_when_unknown_or_missing() {
        let samples = vec![f(0.0, 0.1), f(1.0, 0.2)];
        assert!(sector_times_ms(&samples, &[], 0.0, 1.0).is_empty());
        // The 0.5 split is never reached in these samples.
        assert!(sector_times_ms(&samples, &[0.0, 0.5], 0.0, 1.0).is_empty());
    }

    #[test]
    fn to_ms_rounds() {
        assert_eq!(to_ms(100.0004), 100_000);
        assert_eq!(to_ms(100.0006), 100_001);
    }
}
```

Add to `lib.rs`: `pub mod laptime;`

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core laptime`
Expected: compile errors, `cannot find function line_crossing_time`.

- [ ] **Step 3: Implement** (above the test module)

```rust
//! Lap-boundary and sector timing from raw samples.

use crate::frame::{Channel, Frame};

pub fn to_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

fn pct(f: &Frame) -> f64 {
    f.get(Channel::LapDistPct) as f64
}

/// Session time at which the car crossed the start/finish line between consecutive
/// frames `a` (lap_dist_pct near 1.0) and `b` (near 0.0), linearly interpolated.
/// Falls back to `b`'s time when the pair doesn't look like a wrap.
pub fn line_crossing_time(a: &Frame, b: &Frame) -> f64 {
    let (pa, pb) = (pct(a), pct(b));
    if !(pa.is_finite() && pb.is_finite()) || pa < 0.5 || pb > 0.5 {
        return b.session_time_s;
    }
    let before = 1.0 - pa;
    let total = before + pb;
    if total <= 0.0 {
        return b.session_time_s;
    }
    a.session_time_s + (before / total) * (b.session_time_s - a.session_time_s)
}

/// Session time at which the car passed `target` between `a` and `b` (no wrap), if it did.
fn pct_crossing_time(a: &Frame, b: &Frame, target: f64) -> Option<f64> {
    let (pa, pb) = (pct(a), pct(b));
    if pa.is_finite() && pb.is_finite() && pb > pa && pa < target && target <= pb {
        Some(a.session_time_s + (target - pa) / (pb - pa) * (b.session_time_s - a.session_time_s))
    } else {
        None
    }
}

/// Sector durations in ms for one lap.
///
/// `samples` may include the boundary frames either side of the line; `lap_start_s` and
/// `lap_end_s` are the line-crossing times. Returns an empty Vec when sectors are unknown
/// or any split wasn't observed (e.g. after a reset).
pub fn sector_times_ms(
    samples: &[Frame],
    sector_start_pcts: &[f32],
    lap_start_s: f64,
    lap_end_s: f64,
) -> Vec<i64> {
    if sector_start_pcts.is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<f64> = sector_start_pcts
        .iter()
        .map(|p| *p as f64)
        .filter(|p| *p > 0.0)
        .collect();
    starts.sort_by(|a, b| a.total_cmp(b));

    let mut splits = vec![lap_start_s];
    for target in starts {
        match samples.windows(2).find_map(|w| pct_crossing_time(&w[0], &w[1], target)) {
            Some(t) => splits.push(t),
            None => return Vec::new(),
        }
    }
    splits.push(lap_end_s);
    splits.windows(2).map(|w| to_ms(w[1] - w[0])).collect()
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tea-core laptime`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): line-crossing and sector timing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Recorder state machine

**Files:**
- Create: `crates/tea-core/src/recorder.rs`, `crates/tea-core/src/testutil.rs`
- Modify: `crates/tea-core/src/lib.rs`

**Interfaces:**
- Consumes: `Frame`, `Channel`, `PollResult`, `SessionInfo` (Task 2); `line_crossing_time`, `sector_times_ms`, `to_ms` (Task 3).
- Produces:
  - `recorder::InvalidReason { Reset, OutLap, InLap, OffTrack }` with `as_str() -> &'static str` (`"reset" | "out_lap" | "in_lap" | "off_track"`)
  - `recorder::CompletedLap { lap_id: String, session_id: String, session_started_at_ms: i64, session: SessionInfo, lap_number: i32, lap_time_ms: i64, lap_start_s: f64, invalid_reason: Option<InvalidReason>, fuel_start_l: Option<f32>, fuel_used_l: Option<f32>, sector_times_ms: Vec<i64>, samples: Vec<Frame> }` with `valid() -> bool`. `samples` = [last frame before the start line, the lap's frames…, first frame after the finish line].
  - `recorder::Recorder::new()`, `handle(&mut self, event: PollResult, now_ms: i64) -> Vec<CompletedLap>`, `current_lap() -> Option<i32>`, `is_recording() -> bool`
  - `recorder::SIM_LAP_TIME_WAIT_FRAMES: u32` (= 90)
  - `testutil` (test-only): `session() -> SessionInfo`, `drive(start_lap: i32, start_pct: f64, duration_s: f64) -> Vec<Frame>`, `completed_lap() -> CompletedLap`, constants `LAP_TIME_S = 100.0`, `DT = 0.1`, `T0 = 1000.0`

**Behaviour rules (from spec §4):** a lap closes when `frame.lap` increments by exactly 1. A lap whose start line wasn't observed is never recorded. `Idle`/`NotConnected` abandon the lap in progress. `Paused` changes nothing. A new `Session` abandons the lap. After closing, wait up to 90 frames for the sim's own lap time; accept it only if it changed and is within 1.0 s of the computed time. Invalid reason priority: reset > out_lap > in_lap > off_track.

- [ ] **Step 1: Create `crates/tea-core/src/testutil.rs`**

```rust
//! Synthetic sessions and laps shared by unit tests.

use crate::frame::{Channel, Frame};
use crate::recorder::{CompletedLap, Recorder};
use crate::sim::{PollResult, SessionInfo, Sim};
use std::collections::BTreeMap;

pub const LAP_TIME_S: f64 = 100.0;
pub const DT: f64 = 0.1;
pub const T0: f64 = 1000.0;
const TRACK_M: f64 = 7004.0;

pub fn session() -> SessionInfo {
    SessionInfo {
        sim: Sim::Iracing,
        track_id: "525".into(),
        track_name: "Circuit de Spa-Francorchamps".into(),
        track_config: "Grand Prix".into(),
        track_length_m: TRACK_M as f32,
        car_id: "170".into(),
        car_name: "Porsche 963 GTP".into(),
        session_type: "Practice".into(),
        sector_start_pcts: vec![0.0, 0.3, 0.7],
        conditions: BTreeMap::from([("air_temp".to_string(), "22.40 C".to_string())]),
    }
}

/// Frames at 10 Hz for a car lapping in exactly 100 s at constant speed, starting at
/// `start_pct` of lap `start_lap` at session time 1000 s, for `duration_s` seconds.
/// Fuel drops 0.02 L/s. With `start_pct = 0.5553` no sample lands exactly on the line.
pub fn drive(start_lap: i32, start_pct: f64, duration_s: f64) -> Vec<Frame> {
    let n = (duration_s / DT).round() as usize;
    (0..=n)
        .map(|i| {
            let t = i as f64 * DT;
            let dist = start_pct + t / LAP_TIME_S;
            let mut f = Frame::new(T0 + t, start_lap + dist.floor() as i32);
            f.set(Channel::LapDistPct, dist.fract() as f32);
            f.set(Channel::LapDistM, (dist.fract() * TRACK_M) as f32);
            f.set(Channel::SpeedMs, 70.0);
            f.set(Channel::FuelL, (60.0 - t * 0.02) as f32);
            f.set(Channel::OnPitRoad, 0.0);
            f.set(Channel::OffTrack, 0.0);
            f
        })
        .collect()
}

/// One clean 100 s lap (lap 2 of a Spa practice session) produced by the real recorder.
pub fn completed_lap() -> CompletedLap {
    let mut rec = Recorder::new();
    let mut events = vec![PollResult::Session(session())];
    events.extend(drive(1, 0.5553, 160.0).into_iter().map(PollResult::Frame));
    events.push(PollResult::Idle);
    let mut laps: Vec<CompletedLap> = events
        .into_iter()
        .flat_map(|e| rec.handle(e, 1_700_000_000_000))
        .collect();
    assert_eq!(laps.len(), 1, "fixture should yield exactly one lap");
    laps.remove(0)
}
```

Add to `lib.rs`: `pub mod recorder;` and `#[cfg(test)] mod testutil;`

- [ ] **Step 2: Write the failing tests** in a new `crates/tea-core/src/recorder.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{drive, session, T0};

    fn run(rec: &mut Recorder, events: Vec<PollResult>) -> Vec<CompletedLap> {
        events.into_iter().flat_map(|e| rec.handle(e, 42)).collect()
    }

    fn with_session(frames: Vec<Frame>, tail: PollResult) -> Vec<PollResult> {
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(frames.into_iter().map(PollResult::Frame));
        ev.push(tail);
        ev
    }

    #[test]
    fn records_full_laps_and_discards_partial_first_lap() {
        // Joins lap 1 at 55.53% (never saw its start) and crosses the line at
        // t=1044.47, 1144.47 and 1244.47; goes idle at t=1250 in the middle of lap 4.
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 250.0), PollResult::Idle));
        let numbers: Vec<i32> = laps.iter().map(|l| l.lap_number).collect();
        assert_eq!(numbers, vec![2, 3]);
        for lap in &laps {
            assert_eq!(lap.lap_time_ms, 100_000, "lap {}", lap.lap_number);
            assert!(lap.valid());
            assert_eq!(lap.session_started_at_ms, 42);
            assert_eq!(lap.session.track_name, "Circuit de Spa-Francorchamps");
        }
        assert!((laps[0].lap_start_s - (T0 + 44.47)).abs() < 1e-3);
        assert_ne!(laps[0].lap_id, laps[1].lap_id);
        assert_eq!(laps[0].session_id, laps[1].session_id);
    }

    #[test]
    fn samples_include_boundary_frames() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 160.0), PollResult::Idle));
        let s = &laps[0].samples;
        assert!(s.first().unwrap().get(Channel::LapDistPct) > 0.99);
        assert!(s.last().unwrap().get(Channel::LapDistPct) < 0.01);
        assert_eq!(s.len(), 1002); // 1000 in-lap frames + 2 boundary frames
    }

    #[test]
    fn computes_fuel_and_sectors() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 160.0), PollResult::Idle));
        let lap = &laps[0];
        assert_eq!(lap.sector_times_ms, vec![30_000, 40_000, 30_000]);
        assert!((lap.fuel_used_l.unwrap() - 2.0).abs() < 0.01, "{:?}", lap.fuel_used_l);
        assert!(lap.fuel_start_l.unwrap() > 59.0);
    }

    #[test]
    fn uses_sim_lap_time_when_published() {
        let mut frames = drive(1, 0.5553, 160.0);
        for f in &mut frames {
            // The old value until 0.5 s after the crossing at t=1144.47, then the new one.
            f.sim_last_lap_time_s = Some(if f.session_time_s >= T0 + 145.0 { 100.2 } else { 99.0 });
        }
        let laps = run(&mut Recorder::new(), with_session(frames, PollResult::Idle));
        assert_eq!(laps[0].lap_time_ms, 100_200);
    }

    #[test]
    fn ignores_sim_lap_time_far_from_computed() {
        let mut frames = drive(1, 0.5553, 160.0);
        for f in &mut frames {
            f.sim_last_lap_time_s = Some(if f.session_time_s >= T0 + 145.0 { 150.0 } else { 99.0 });
        }
        let laps = run(&mut Recorder::new(), with_session(frames, PollResult::Idle));
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn waits_at_most_90_frames_for_sim_time() {
        // No Idle at the end: lap 2 must still be emitted once the wait expires.
        let frames = drive(1, 0.5553, 144.47 + 0.1 * (SIM_LAP_TIME_WAIT_FRAMES as f64 + 2.0));
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(frames.into_iter().map(PollResult::Frame));
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_number, 2);
    }

    fn lap2_with(edit: impl Fn(&mut Frame)) -> CompletedLap {
        let mut frames = drive(1, 0.5553, 160.0);
        frames.iter_mut().for_each(|f| edit(f));
        run(&mut Recorder::new(), with_session(frames, PollResult::Idle)).remove(0)
    }

    fn at(f: &Frame, from: f64, to: f64) -> bool {
        f.session_time_s >= T0 + from && f.session_time_s <= T0 + to
    }

    #[test]
    fn flags_out_lap() {
        let lap = lap2_with(|f| if at(f, 44.4, 50.0) { f.set(Channel::OnPitRoad, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::OutLap));
        assert!(!lap.valid());
    }

    #[test]
    fn flags_in_lap() {
        let lap = lap2_with(|f| if at(f, 140.0, 144.4) { f.set(Channel::OnPitRoad, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::InLap));
    }

    #[test]
    fn flags_off_track() {
        let lap = lap2_with(|f| if at(f, 79.95, 80.05) { f.set(Channel::OffTrack, 1.0) });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::OffTrack));
    }

    #[test]
    fn flags_reset_on_teleport() {
        let lap = lap2_with(|f| {
            if at(f, 79.95, 80.05) {
                let p = f.get(Channel::LapDistPct);
                f.set(Channel::LapDistPct, p + 0.2);
            }
        });
        assert_eq!(lap.invalid_reason, Some(InvalidReason::Reset));
    }

    #[test]
    fn idle_mid_lap_discards_it() {
        let laps = run(&mut Recorder::new(), with_session(drive(1, 0.5553, 100.0), PollResult::Idle));
        assert!(laps.is_empty());
    }

    #[test]
    fn paused_does_not_split_or_drop_the_lap() {
        let mut ev = vec![PollResult::Session(session())];
        for (i, f) in drive(1, 0.5553, 160.0).into_iter().enumerate() {
            if i % 10 == 0 {
                ev.push(PollResult::Paused);
            }
            ev.push(PollResult::Frame(f));
        }
        ev.push(PollResult::Idle);
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn session_change_discards_lap_in_progress() {
        let frames = drive(1, 0.5553, 250.0);
        let (first, second) = frames.split_at(1000); // split at t=1100, mid lap 2
        let mut race = session();
        race.session_type = "Race".into();
        let mut ev = vec![PollResult::Session(session())];
        ev.extend(first.iter().cloned().map(PollResult::Frame));
        ev.push(PollResult::Session(race));
        ev.extend(second.iter().cloned().map(PollResult::Frame));
        ev.push(PollResult::Idle);
        let laps = run(&mut Recorder::new(), ev);
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_number, 3);
        assert_eq!(laps[0].session.session_type, "Race");
    }

    #[test]
    fn frames_without_a_session_are_ignored() {
        let ev: Vec<PollResult> = drive(1, 0.5553, 160.0).into_iter().map(PollResult::Frame).collect();
        assert!(run(&mut Recorder::new(), ev).is_empty());
    }

    #[test]
    fn reports_current_lap() {
        let mut rec = Recorder::new();
        run(&mut rec, with_session(drive(1, 0.5553, 60.0), PollResult::Paused));
        assert!(rec.is_recording());
        assert_eq!(rec.current_lap(), Some(2));
        rec.handle(PollResult::Idle, 0);
        assert!(!rec.is_recording());
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p tea-core recorder`
Expected: compile errors, `cannot find type Recorder`.

- [ ] **Step 4: Implement** (above the test module in `recorder.rs`)

```rust
//! Turns a stream of [`PollResult`]s into completed laps.

use crate::frame::{Channel, Frame};
use crate::laptime::{line_crossing_time, sector_times_ms, to_ms};
use crate::sim::{PollResult, SessionInfo};

/// Frames to wait after a lap closes for the sim to publish its official lap time.
pub const SIM_LAP_TIME_WAIT_FRAMES: u32 = 90;
/// If the sim's lap time disagrees with ours by more than this, it's stale; use ours.
const SIM_LAP_TIME_TOLERANCE_S: f64 = 1.0;
/// A jump in lap_dist_pct bigger than this in one frame (without a line crossing) is a reset.
const TELEPORT_PCT: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidReason {
    Reset,
    OutLap,
    InLap,
    OffTrack,
}

impl InvalidReason {
    pub fn as_str(self) -> &'static str {
        match self {
            InvalidReason::Reset => "reset",
            InvalidReason::OutLap => "out_lap",
            InvalidReason::InLap => "in_lap",
            InvalidReason::OffTrack => "off_track",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletedLap {
    pub lap_id: String,
    pub session_id: String,
    pub session_started_at_ms: i64,
    pub session: SessionInfo,
    pub lap_number: i32,
    pub lap_time_ms: i64,
    /// Session time of the line crossing that began this lap.
    pub lap_start_s: f64,
    pub invalid_reason: Option<InvalidReason>,
    pub fuel_start_l: Option<f32>,
    pub fuel_used_l: Option<f32>,
    pub sector_times_ms: Vec<i64>,
    /// [last frame before the start line, the lap's frames..., first frame after the finish line]
    pub samples: Vec<Frame>,
}

impl CompletedLap {
    pub fn valid(&self) -> bool {
        self.invalid_reason.is_none()
    }
}

struct ActiveSession {
    id: String,
    started_at_ms: i64,
    info: SessionInfo,
}

struct LapInProgress {
    lap_number: i32,
    samples: Vec<Frame>,
    reset: bool,
}

impl LapInProgress {
    fn starting(lap_number: i32, before_line: Frame, after_line: Frame) -> Self {
        Self { lap_number, samples: vec![before_line, after_line], reset: false }
    }
}

/// A closed lap waiting (briefly) for the sim's official time.
struct PendingLap {
    lap: CompletedLap,
    computed_s: f64,
    sim_time_before: Option<f32>,
    frames_waited: u32,
}

#[derive(Default)]
pub struct Recorder {
    session: Option<ActiveSession>,
    lap: Option<LapInProgress>,
    prev: Option<Frame>,
    pending: Option<PendingLap>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_lap(&self) -> Option<i32> {
        self.lap.as_ref().map(|l| l.lap_number)
    }

    pub fn is_recording(&self) -> bool {
        self.lap.is_some()
    }

    pub fn handle(&mut self, event: PollResult, now_ms: i64) -> Vec<CompletedLap> {
        let mut out = Vec::new();
        match event {
            PollResult::Paused => {}
            PollResult::Idle => self.abandon_lap(&mut out),
            PollResult::NotConnected => {
                self.abandon_lap(&mut out);
                self.session = None;
            }
            PollResult::Session(info) => {
                self.abandon_lap(&mut out);
                self.session = Some(ActiveSession {
                    id: uuid::Uuid::new_v4().to_string(),
                    started_at_ms: now_ms,
                    info,
                });
            }
            PollResult::Frame(f) => self.on_frame(f, &mut out),
        }
        out
    }

    fn abandon_lap(&mut self, out: &mut Vec<CompletedLap>) {
        self.flush_pending(out);
        self.lap = None;
        self.prev = None;
    }

    fn on_frame(&mut self, f: Frame, out: &mut Vec<CompletedLap>) {
        if self.session.is_none() {
            return;
        }
        if let Some(p) = self.pending.as_mut() {
            p.frames_waited += 1;
        }
        self.try_resolve_pending(&f, out);

        let prev = self.prev.take();
        let lap_number = self.lap.as_ref().map(|l| l.lap_number);
        match (lap_number, prev) {
            (Some(n), Some(prev)) if f.lap == n + 1 => {
                let mut finished = self.lap.take().expect("lap in progress");
                finished.samples.push(f.clone());
                self.close_lap(finished, out);
                self.try_resolve_pending(&f, out);
                self.lap = Some(LapInProgress::starting(f.lap, prev, f.clone()));
            }
            // Lap counter jumped or went backwards: the session was reset. Drop the lap.
            (Some(n), _) if f.lap != n => self.lap = None,
            (Some(_), prev) => {
                let lap = self.lap.as_mut().expect("lap in progress");
                if let Some(prev) = &prev {
                    let jump = (f.get(Channel::LapDistPct) - prev.get(Channel::LapDistPct)).abs();
                    if jump > TELEPORT_PCT {
                        lap.reset = true;
                    }
                }
                lap.samples.push(f.clone());
            }
            // First line crossing we've seen: a lap starts here.
            (None, Some(prev)) if f.lap == prev.lap + 1 => {
                self.lap = Some(LapInProgress::starting(f.lap, prev, f.clone()));
            }
            (None, _) => {}
        }
        self.prev = Some(f);
    }

    fn close_lap(&mut self, lap: LapInProgress, out: &mut Vec<CompletedLap>) {
        // Only one lap can wait for the sim's time; an older one gets our computed time.
        self.flush_pending(out);
        let Some(session) = self.session.as_ref() else { return };
        let s = &lap.samples;
        let n = s.len();
        if n < 3 {
            return;
        }
        let start = line_crossing_time(&s[0], &s[1]);
        let end = line_crossing_time(&s[n - 2], &s[n - 1]);
        let computed = end - start;
        if !(computed > 0.0) {
            return;
        }
        let in_lap = &s[1..n - 1];
        let invalid_reason = if lap.reset {
            Some(InvalidReason::Reset)
        } else if in_lap[0].flag(Channel::OnPitRoad) {
            Some(InvalidReason::OutLap)
        } else if in_lap.iter().any(|f| f.flag(Channel::OnPitRoad)) {
            Some(InvalidReason::InLap)
        } else if in_lap.iter().any(|f| f.flag(Channel::OffTrack)) {
            Some(InvalidReason::OffTrack)
        } else {
            None
        };
        let finite = |v: f32| v.is_finite().then_some(v);
        let fuel_start_l = finite(in_lap[0].get(Channel::FuelL));
        let fuel_end = finite(in_lap[in_lap.len() - 1].get(Channel::FuelL));
        let fuel_used_l = match (fuel_start_l, fuel_end) {
            (Some(a), Some(b)) if a >= b => Some(a - b),
            _ => None, // unknown, or refuelled during the lap
        };
        let sector_times_ms = sector_times_ms(s, &session.info.sector_start_pcts, start, end);
        let sim_time_before = s[n - 2].sim_last_lap_time_s;
        let completed = CompletedLap {
            lap_id: uuid::Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            session_started_at_ms: session.started_at_ms,
            session: session.info.clone(),
            lap_number: lap.lap_number,
            lap_time_ms: to_ms(computed),
            lap_start_s: start,
            invalid_reason,
            fuel_start_l,
            fuel_used_l,
            sector_times_ms,
            samples: lap.samples,
        };
        self.pending = Some(PendingLap { lap: completed, computed_s: computed, sim_time_before, frames_waited: 0 });
    }

    fn try_resolve_pending(&mut self, f: &Frame, out: &mut Vec<CompletedLap>) {
        let Some(p) = self.pending.as_mut() else { return };
        let sim_time = f.sim_last_lap_time_s.filter(|t| {
            *t > 0.0
                && p.sim_time_before != Some(*t)
                && ((*t as f64) - p.computed_s).abs() <= SIM_LAP_TIME_TOLERANCE_S
        });
        let timed_out = p.frames_waited >= SIM_LAP_TIME_WAIT_FRAMES;
        if let Some(t) = sim_time {
            let mut p = self.pending.take().expect("pending lap");
            p.lap.lap_time_ms = to_ms(t as f64);
            out.push(p.lap);
        } else if timed_out {
            self.flush_pending(out);
        }
    }

    fn flush_pending(&mut self, out: &mut Vec<CompletedLap>) {
        if let Some(p) = self.pending.take() {
            out.push(p.lap);
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tea-core recorder`
Expected: 15 passed. If `uses_sim_lap_time_when_published` fails with 100199 or 100201, the f32 → f64 conversion is rounding: `100.2f32 as f64` is 100.19999694…, which rounds to 100200 ms. Fix any failure by checking the arithmetic, not by loosening the test.

- [ ] **Step 6: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): lap-splitting recorder state machine

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `.tlap` file format

**Files:**
- Create: `crates/tea-core/src/tlap.rs`
- Modify: `crates/tea-core/src/lib.rs`

**Interfaces:**
- Consumes: `SessionInfo` (Task 2); `testutil::session()` (Task 4, tests only).
- Produces:
  - `tlap::FORMAT_VERSION: u32` (= 1)
  - `tlap::ChannelEntry { name: String, unit: String, offset: u64 }`
  - `tlap::TlapHeader { format_version: u32, lap_id, session_id, session_started_at_ms: i64, session: SessionInfo, lap_number: i32, lap_time_ms: i64, valid: bool, invalid_reason: Option<String>, fuel_start_l: Option<f32>, fuel_used_l: Option<f32>, sector_times_ms: Vec<i64>, created_at_ms: i64, sample_count: u64, channels: Vec<ChannelEntry> }`
  - `tlap::TlapFile { header: TlapHeader, data: Vec<Vec<f32>> }` (`data[i]` belongs to `header.channels[i]`)
  - `tlap::encode(header: &mut TlapHeader, data: &[Vec<f32>]) -> Result<Vec<u8>, TlapError>` (fills `format_version`, `sample_count`, offsets)
  - `tlap::decode(bytes: &[u8]) -> Result<TlapFile, TlapError>`
  - `tlap::TlapError { BadMagic, NewerVersion { found, supported }, Corrupt(String), Invalid(String), Io, Json }`

**Layout:** `zstd( b"TLAP" | u32 LE version | u32 LE header_len | header JSON | channel data )`. Channel data is each channel as `sample_count` little-endian `f32`s, back to back. `offset` is relative to the start of channel data.

- [ ] **Step 1: Write the failing tests** in a new `crates/tea-core/src/tlap.rs`

```rust
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
```

Add to `lib.rs`: `pub mod tlap;`

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core tlap`
Expected: compile errors, `cannot find type TlapHeader`.

- [ ] **Step 3: Implement** (above the test module)

```rust
//! `.tlap`: one lap's telemetry in a portable, self-describing, compressed file.

use crate::sim::SessionInfo;
use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"TLAP";
const PREFIX_LEN: usize = 12;

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
    let raw = zstd::decode_all(bytes).map_err(|_| TlapError::BadMagic)?;
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tea-core tlap`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): .tlap lap file format

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: SQLite store with crash-safe saves and reconcile

**Files:**
- Create: `crates/tea-core/src/store.rs`
- Modify: `crates/tea-core/src/lib.rs`

**Interfaces:**
- Consumes: `CompletedLap` (Task 4), `tlap::{encode, decode, TlapHeader, ChannelEntry}` (Task 5), `Channel` (Task 2), `testutil::completed_lap()` (tests).
- Produces:
  - `store::Store::open(root: &Path) -> anyhow::Result<Store>`: creates `root/laps/`, opens `root/index.db` in WAL mode, migrates to schema v1, and errors on a newer schema.
  - `Store::save_lap(&mut self, lap: &CompletedLap, created_at_ms: i64) -> anyhow::Result<()>`
  - `Store::recent_laps(&self, limit: u32) -> anyhow::Result<Vec<LapSummary>>`
  - `Store::reconcile(&mut self) -> anyhow::Result<ReconcileReport>`
  - `Store::read_lap_file(&self, lap_id: &str) -> anyhow::Result<tlap::TlapFile>`
  - `store::LapSummary { lap_id, session_id, sim, track_name, track_config, car_name, session_type, lap_number: i32, lap_time_ms: i64, valid: bool, invalid_reason: Option<String>, fuel_used_l: Option<f32>, created_at_ms: i64 }` (serde `Serialize`)
  - `store::ReconcileReport { reindexed: u32, missing: u32, temp_removed: u32, unreadable: u32 }`
  - `.tlap` channel list: `t_s` (seconds since lap start; negative for the first boundary sample) first, then every `Channel` that has at least one finite value in the lap, in `Channel::ALL` order.

- [ ] **Step 1: Write the failing tests** in a new `crates/tea-core/src/store.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::completed_lap;
    use tempfile::tempdir;

    #[test]
    fn save_lap_writes_file_and_index_row() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 777).unwrap();

        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 1);
        let s = &laps[0];
        assert_eq!(s.lap_id, lap.lap_id);
        assert_eq!(s.sim, "iracing");
        assert_eq!(s.track_name, "Circuit de Spa-Francorchamps");
        assert_eq!(s.car_name, "Porsche 963 GTP");
        assert_eq!(s.lap_number, 2);
        assert_eq!(s.lap_time_ms, 100_000);
        assert!(s.valid);
        assert_eq!(s.created_at_ms, 777);
        assert!(dir.path().join(format!("laps/{}.tlap", lap.lap_id)).exists());
    }

    #[test]
    fn lap_file_has_time_first_and_only_present_channels() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 1).unwrap();

        let file = store.read_lap_file(&lap.lap_id).unwrap();
        let names: Vec<&str> = file.header.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names[0], "t_s");
        assert!(names.contains(&"lap_dist_pct"));
        assert!(names.contains(&"speed"));
        assert!(!names.contains(&"pos_x"), "absent channels must not be stored");
        assert_eq!(file.header.sample_count as usize, lap.samples.len());
        let t = &file.data[0];
        assert!(t[0] < 0.0 && t[1] >= 0.0, "first boundary sample is before the line");
        assert_eq!(file.header.sector_times_ms, vec![30_000, 40_000, 30_000]);
    }

    #[test]
    fn reconcile_reindexes_orphan_files() {
        let dir = tempdir().unwrap();
        let lap = completed_lap();
        {
            let mut store = Store::open(dir.path()).unwrap();
            store.save_lap(&lap, 1).unwrap();
        }
        // Simulate a crash between writing the file and indexing it: lose the index.
        for f in ["index.db", "index.db-wal", "index.db-shm"] {
            let _ = std::fs::remove_file(dir.path().join(f));
        }
        let mut store = Store::open(dir.path()).unwrap();
        assert!(store.recent_laps(10).unwrap().is_empty());
        let report = store.reconcile().unwrap();
        assert_eq!(report.reindexed, 1);
        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_id, lap.lap_id);
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn reconcile_cleans_temp_and_flags_missing() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 1).unwrap();
        std::fs::write(dir.path().join("laps/half-written.tlap.tmp"), b"partial").unwrap();
        std::fs::remove_file(dir.path().join(format!("laps/{}.tlap", lap.lap_id))).unwrap();

        let report = store.reconcile().unwrap();
        assert_eq!(report.temp_removed, 1);
        assert_eq!(report.missing, 1);
        assert!(!dir.path().join("laps/half-written.tlap.tmp").exists());
        assert!(store.recent_laps(10).unwrap().is_empty(), "missing laps are hidden");
    }

    #[test]
    fn reconcile_skips_unreadable_files_without_deleting_them() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let junk = dir.path().join("laps/not-really.tlap");
        std::fs::write(&junk, b"garbage").unwrap();
        let report = store.reconcile().unwrap();
        assert_eq!(report.unreadable, 1);
        assert!(junk.exists());
    }

    #[test]
    fn refuses_database_from_newer_version() {
        let dir = tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        let conn = Connection::open(dir.path().join("index.db")).unwrap();
        conn.execute("INSERT INTO schema_version (version) VALUES (99)", []).unwrap();
        drop(conn);
        let err = Store::open(dir.path()).err().expect("should refuse").to_string();
        assert!(err.contains("newer version"), "{err}");
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        Store::open(dir.path()).unwrap();
    }
}
```

Add to `lib.rs`: `pub mod store;`

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core store`
Expected: compile errors, `cannot find type Store`.

- [ ] **Step 3: Implement** (above the test module)

```rust
//! The lap library on disk: SQLite index + one `.tlap` file per lap.

use crate::frame::Channel;
use crate::recorder::CompletedLap;
use crate::tlap::{self, ChannelEntry, TlapFile, TlapHeader};
use anyhow::Context;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    sim             TEXT NOT NULL,
    track_id        TEXT NOT NULL,
    track_name      TEXT NOT NULL,
    track_config    TEXT NOT NULL,
    track_length_m  REAL NOT NULL,
    car_id          TEXT NOT NULL,
    car_name        TEXT NOT NULL,
    session_type    TEXT NOT NULL,
    started_at_ms   INTEGER NOT NULL,
    conditions_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS laps (
    id                TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions(id),
    lap_number        INTEGER NOT NULL,
    lap_time_ms       INTEGER NOT NULL,
    valid             INTEGER NOT NULL,
    invalid_reason    TEXT,
    fuel_start_l      REAL,
    fuel_used_l       REAL,
    sector_times_json TEXT NOT NULL,
    file_path         TEXT NOT NULL,
    file_missing      INTEGER NOT NULL DEFAULT 0,
    created_at_ms     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS laps_by_session ON laps(session_id);
CREATE INDEX IF NOT EXISTS laps_by_created ON laps(created_at_ms);
CREATE TABLE IF NOT EXISTS track_cache (
    sim            TEXT NOT NULL,
    track_id       TEXT NOT NULL,
    track_config   TEXT NOT NULL,
    outline_lap_id TEXT,
    outline_blob   BLOB,
    corners_json   TEXT,
    updated_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (sim, track_id, track_config)
);
";

#[derive(Clone, Debug, Serialize)]
pub struct LapSummary {
    pub lap_id: String,
    pub session_id: String,
    pub sim: String,
    pub track_name: String,
    pub track_config: String,
    pub car_name: String,
    pub session_type: String,
    pub lap_number: i32,
    pub lap_time_ms: i64,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub fuel_used_l: Option<f32>,
    pub created_at_ms: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub reindexed: u32,
    pub missing: u32,
    pub temp_removed: u32,
    pub unreadable: u32,
}

pub struct Store {
    root: PathBuf,
    conn: Connection,
}

impl Store {
    pub fn open(root: &Path) -> anyhow::Result<Store> {
        fs::create_dir_all(root.join("laps"))
            .with_context(|| format!("creating {}", root.display()))?;
        let conn = Connection::open(root.join("index.db"))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&conn)?;
        Ok(Store { root: root.to_path_buf(), conn })
    }

    /// Write the lap file crash-safely (temp → fsync → rename), then index it.
    pub fn save_lap(&mut self, lap: &CompletedLap, created_at_ms: i64) -> anyhow::Result<()> {
        let (mut header, data) = lap_to_tlap(lap, created_at_ms);
        let bytes = tlap::encode(&mut header, &data)?;
        let rel = format!("laps/{}.tlap", lap.lap_id);
        let final_path = self.root.join(&rel);
        let tmp_path = self.root.join(format!("laps/{}.tlap.tmp", lap.lap_id));
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        self.index_lap(&header, &rel)
    }

    pub fn read_lap_file(&self, lap_id: &str) -> anyhow::Result<TlapFile> {
        let rel: String = self
            .conn
            .query_row("SELECT file_path FROM laps WHERE id = ?1", [lap_id], |r| r.get(0))
            .with_context(|| format!("lap {lap_id} not found"))?;
        let bytes = fs::read(self.root.join(rel))?;
        Ok(tlap::decode(&bytes)?)
    }

    pub fn recent_laps(&self, limit: u32) -> anyhow::Result<Vec<LapSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.id, l.session_id, s.sim, s.track_name, s.track_config, s.car_name,
                    s.session_type, l.lap_number, l.lap_time_ms, l.valid, l.invalid_reason,
                    l.fuel_used_l, l.created_at_ms
             FROM laps l JOIN sessions s ON s.id = l.session_id
             WHERE l.file_missing = 0
             ORDER BY l.created_at_ms DESC, l.lap_number DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(LapSummary {
                lap_id: r.get(0)?,
                session_id: r.get(1)?,
                sim: r.get(2)?,
                track_name: r.get(3)?,
                track_config: r.get(4)?,
                car_name: r.get(5)?,
                session_type: r.get(6)?,
                lap_number: r.get(7)?,
                lap_time_ms: r.get(8)?,
                valid: r.get(9)?,
                invalid_reason: r.get(10)?,
                fuel_used_l: r.get::<_, Option<f64>>(11)?.map(|v| v as f32),
                created_at_ms: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Bring the index in line with the files on disk after a crash or manual file changes.
    pub fn reconcile(&mut self) -> anyhow::Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let mut on_disk = HashSet::new();
        for entry in fs::read_dir(self.root.join("laps"))? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            if name.ends_with(".tlap.tmp") {
                fs::remove_file(&path)?;
                report.temp_removed += 1;
                continue;
            }
            let Some(id) = name.strip_suffix(".tlap") else { continue };
            on_disk.insert(id.to_string());
            let known: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM laps WHERE id = ?1)",
                [id],
                |r| r.get(0),
            )?;
            if known {
                self.conn.execute("UPDATE laps SET file_missing = 0 WHERE id = ?1", [id])?;
                continue;
            }
            let decoded = fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(tlap::decode(&b)?));
            match decoded {
                Ok(file) if file.header.lap_id == id => {
                    self.index_lap(&file.header, &format!("laps/{name}"))?;
                    report.reindexed += 1;
                }
                Ok(_) => {
                    log::warn!("{name}: lap id in header doesn't match file name; skipped");
                    report.unreadable += 1;
                }
                Err(e) => {
                    log::warn!("{name}: {e:#}; skipped");
                    report.unreadable += 1;
                }
            }
        }
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT id FROM laps WHERE file_missing = 0")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in ids.into_iter().filter(|id| !on_disk.contains(id)) {
            self.conn.execute("UPDATE laps SET file_missing = 1 WHERE id = ?1", [&id])?;
            report.missing += 1;
        }
        Ok(report)
    }

    fn index_lap(&mut self, h: &TlapHeader, rel_path: &str) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sessions (id, sim, track_id, track_name, track_config,
                 track_length_m, car_id, car_name, session_type, started_at_ms, conditions_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                h.session_id,
                h.session.sim.as_str(),
                h.session.track_id,
                h.session.track_name,
                h.session.track_config,
                h.session.track_length_m as f64,
                h.session.car_id,
                h.session.car_name,
                h.session.session_type,
                h.session_started_at_ms,
                serde_json::to_string(&h.session.conditions)?,
            ],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO laps (id, session_id, lap_number, lap_time_ms, valid,
                 invalid_reason, fuel_start_l, fuel_used_l, sector_times_json, file_path,
                 file_missing, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
            params![
                h.lap_id,
                h.session_id,
                h.lap_number,
                h.lap_time_ms,
                h.valid,
                h.invalid_reason,
                h.fuel_start_l.map(|v| v as f64),
                h.fuel_used_l.map(|v| v as f64),
                serde_json::to_string(&h.sector_times_ms)?,
                rel_path,
                h.created_at_ms,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;
    let version: Option<i64> =
        tx.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))?;
    match version {
        None => {
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?1)", [SCHEMA_VERSION])?;
        }
        Some(v) if v == SCHEMA_VERSION => {}
        Some(v) if v > SCHEMA_VERSION => anyhow::bail!(
            "index.db was created by a newer version of Tea Telemetry (schema {v}); please update the app"
        ),
        Some(v) => anyhow::bail!("unsupported index.db schema version {v}"),
    }
    tx.commit()?;
    Ok(())
}

fn lap_to_tlap(lap: &CompletedLap, created_at_ms: i64) -> (TlapHeader, Vec<Vec<f32>>) {
    let mut channels = vec![ChannelEntry { name: "t_s".into(), unit: "s".into(), offset: 0 }];
    let mut data: Vec<Vec<f32>> = vec![lap
        .samples
        .iter()
        .map(|f| (f.session_time_s - lap.lap_start_s) as f32)
        .collect()];
    for &ch in Channel::ALL {
        if lap.samples.iter().any(|f| f.get(ch).is_finite()) {
            channels.push(ChannelEntry { name: ch.name().into(), unit: ch.unit().into(), offset: 0 });
            data.push(lap.samples.iter().map(|f| f.get(ch)).collect());
        }
    }
    let header = TlapHeader {
        format_version: tlap::FORMAT_VERSION,
        lap_id: lap.lap_id.clone(),
        session_id: lap.session_id.clone(),
        session_started_at_ms: lap.session_started_at_ms,
        session: lap.session.clone(),
        lap_number: lap.lap_number,
        lap_time_ms: lap.lap_time_ms,
        valid: lap.valid(),
        invalid_reason: lap.invalid_reason.map(|r| r.as_str().to_string()),
        fuel_start_l: lap.fuel_start_l,
        fuel_used_l: lap.fuel_used_l,
        sector_times_ms: lap.sector_times_ms.clone(),
        created_at_ms,
        sample_count: 0,
        channels,
    };
    (header, data)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tea-core store`
Expected: 7 passed. If `pragma_update_and_check` or `Transaction::new_unchecked` don't compile, check the rusqlite 0.32 docs for the exact signature. Keep the behaviour (WAL on; IMMEDIATE migration transaction).

- [ ] **Step 5: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): SQLite lap store with crash-safe saves and reconcile

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Raw capture and ReplaySource

**Files:**
- Create: `crates/tea-core/src/sim/replay.rs`
- Modify: `crates/tea-core/src/sim/mod.rs` (add `pub mod replay;`)

**Interfaces:**
- Consumes: `PollResult`, `Sim`, `SimSource` (Task 2); `testutil::{session, drive}` (tests).
- Produces:
  - `replay::CaptureWriter::create(path: &Path, sim: Sim) -> anyhow::Result<CaptureWriter>`, `write(&mut self, ev: &PollResult) -> anyhow::Result<()>`. It finishes the zstd stream when dropped.
  - `replay::ReplaySource::open(path: &Path, realtime: bool) -> anyhow::Result<ReplaySource>`
  - `replay::ReplaySource::from_events(sim: Sim, events: Vec<PollResult>, realtime: bool) -> ReplaySource`
  - `ReplaySource: SimSource`. It yields events in order, then `NotConnected` forever. In `realtime` mode it sleeps between frames by the session-time difference (capped at 0.5 s).

**Capture file (`.tcap`):** `zstd( bincode(CaptureHeader{version: 1, sim}) , bincode(PollResult)* )`.

- [ ] **Step 1: Write the failing tests** in a new `crates/tea-core/src/sim/replay.rs`

```rust
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
}
```

Add `pub mod replay;` at the top of `sim/mod.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core replay`
Expected: compile errors, `cannot find type CaptureWriter`.

- [ ] **Step 3: Implement** (above the test module)

```rust
//! Raw capture of adapter output, and a [`SimSource`] that plays it back.
//! Used for tests, fixtures and the "mock sim" dev mode.

use super::{PollResult, Sim, SimSource};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::Duration;

const CAPTURE_VERSION: u32 = 1;
const MAX_REALTIME_GAP_S: f64 = 0.5;
const NON_FRAME_DELAY: Duration = Duration::from_millis(16);

#[derive(Serialize, Deserialize)]
struct CaptureHeader {
    version: u32,
    sim: Sim,
}

pub struct CaptureWriter {
    enc: zstd::stream::write::AutoFinishEncoder<'static, BufWriter<File>>,
}

impl CaptureWriter {
    pub fn create(path: &Path, sim: Sim) -> anyhow::Result<Self> {
        let file = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
        let mut enc = zstd::stream::write::Encoder::new(file, 3)?.auto_finish();
        bincode::serialize_into(&mut enc, &CaptureHeader { version: CAPTURE_VERSION, sim })?;
        Ok(Self { enc })
    }

    pub fn write(&mut self, ev: &PollResult) -> anyhow::Result<()> {
        bincode::serialize_into(&mut self.enc, ev)?;
        Ok(())
    }
}

pub struct ReplaySource {
    sim: Sim,
    events: Box<dyn Iterator<Item = PollResult>>,
    realtime: bool,
    last_time_s: Option<f64>,
}

impl ReplaySource {
    pub fn open(path: &Path, realtime: bool) -> anyhow::Result<Self> {
        let file = BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
        let mut dec = zstd::stream::read::Decoder::with_buffer(file)?;
        let header: CaptureHeader =
            bincode::deserialize_from(&mut dec).context("not a Tea Telemetry capture file")?;
        if header.version > CAPTURE_VERSION {
            anyhow::bail!("capture was made by a newer version of Tea Telemetry");
        }
        // A truncated capture (app killed while capturing) simply ends early.
        let events = std::iter::from_fn(move || bincode::deserialize_from::<_, PollResult>(&mut dec).ok());
        Ok(Self { sim: header.sim, events: Box::new(events), realtime, last_time_s: None })
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tea-core replay`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): raw capture writer and replay source

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: iRacing shared-memory layout parsing

**Files:**
- Create: `crates/tea-core/src/sim/iracing/mod.rs`, `crates/tea-core/src/sim/iracing/layout.rs`, `crates/tea-core/src/sim/iracing/testimage.rs`
- Modify: `crates/tea-core/src/sim/mod.rs` (add `pub mod iracing;`)

**Interfaces:**
- Consumes: nothing beyond std.
- Produces:
  - `layout::{HEADER_LEN = 112, VAR_HEADER_LEN = 144, MAX_BUFS = 4}`
  - `layout::VarType { Char, Bool, Int, BitField, Float, Double }` with `size() -> usize`, `code() -> i32`, `from_code(i32) -> Option<VarType>`
  - `layout::VarBuf { tick_count: i32, buf_offset: i32 }`
  - `layout::IrHeader { ver, status, tick_rate, session_info_update, session_info_len, session_info_offset, num_vars, var_header_offset, num_buf, buf_len: i32, var_bufs: [VarBuf; 4] }` with `parse(&[u8]) -> Result<IrHeader, LayoutError>`, `is_connected() -> bool`, `latest_buf() -> Option<VarBuf>`
  - `layout::VarHeader { var_type: VarType, offset: usize, count: usize, name: String, unit: String }`
  - `layout::parse_var_headers(bytes: &[u8], num_vars: usize) -> Result<Vec<VarHeader>, LayoutError>`
  - `layout::read_value(buf: &[u8], var: &VarHeader, index: usize) -> Option<f64>`
  - `layout::LayoutError { TooShort { need, have }, BadType(i32) }`
  - `testimage::ImageBuilder` (test-only): `new()`, `var(self, name, VarType) -> Self`, `set(&mut self, name, f64) -> &mut Self`, pub fields `yaml: String`, `tick: i32`, `status: i32`, `session_info_update: i32`, `build(&self) -> Vec<u8>`. The image holds 3 buffers; slot 1 has the newest tick and the real values.

**irsdk layout (from `irsdk_defines.h`, little-endian):**
- **Header, 112 bytes:** `ver@0 status@4 tickRate@8 sessionInfoUpdate@12 sessionInfoLen@16 sessionInfoOffset@20 numVars@24 varHeaderOffset@28 numBuf@32 bufLen@36 pad@40..48`, then `varBuf[4]` at `48 + 16*i` = `{tickCount@+0, bufOffset@+4, pad@+8}`.
- **Var header, 144 bytes:** `type@0 offset@4 count@8 countAsTime@12(u8) pad@13 name@16[32] desc@48[64] unit@112[32]`.
- **Type codes:** char=0, bool=1, int=2, bitField=3, float=4, double=5, with sizes 1, 1, 4, 4, 4, 8.

- [ ] **Step 1: Create `sim/iracing/mod.rs`**

```rust
//! iRacing adapter: reads the irsdk shared-memory telemetry.

pub mod layout;
#[cfg(test)]
pub(crate) mod testimage;
```

and add `pub mod iracing;` to `sim/mod.rs`.

- [ ] **Step 2: Create `sim/iracing/testimage.rs`**

```rust
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
```

- [ ] **Step 3: Write the failing tests** in a new `sim/iracing/layout.rs`

```rust
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
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p tea-core layout`
Expected: compile errors, `cannot find type IrHeader`.

- [ ] **Step 5: Implement** (above the test module in `layout.rs`)

```rust
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
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p tea-core layout`
Expected: 4 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/tea-core
git commit -m "feat(iracing): irsdk shared-memory layout parsing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: iRacing session-info YAML

**Files:**
- Create: `crates/tea-core/src/sim/iracing/yaml.rs`
- Modify: `crates/tea-core/src/sim/iracing/mod.rs` (add `pub mod yaml;`), `crates/tea-core/src/sim/iracing/testimage.rs` (add `SAMPLE_YAML`)

**Interfaces:**
- Consumes: `SessionInfo`, `Sim` (Task 2).
- Produces:
  - `yaml::Node { Scalar(String), Map(Vec<(String, Node)>), List(Vec<Node>) }` with `get(&str) -> Option<&Node>`, `path(&[&str]) -> Option<&Node>`, `str() -> Option<&str>`, `list() -> &[Node]`
  - `yaml::parse(src: &str) -> Node`: parses iRacing's YAML subset and never panics.
  - `yaml::ParsedSession { info: SessionInfo, key: String }`. `key` identifies the session; a different key means a new session.
  - `yaml::session_from_yaml(yaml: &str, session_num: i32) -> ParsedSession`
  - `testimage::SAMPLE_YAML: &str` (test-only)

**Why a hand parser:** iRacing's session string is not always valid YAML (unescaped special characters in names), and we need only a dozen fields. The format is regular: `key: value` maps indented by spaces, and list items (`- key: value`) at the same indent as their parent key.

- [ ] **Step 1: Add `SAMPLE_YAML` to `testimage.rs`**

```rust
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
```

- [ ] **Step 2: Write the failing tests** in a new `sim/iracing/yaml.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::iracing::testimage::SAMPLE_YAML;

    #[test]
    fn extracts_session_info() {
        let p = session_from_yaml(SAMPLE_YAML, 0);
        let i = &p.info;
        assert_eq!(i.sim, Sim::Iracing);
        assert_eq!(i.track_id, "525");
        assert_eq!(i.track_name, "Circuit de Spa-Francorchamps");
        assert_eq!(i.track_config, "Grand Prix");
        assert!((i.track_length_m - 6930.0).abs() < 0.5);
        assert_eq!(i.car_id, "170");
        assert_eq!(i.car_name, "Porsche 963 GTP");
        assert_eq!(i.session_type, "Practice");
        assert_eq!(i.sector_start_pcts, vec![0.0, 0.33, 0.66]);
        assert_eq!(i.conditions.get("air_temp").map(String::as_str), Some("22.40 C"));
        assert_eq!(i.conditions.get("track_temp").map(String::as_str), Some("31.20 C"));
    }

    #[test]
    fn session_type_follows_session_num() {
        assert_eq!(session_from_yaml(SAMPLE_YAML, 1).info.session_type, "Race");
        assert_eq!(session_from_yaml(SAMPLE_YAML, 7).info.session_type, "Unknown");
    }

    #[test]
    fn key_changes_with_session_num_car_or_track() {
        let a = session_from_yaml(SAMPLE_YAML, 0).key;
        assert_eq!(a, session_from_yaml(SAMPLE_YAML, 0).key);
        assert_ne!(a, session_from_yaml(SAMPLE_YAML, 1).key);
        let other_car = SAMPLE_YAML.replace("CarID: 170", "CarID: 171");
        assert_ne!(a, session_from_yaml(&other_car, 0).key);
    }

    #[test]
    fn nested_lists_stay_nested() {
        let root = parse(SAMPLE_YAML);
        let sessions = root.path(&["SessionInfo", "Sessions"]).unwrap().list();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].get("ResultsPositions").unwrap().list().len(), 2);
        assert_eq!(
            root.path(&["WeekendInfo", "WeekendOptions", "StartingGrid"]).and_then(Node::str),
            Some("single file")
        );
    }

    #[test]
    fn values_with_colons_and_quotes() {
        let root = parse(SAMPLE_YAML);
        let drivers = root.path(&["DriverInfo", "Drivers"]).unwrap().list();
        assert_eq!(drivers[1].get("UserName").and_then(Node::str), Some("Test Driver: One"));
    }

    #[test]
    fn missing_sections_give_defaults() {
        let p = session_from_yaml("---\nWeekendInfo:\n TrackName: x\n", 0);
        assert_eq!(p.info.track_name, "x"); // falls back to TrackName
        assert_eq!(p.info.track_length_m, 0.0);
        assert_eq!(p.info.car_name, "");
        assert_eq!(p.info.session_type, "Unknown");
        assert!(p.info.sector_start_pcts.is_empty());
        // Total garbage must not panic either.
        let _ = session_from_yaml("::::\n  - - -\n\t\u{0}", 0);
        let _ = parse("");
    }
}
```

Add `pub mod yaml;` to `sim/iracing/mod.rs`.

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p tea-core yaml`
Expected: compile errors, `cannot find function session_from_yaml`.

- [ ] **Step 4: Implement** (above the test module)

```rust
//! A small parser for iRacing's session-info YAML subset, plus extraction of [`SessionInfo`].

use crate::sim::{SessionInfo, Sim};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Scalar(String),
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Node> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn list(&self) -> &[Node] {
        match self {
            Node::List(items) => items,
            _ => &[],
        }
    }
}

struct Line {
    /// Column of the `-` if this line starts a list item.
    dash_indent: Option<usize>,
    /// Column where the key starts.
    key_indent: usize,
    key: String,
    value: String,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

fn tokenize(src: &str) -> Vec<Line> {
    src.lines()
        .filter_map(|raw| {
            let line = raw.trim_end();
            let indent = line.len() - line.trim_start().len();
            let mut rest = line.trim_start();
            if rest.is_empty() || rest == "---" || rest == "..." || rest.starts_with('#') {
                return None;
            }
            let mut dash_indent = None;
            let mut key_indent = indent;
            if let Some(after) = rest.strip_prefix("- ") {
                let inner = after.trim_start();
                dash_indent = Some(indent);
                key_indent = indent + 2 + (after.len() - inner.len());
                rest = inner;
            }
            let (key, value) = rest.split_once(':')?;
            Some(Line { dash_indent, key_indent, key: key.trim().to_string(), value: unquote(value) })
        })
        .collect()
}

fn parse_map(lines: &[Line], pos: &mut usize, indent: usize, mut item_head: bool) -> Vec<(String, Node)> {
    let mut entries = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if item_head {
            item_head = false; // the "- key: value" line that opens a list item
        } else if line.dash_indent.is_some() || line.key_indent != indent {
            if line.dash_indent.is_none() && line.key_indent > indent {
                *pos += 1; // stray over-indented line: skip it
                continue;
            }
            break;
        }
        *pos += 1;
        let node = if !line.value.is_empty() {
            Node::Scalar(line.value.clone())
        } else {
            match lines.get(*pos) {
                Some(next) if next.dash_indent.is_some_and(|d| d >= indent) => {
                    let d = next.dash_indent.expect("checked");
                    Node::List(parse_list(lines, pos, d))
                }
                Some(next) if next.dash_indent.is_none() && next.key_indent > indent => {
                    let child_indent = next.key_indent;
                    Node::Map(parse_map(lines, pos, child_indent, false))
                }
                _ => Node::Scalar(String::new()),
            }
        };
        entries.push((line.key.clone(), node));
    }
    entries
}

fn parse_list(lines: &[Line], pos: &mut usize, dash_indent: usize) -> Vec<Node> {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if line.dash_indent != Some(dash_indent) {
            break;
        }
        let key_indent = line.key_indent;
        items.push(Node::Map(parse_map(lines, pos, key_indent, true)));
    }
    items
}

/// Parse iRacing's YAML subset. Never panics; unparseable lines are skipped.
pub fn parse(src: &str) -> Node {
    let lines = tokenize(src);
    let mut pos = 0;
    let mut root = Vec::new();
    while pos < lines.len() {
        let before = pos;
        let indent = lines[pos].key_indent;
        root.extend(parse_map(&lines, &mut pos, indent, lines[pos].dash_indent.is_some()));
        if pos == before {
            pos += 1;
        }
    }
    Node::Map(root)
}

pub struct ParsedSession {
    pub info: SessionInfo,
    /// Identity of the session; a change means a new session.
    pub key: String,
}

fn parse_track_length_m(s: &str) -> f32 {
    let mut parts = s.split_whitespace();
    let value: f64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let metres = match parts.next() {
        Some("mi") => value * 1609.344,
        Some("m") => value,
        _ => value * 1000.0, // iRacing reports km
    };
    metres as f32
}

pub fn session_from_yaml(yaml: &str, session_num: i32) -> ParsedSession {
    let root = parse(yaml);
    let weekend = |key: &str| {
        root.path(&["WeekendInfo", key]).and_then(Node::str).unwrap_or("").to_string()
    };

    let driver_idx = root.path(&["DriverInfo", "DriverCarIdx"]).and_then(Node::str).unwrap_or("");
    let driver = root
        .path(&["DriverInfo", "Drivers"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .find(|d| d.get("CarIdx").and_then(Node::str) == Some(driver_idx));
    let car = |key: &str| driver.and_then(|d| d.get(key)).and_then(Node::str).unwrap_or("").to_string();

    let num = session_num.to_string();
    let session_type = root
        .path(&["SessionInfo", "Sessions"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .find(|s| s.get("SessionNum").and_then(Node::str) == Some(num.as_str()))
        .and_then(|s| s.get("SessionType"))
        .and_then(Node::str)
        .unwrap_or("Unknown")
        .to_string();

    let mut sector_start_pcts: Vec<f32> = root
        .path(&["SplitTimeInfo", "Sectors"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|s| s.get("SectorStartPct")?.str()?.parse().ok())
        .collect();
    sector_start_pcts.sort_by(|a, b| a.total_cmp(b));

    let mut conditions = BTreeMap::new();
    for (yaml_key, name) in [
        ("TrackAirTemp", "air_temp"),
        ("TrackSurfaceTemp", "track_temp"),
        ("TrackSkies", "skies"),
        ("TrackWeatherType", "weather"),
    ] {
        let v = weekend(yaml_key);
        if !v.is_empty() {
            conditions.insert(name.to_string(), v);
        }
    }

    let display = weekend("TrackDisplayName");
    let info = SessionInfo {
        sim: Sim::Iracing,
        track_id: weekend("TrackID"),
        track_name: if display.is_empty() { weekend("TrackName") } else { display },
        track_config: weekend("TrackConfigName"),
        track_length_m: parse_track_length_m(&weekend("TrackLength")),
        car_id: car("CarID"),
        car_name: car("CarScreenName"),
        session_type,
        sector_start_pcts,
        conditions,
    };
    let key = format!(
        "{}/{}/{}/{}/{}",
        weekend("SubSessionID"),
        session_num,
        info.track_id,
        info.track_config,
        info.car_id
    );
    ParsedSession { info, key }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tea-core yaml`
Expected: 6 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/tea-core
git commit -m "feat(iracing): session-info YAML parsing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: iRacing source (var mapping, poll logic, Win32 memory)

**Files:**
- Create: `crates/tea-core/src/sim/iracing/mapping.rs`, `crates/tea-core/src/sim/iracing/source.rs`, `crates/tea-core/src/sim/iracing/win.rs`
- Modify: `crates/tea-core/src/sim/iracing/mod.rs`

**Interfaces:**
- Consumes: `layout::*` (Task 8), `yaml::session_from_yaml` (Task 9), `Frame`/`Channel` (Task 2), `PollResult`/`SimSource`/`Sim` (Task 2), `testimage::{ImageBuilder, SAMPLE_YAML}` (tests).
- Produces:
  - `mapping::VarMap::resolve(vars: &[VarHeader]) -> VarMap` (+ `Default`), `read(&self, buf: &[u8]) -> Option<(Frame, Control)>` (None when `SessionTime` is missing)
  - `mapping::Control { is_replay: bool, is_on_track: bool, session_num: i32 }`
  - `source::SharedMem` trait: `read(&self, offset: usize, len: usize) -> Option<Vec<u8>>`, `wait_for_data(&self, timeout_ms: u32)`
  - `source::IracingSource<M: SharedMem>::new(connect: Box<dyn FnMut() -> Option<M>>) -> Self`, implementing `SimSource`
  - `win::WinSharedMem` (Windows only), `win::connect() -> Option<WinSharedMem>`
  - `iracing::live_source() -> IracingSource<win::WinSharedMem>`

**Poll rules:** no mapping → `NotConnected`. Header status not connected → drop the connection, `NotConnected`. No new tick → `Paused`. If the tick changed while copying (torn read) → `Paused`, sample skipped. Session YAML identity changed or `SessionNum` changed → `Session(info)`, and the same sample is delivered on the next poll. `IsReplayPlaying` or `!IsOnTrack` → `Idle`. Otherwise → `Frame`. `Clutch` is inverted (iRacing reports 1 = fully engaged; we store pedal travel). `PlayerTrackSurface == 0` (irsdk `OffTrack`) → `off_track = 1`.

- [ ] **Step 1: Create `mapping.rs`**

```rust
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
```

- [ ] **Step 2: Write the failing tests** in a new `source.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Channel;
    use crate::sim::iracing::layout::VarType;
    use crate::sim::iracing::testimage::{ImageBuilder, SAMPLE_YAML};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct FakeMem(Rc<RefCell<Vec<u8>>>);

    impl SharedMem for FakeMem {
        fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
            self.0.borrow().get(offset..offset + len).map(|s| s.to_vec())
        }
        fn wait_for_data(&self, _timeout_ms: u32) {}
    }

    struct Rig {
        src: IracingSource<FakeMem>,
        b: ImageBuilder,
        img: Rc<RefCell<Vec<u8>>>,
        running: Rc<Cell<bool>>,
    }

    impl Rig {
        fn new() -> Rig {
            let mut b = ImageBuilder::new()
                .var("SessionTime", VarType::Double)
                .var("Lap", VarType::Int)
                .var("LapLastLapTime", VarType::Float)
                .var("LapDistPct", VarType::Float)
                .var("Speed", VarType::Float)
                .var("Clutch", VarType::Float)
                .var("PlayerTrackSurface", VarType::Int)
                .var("IsReplayPlaying", VarType::Bool)
                .var("IsOnTrack", VarType::Bool)
                .var("SessionNum", VarType::Int)
                .var("OnPitRoad", VarType::Bool);
            b.yaml = SAMPLE_YAML.to_string();
            b.set("SessionTime", 1234.5)
                .set("Lap", 3.0)
                .set("LapLastLapTime", -1.0)
                .set("LapDistPct", 0.25)
                .set("Speed", 55.5)
                .set("Clutch", 0.25)
                .set("PlayerTrackSurface", 3.0) // OnTrack
                .set("IsOnTrack", 1.0);
            let img = Rc::new(RefCell::new(b.build()));
            let running = Rc::new(Cell::new(true));
            let (i2, r2) = (img.clone(), running.clone());
            let src = IracingSource::new(Box::new(move || r2.get().then(|| FakeMem(i2.clone()))));
            Rig { src, b, img, running }
        }

        /// Publish the builder's current state as a new tick.
        fn tick(&mut self) {
            self.b.tick += 1;
            *self.img.borrow_mut() = self.b.build();
        }
    }

    fn expect_frame(r: PollResult) -> crate::frame::Frame {
        match r {
            PollResult::Frame(f) => f,
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn not_running_reports_not_connected() {
        let mut rig = Rig::new();
        rig.running.set(false);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
    }

    #[test]
    fn first_poll_announces_session_then_frames() {
        let mut rig = Rig::new();
        match rig.src.poll() {
            PollResult::Session(info) => {
                assert_eq!(info.track_name, "Circuit de Spa-Francorchamps");
                assert_eq!(info.car_name, "Porsche 963 GTP");
                assert_eq!(info.session_type, "Practice");
            }
            other => panic!("expected Session, got {other:?}"),
        }
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.session_time_s, 1234.5);
        assert_eq!(f.lap, 3);
        assert_eq!(f.sim_last_lap_time_s, None);
        assert_eq!(f.get(Channel::SpeedMs), 55.5);
        assert_eq!(f.get(Channel::Clutch), 0.75);
        assert_eq!(f.get(Channel::OffTrack), 0.0);
        assert_eq!(f.get(Channel::OnPitRoad), 0.0);
        assert!(f.get(Channel::PosX).is_nan());
    }

    #[test]
    fn same_tick_is_paused_new_tick_is_frame() {
        let mut rig = Rig::new();
        rig.src.poll(); // Session
        expect_frame(rig.src.poll());
        assert!(matches!(rig.src.poll(), PollResult::Paused));
        rig.b.set("SessionTime", 1234.6).set("LapLastLapTime", 101.25);
        rig.tick();
        let f = expect_frame(rig.src.poll());
        assert_eq!(f.session_time_s, 1234.6);
        assert_eq!(f.sim_last_lap_time_s, Some(101.25));
    }

    #[test]
    fn replay_or_off_car_is_idle() {
        let mut rig = Rig::new();
        rig.src.poll();
        rig.b.set("IsReplayPlaying", 1.0);
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
        rig.b.set("IsReplayPlaying", 0.0).set("IsOnTrack", 0.0);
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Idle));
    }

    #[test]
    fn off_track_surface_sets_flag() {
        let mut rig = Rig::new();
        rig.src.poll();
        rig.b.set("PlayerTrackSurface", 0.0);
        rig.tick();
        assert_eq!(expect_frame(rig.src.poll()).get(Channel::OffTrack), 1.0);
    }

    #[test]
    fn session_num_change_emits_new_session() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.set("SessionNum", 1.0);
        rig.tick();
        match rig.src.poll() {
            PollResult::Session(info) => assert_eq!(info.session_type, "Race"),
            other => panic!("expected Session, got {other:?}"),
        }
        expect_frame(rig.src.poll()); // the same sample follows
    }

    #[test]
    fn yaml_update_with_same_identity_is_not_a_new_session() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.session_info_update += 1; // e.g. results changed
        rig.tick();
        expect_frame(rig.src.poll());
    }

    #[test]
    fn sim_exit_then_reconnect() {
        let mut rig = Rig::new();
        rig.src.poll();
        expect_frame(rig.src.poll());
        rig.b.status = 0; // sim shutting down
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.running.set(false);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
        rig.running.set(true);
        rig.b.status = 1;
        rig.tick();
        assert!(matches!(rig.src.poll(), PollResult::Session(_)));
        expect_frame(rig.src.poll());
    }

    #[test]
    fn truncated_memory_is_not_connected_not_a_panic() {
        let mut rig = Rig::new();
        rig.img.borrow_mut().truncate(50);
        assert!(matches!(rig.src.poll(), PollResult::NotConnected));
    }
}
```

- [ ] **Step 3: Update `sim/iracing/mod.rs`**

```rust
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
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p tea-core source`
Expected: compile errors, `cannot find trait SharedMem` / missing `win` module.

- [ ] **Step 5: Implement `source.rs`** (above the test module)

```rust
//! iRacing [`SimSource`]: turns shared-memory snapshots into [`PollResult`]s.

use super::layout::{parse_var_headers, IrHeader, HEADER_LEN, VAR_HEADER_LEN};
use super::mapping::VarMap;
use super::yaml::session_from_yaml;
use crate::sim::{PollResult, Sim, SimSource};

/// Max time one poll blocks waiting for the sim to publish a sample.
const WAIT_MS: u32 = 100;

/// Read access to the sim's shared memory (real on Windows, fake in tests).
pub trait SharedMem {
    /// Copy `len` bytes at `offset`, or None if the range is out of bounds.
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>>;
    /// Block until the sim signals new data or `timeout_ms` elapses.
    fn wait_for_data(&self, timeout_ms: u32);
}

struct Disconnected;

struct Connection<M> {
    mem: M,
    vars: VarMap,
    var_layout: Option<(i32, i32)>,
    last_tick: Option<i32>,
    session_info_update: Option<i32>,
    session_num: Option<i32>,
    session_key: Option<String>,
}

impl<M: SharedMem> Connection<M> {
    fn new(mem: M) -> Self {
        Self {
            mem,
            vars: VarMap::default(),
            var_layout: None,
            last_tick: None,
            session_info_update: None,
            session_num: None,
            session_key: None,
        }
    }

    fn header(&self) -> Result<IrHeader, Disconnected> {
        let bytes = self.mem.read(0, HEADER_LEN).ok_or(Disconnected)?;
        IrHeader::parse(&bytes).map_err(|_| Disconnected)
    }

    fn poll(&mut self) -> Result<PollResult, Disconnected> {
        self.mem.wait_for_data(WAIT_MS);
        let header = self.header()?;
        if !header.is_connected() {
            return Err(Disconnected);
        }

        let layout = (header.num_vars, header.var_header_offset);
        if self.var_layout != Some(layout) {
            let n = header.num_vars.max(0) as usize;
            let bytes = self
                .mem
                .read(header.var_header_offset.max(0) as usize, n * VAR_HEADER_LEN)
                .ok_or(Disconnected)?;
            let vars = parse_var_headers(&bytes, n).map_err(|_| Disconnected)?;
            self.vars = VarMap::resolve(&vars);
            self.var_layout = Some(layout);
        }

        let Some(latest) = header.latest_buf() else { return Ok(PollResult::Paused) };
        if self.last_tick == Some(latest.tick_count) {
            return Ok(PollResult::Paused);
        }
        let buf = self
            .mem
            .read(latest.buf_offset.max(0) as usize, header.buf_len.max(0) as usize)
            .ok_or(Disconnected)?;
        // If the sim rewrote this slot while we copied it, skip this sample.
        let still_same = self
            .header()?
            .var_bufs
            .iter()
            .any(|b| b.buf_offset == latest.buf_offset && b.tick_count == latest.tick_count);
        if !still_same {
            return Ok(PollResult::Paused);
        }
        self.last_tick = Some(latest.tick_count);

        let Some((frame, control)) = self.vars.read(&buf) else { return Ok(PollResult::Idle) };

        if self.session_info_update != Some(header.session_info_update)
            || self.session_num != Some(control.session_num)
        {
            let raw = self
                .mem
                .read(header.session_info_offset.max(0) as usize, header.session_info_len.max(0) as usize)
                .ok_or(Disconnected)?;
            let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            let parsed = session_from_yaml(&String::from_utf8_lossy(&raw[..end]), control.session_num);
            self.session_info_update = Some(header.session_info_update);
            self.session_num = Some(control.session_num);
            if self.session_key.as_deref() != Some(parsed.key.as_str()) {
                self.session_key = Some(parsed.key);
                self.last_tick = None; // deliver this same sample on the next poll
                return Ok(PollResult::Session(parsed.info));
            }
        }

        if control.is_replay || !control.is_on_track {
            return Ok(PollResult::Idle);
        }
        Ok(PollResult::Frame(frame))
    }
}

pub struct IracingSource<M: SharedMem> {
    connect: Box<dyn FnMut() -> Option<M>>,
    conn: Option<Connection<M>>,
}

impl<M: SharedMem> IracingSource<M> {
    pub fn new(connect: Box<dyn FnMut() -> Option<M>>) -> Self {
        Self { connect, conn: None }
    }
}

impl<M: SharedMem> SimSource for IracingSource<M> {
    fn sim(&self) -> Sim {
        Sim::Iracing
    }

    fn poll(&mut self) -> PollResult {
        if self.conn.is_none() {
            match (self.connect)() {
                Some(mem) => self.conn = Some(Connection::new(mem)),
                None => return PollResult::NotConnected,
            }
        }
        let conn = self.conn.as_mut().expect("connected");
        match conn.poll() {
            Ok(result) => result,
            Err(Disconnected) => {
                self.conn = None;
                PollResult::NotConnected
            }
        }
    }
}
```

- [ ] **Step 6: Implement `win.rs`** (thin Win32 FFI; not unit-tested, verified manually in Task 12)

```rust
//! Real shared-memory access to iRacing via Win32 (kernel32).

use super::source::SharedMem;
use std::ffi::c_void;

type Handle = *mut c_void;

const FILE_MAP_READ: u32 = 0x0004;
const SYNCHRONIZE: u32 = 0x0010_0000;

#[repr(C)]
struct MemoryBasicInformation {
    base_address: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenFileMappingW(desired_access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(mapping: Handle, access: u32, off_high: u32, off_low: u32, bytes: usize) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn VirtualQuery(addr: *const c_void, info: *mut MemoryBasicInformation, len: usize) -> usize;
    fn OpenEventW(desired_access: u32, inherit: i32, name: *const u16) -> Handle;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct WinSharedMem {
    mapping: Handle,
    view: *const u8,
    size: usize,
    event: Handle,
}

/// Open iRacing's telemetry memory map, or None if the sim isn't running.
pub fn connect() -> Option<WinSharedMem> {
    unsafe {
        let mapping = OpenFileMappingW(FILE_MAP_READ, 0, wide("Local\\IRSDKMemMapFileName").as_ptr());
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        if view.is_null() {
            CloseHandle(mapping);
            return None;
        }
        let mut info: MemoryBasicInformation = std::mem::zeroed();
        let ok = VirtualQuery(view, &mut info, std::mem::size_of::<MemoryBasicInformation>());
        if ok == 0 {
            UnmapViewOfFile(view);
            CloseHandle(mapping);
            return None;
        }
        let event = OpenEventW(SYNCHRONIZE, 0, wide("Local\\IRSDKDataValidEvent").as_ptr());
        Some(WinSharedMem { mapping, view: view as *const u8, size: info.region_size, event })
    }
}

impl SharedMem for WinSharedMem {
    fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let end = offset.checked_add(len)?;
        if end > self.size {
            return None;
        }
        let mut out = vec![0u8; len];
        // The sim writes this memory concurrently; torn reads are detected by the caller
        // re-checking the buffer's tick count after copying.
        unsafe { std::ptr::copy_nonoverlapping(self.view.add(offset), out.as_mut_ptr(), len) };
        Some(out)
    }

    fn wait_for_data(&self, timeout_ms: u32) {
        if self.event.is_null() {
            std::thread::sleep(std::time::Duration::from_millis(16));
        } else {
            unsafe { WaitForSingleObject(self.event, timeout_ms) };
        }
    }
}

impl Drop for WinSharedMem {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view as *const c_void);
            CloseHandle(self.mapping);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}
```

- [ ] **Step 7: Run all core tests**

Run: `cargo test -p tea-core`
Expected: everything passes, including the 9 `source` tests. `cargo build -p tea-core` has no warnings about `win.rs`.

- [ ] **Step 8: Commit**

```bash
git add crates/tea-core
git commit -m "feat(iracing): live source with var mapping and Win32 shared memory

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: RecorderService (thread, supervisor, capture)

**Files:**
- Create: `crates/tea-core/src/service.rs`
- Modify: `crates/tea-core/src/lib.rs` (add `pub mod service;`)

**Interfaces:**
- Consumes: `SimSource`, `PollResult`, `Sim` (Task 2); `Recorder` (Task 4); `Store` (Task 6); `CaptureWriter`, `ReplaySource` (Task 7); `testutil::{session, drive}` (tests).
- Produces:
  - `service::SourceFactory = Box<dyn Fn() -> Box<dyn SimSource> + Send>`
  - `service::RecState { NoSim, Idle, Recording }` (serde snake_case: `"no_sim" | "idle" | "recording"`)
  - `service::RecorderStatus { state: RecState, sim: Option<Sim>, lap: Option<i32>, laps_saved: u32, last_error: Option<String> }` (serde `Serialize`, `Default` = NoSim)
  - `service::ServiceConfig { data_root: PathBuf, capture_raw: bool, not_connected_backoff: Duration }`
  - `service::RecorderService::start(factory: SourceFactory, config: ServiceConfig) -> anyhow::Result<RecorderService>` (opens the store and runs `reconcile` before spawning), `status(&self) -> RecorderStatus`. Dropping it stops and joins the thread.

- [ ] **Step 1: Write the failing tests** in a new `service.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::replay::ReplaySource;
    use crate::testutil::{drive, session};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    fn two_lap_events() -> Vec<PollResult> {
        let mut ev = vec![PollResult::NotConnected, PollResult::Session(session())];
        ev.extend(drive(1, 0.5553, 250.0).into_iter().map(PollResult::Frame));
        ev
    }

    fn config(dir: &std::path::Path) -> ServiceConfig {
        ServiceConfig {
            data_root: dir.to_path_buf(),
            capture_raw: false,
            not_connected_backoff: Duration::from_millis(10),
        }
    }

    fn wait_for(svc: &RecorderService, pred: impl Fn(&RecorderStatus) -> bool) -> RecorderStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = svc.status();
            if pred(&s) || Instant::now() > deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn records_laps_from_replay() {
        let dir = tempfile::tempdir().unwrap();
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
        });
        let svc = RecorderService::start(factory, config(dir.path())).unwrap();
        // The replay ends in NotConnected, which flushes the pending lap 3.
        let status = wait_for(&svc, |s| s.laps_saved >= 2);
        assert_eq!(status.laps_saved, 2);
        let status = wait_for(&svc, |s| s.state == RecState::NoSim);
        assert_eq!(status.state, RecState::NoSim);
        drop(svc);

        let store = Store::open(dir.path()).unwrap();
        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 2);
        assert!(laps.iter().all(|l| l.lap_time_ms == 100_000));
    }

    #[test]
    fn recorder_restarts_after_panic() {
        struct Boom;
        impl SimSource for Boom {
            fn sim(&self) -> Sim {
                Sim::Iracing
            }
            fn poll(&mut self) -> PollResult {
                panic!("boom: malformed shared memory")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Box::new(Boom)
            } else {
                Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
            }
        });
        let svc = RecorderService::start(factory, config(dir.path())).unwrap();
        let status = wait_for(&svc, |s| s.laps_saved >= 2);
        assert_eq!(status.laps_saved, 2, "recorder must recover and keep recording");
        assert!(status.last_error.unwrap_or_default().contains("boom"));
    }

    #[test]
    fn capture_raw_writes_a_replayable_file() {
        let dir = tempfile::tempdir().unwrap();
        let events = two_lap_events();
        let factory: SourceFactory = Box::new(move || -> Box<dyn SimSource> {
            Box::new(ReplaySource::from_events(Sim::Iracing, events.clone(), false))
        });
        let mut cfg = config(dir.path());
        cfg.capture_raw = true;
        let svc = RecorderService::start(factory, cfg).unwrap();
        wait_for(&svc, |s| s.laps_saved >= 2);
        drop(svc); // finishes the capture file

        let capture = std::fs::read_dir(dir.path().join("captures"))
            .unwrap()
            .next()
            .expect("a capture file")
            .unwrap()
            .path();
        let mut replay = ReplaySource::open(&capture, false).unwrap();
        assert!(matches!(replay.poll(), PollResult::NotConnected));
        assert!(matches!(replay.poll(), PollResult::Session(_)));
        assert!(matches!(replay.poll(), PollResult::Frame(_)));
    }
}
```

Add to `lib.rs`: `pub mod service;`

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tea-core service`
Expected: compile errors, `cannot find type RecorderService`.

- [ ] **Step 3: Implement** (above the test module)

```rust
//! The background recorder: polls a sim, splits laps, saves them. Survives panics.

use crate::recorder::Recorder;
use crate::sim::replay::CaptureWriter;
use crate::sim::{PollResult, Sim, SimSource};
use crate::store::Store;
use serde::Serialize;
use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RESTART_DELAY: Duration = Duration::from_secs(1);

pub type SourceFactory = Box<dyn Fn() -> Box<dyn SimSource> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecState {
    #[default]
    NoSim,
    Idle,
    Recording,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
pub struct RecorderStatus {
    pub state: RecState,
    pub sim: Option<Sim>,
    pub lap: Option<i32>,
    pub laps_saved: u32,
    pub last_error: Option<String>,
}

pub struct ServiceConfig {
    pub data_root: PathBuf,
    /// Debug: dump every PollResult to `data_root/captures/` for building fixtures.
    pub capture_raw: bool,
    /// How long to wait before re-checking for the sim when it isn't running.
    pub not_connected_backoff: Duration,
}

pub struct RecorderService {
    status: Arc<Mutex<RecorderStatus>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

fn lock(status: &Mutex<RecorderStatus>) -> MutexGuard<'_, RecorderStatus> {
    status.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl RecorderService {
    pub fn start(factory: SourceFactory, config: ServiceConfig) -> anyhow::Result<Self> {
        let mut store = Store::open(&config.data_root)?;
        let report = store.reconcile()?;
        log::info!("store reconciled: {report:?}");
        let status = Arc::new(Mutex::new(RecorderStatus::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_status, thread_stop) = (status.clone(), stop.clone());
        let handle = thread::Builder::new()
            .name("recorder".into())
            .spawn(move || supervise(factory, config, store, thread_status, thread_stop))?;
        Ok(Self { status, stop, handle: Some(handle) })
    }

    pub fn status(&self) -> RecorderStatus {
        lock(&self.status).clone()
    }
}

impl Drop for RecorderService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let until = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10).min(d));
    }
}

fn supervise(
    factory: SourceFactory,
    config: ServiceConfig,
    mut store: Store,
    status: Arc<Mutex<RecorderStatus>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let result = panic::catch_unwind(AssertUnwindSafe(|| run(&factory, &config, &mut store, &status, &stop)));
        let error = match result {
            Ok(Ok(())) => break, // stop requested
            Ok(Err(e)) => format!("{e:#}"),
            Err(p) => format!("recorder crashed: {}", panic_message(p.as_ref())),
        };
        log::error!("{error}; restarting recorder");
        {
            let mut s = lock(&status);
            s.last_error = Some(error);
            s.state = RecState::NoSim;
            s.sim = None;
            s.lap = None;
        }
        sleep_unless_stopped(RESTART_DELAY, &stop);
    }
}

fn capture_path(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = root.join("captures");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("capture-{}.tcap", now_ms())))
}

fn run(
    factory: &SourceFactory,
    config: &ServiceConfig,
    store: &mut Store,
    status: &Mutex<RecorderStatus>,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let mut source = factory();
    let mut recorder = Recorder::new();
    let mut capture = if config.capture_raw {
        Some(CaptureWriter::create(&capture_path(&config.data_root)?, source.sim())?)
    } else {
        None
    };
    while !stop.load(Ordering::SeqCst) {
        let event = source.poll();
        if let Some(c) = capture.as_mut() {
            c.write(&event)?;
        }
        let not_connected = matches!(event, PollResult::NotConnected);
        for lap in recorder.handle(event, now_ms()) {
            match store.save_lap(&lap, now_ms()) {
                Ok(()) => {
                    log::info!("saved lap {} ({} ms)", lap.lap_number, lap.lap_time_ms);
                    lock(status).laps_saved += 1;
                }
                Err(e) => {
                    log::error!("failed to save lap {}: {e:#}", lap.lap_number);
                    lock(status).last_error = Some(format!("failed to save lap: {e:#}"));
                }
            }
        }
        {
            let mut s = lock(status);
            s.sim = (!not_connected).then(|| source.sim());
            s.state = if not_connected {
                RecState::NoSim
            } else if recorder.is_recording() {
                RecState::Recording
            } else {
                RecState::Idle
            };
            s.lap = recorder.current_lap();
        }
        if not_connected {
            sleep_unless_stopped(config.not_connected_backoff, stop);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tea-core service`
Expected: 3 passed. The panic test prints a `boom` panic message to stderr; that's expected.

- [ ] **Step 5: Run the whole core suite**

Run: `cargo test -p tea-core`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tea-core
git commit -m "feat(core): supervised background recorder service

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Tauri wiring, status screen, mock mode, and real-sim check

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/lib.rs`, `src-tauri/tauri.conf.json`, `src/routes/+page.svelte`, `package.json`
- Create: `src/lib/format.ts`, `src/lib/format.test.ts`, `crates/tea-core/examples/make_mock_capture.rs`

**Interfaces:**
- Consumes: `RecorderService`, `RecorderStatus`, `ServiceConfig`, `SourceFactory` (Task 11); `Store`, `LapSummary` (Task 6); `ReplaySource`, `CaptureWriter` (Task 7); `iracing::live_source` (Task 10).
- Produces:
  - Tauri commands: `get_status() -> RecorderStatus` and `recent_laps(limit: u32) -> Result<Vec<LapSummary>, String>`
  - Env vars: `TEA_MOCK=<path.tcap>` replays a capture in real time instead of reading iRacing. `TEA_CAPTURE=1` dumps raw adapter output to `%APPDATA%\tea-telemetry\captures\`.
  - `src/lib/format.ts`: `formatLapTime(ms: number): string` (`"m:ss.mmm"`, or `"--:--.---"` for invalid input)

- [ ] **Step 1: Write the failing frontend test** `src/lib/format.test.ts`

```ts
import { describe, expect, it } from 'vitest';
import { formatLapTime } from './format';

describe('formatLapTime', () => {
  it('formats minutes, seconds and milliseconds', () => {
    expect(formatLapTime(123412)).toBe('2:03.412');
    expect(formatLapTime(59999)).toBe('0:59.999');
    expect(formatLapTime(0)).toBe('0:00.000');
    expect(formatLapTime(600000)).toBe('10:00.000');
  });

  it('rounds fractional milliseconds', () => {
    expect(formatLapTime(100000.6)).toBe('1:40.001');
  });

  it('shows a placeholder for invalid input', () => {
    expect(formatLapTime(-1)).toBe('--:--.---');
    expect(formatLapTime(Number.NaN)).toBe('--:--.---');
  });
});
```

- [ ] **Step 2: Add Vitest and run to verify it fails**

```bash
npm install -D vitest
```
Add `"test": "vitest run"` to `"scripts"` in `package.json`.

Run: `npm test`
Expected: FAIL, cannot resolve `./format`.

- [ ] **Step 3: Implement `src/lib/format.ts`**

```ts
/** Format a lap time in milliseconds as m:ss.mmm. */
export function formatLapTime(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '--:--.---';
  const total = Math.round(ms);
  const minutes = Math.floor(total / 60000);
  const seconds = Math.floor((total % 60000) / 1000);
  const millis = total % 1000;
  return `${minutes}:${String(seconds).padStart(2, '0')}.${String(millis).padStart(3, '0')}`;
}
```

Run: `npm test`
Expected: 3 passed.

- [ ] **Step 4: Add the log plugin.** In `src-tauri/Cargo.toml` `[dependencies]` add:

```toml
tauri-plugin-log = "2"
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 5: Replace `src-tauri/src/lib.rs`**

Keep the scaffold's `tauri_plugin_opener` line if present; everything else is replaced.

```rust
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tauri::Manager;
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};
use tea_core::service::{RecorderService, RecorderStatus, ServiceConfig, SourceFactory};
use tea_core::sim::replay::ReplaySource;
use tea_core::sim::SimSource;
use tea_core::store::{LapSummary, Store};

struct AppState {
    service: RecorderService,
    store: Mutex<Store>,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> RecorderStatus {
    state.service.status()
}

#[tauri::command]
fn recent_laps(state: tauri::State<'_, AppState>, limit: u32) -> Result<Vec<LapSummary>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.recent_laps(limit).map_err(|e| format!("{e:#}"))
}

/// `%APPDATA%\tea-telemetry` (spec §5).
fn data_root() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tea-telemetry")
}

/// Real iRacing, or a replayed capture when `TEA_MOCK` points at a `.tcap` file.
fn source_factory() -> SourceFactory {
    match std::env::var_os("TEA_MOCK") {
        Some(path) => {
            log::info!("mock sim: replaying {}", Path::new(&path).display());
            Box::new(move || -> Box<dyn SimSource> {
                match ReplaySource::open(Path::new(&path), true) {
                    Ok(src) => Box::new(src),
                    Err(e) => panic!("TEA_MOCK capture unreadable: {e:#}"),
                }
            })
        }
        None => Box::new(|| -> Box<dyn SimSource> { Box::new(tea_core::sim::iracing::live_source()) }),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = data_root();
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::Folder { path: root.join("logs"), file_name: Some("tea".into()) }),
                ])
                .level(log::LevelFilter::Info)
                .max_file_size(5_000_000)
                .rotation_strategy(RotationStrategy::KeepOne)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let store = Store::open(&root)?;
            let service = RecorderService::start(
                source_factory(),
                ServiceConfig {
                    data_root: root.clone(),
                    capture_raw: std::env::var_os("TEA_CAPTURE").is_some(),
                    not_connected_backoff: Duration::from_secs(2),
                },
            )?;
            app.manage(AppState { service, store: Mutex::new(store) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_status, recent_laps])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

If the scaffold has no `tauri-plugin-opener` dependency, delete that `.plugin(...)` line. If `tauri_plugin_log`'s builder API differs in the installed version, check the plugin docs and keep the same behaviour: stdout plus a rotating file in `root/logs`, capped at 5 MB.

- [ ] **Step 6: Set the window title and size** in `src-tauri/tauri.conf.json`: `"productName": "Tea Telemetry"`, and under `app.windows[0]` use `"title": "Tea Telemetry"`, `"width": 1100`, `"height": 720`.

- [ ] **Step 7: Replace `src/routes/+page.svelte`**

```svelte
<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { formatLapTime } from '$lib/format';

  type RecorderStatus = {
    state: 'no_sim' | 'idle' | 'recording';
    sim: 'iracing' | 'lmu' | null;
    lap: number | null;
    laps_saved: number;
    last_error: string | null;
  };

  type LapSummary = {
    lap_id: string;
    sim: string;
    track_name: string;
    track_config: string;
    car_name: string;
    session_type: string;
    lap_number: number;
    lap_time_ms: number;
    valid: boolean;
    invalid_reason: string | null;
    fuel_used_l: number | null;
    created_at_ms: number;
  };

  let status = $state<RecorderStatus | null>(null);
  let laps = $state<LapSummary[]>([]);
  let error = $state<string | null>(null);

  const simName = (s: string | null) => (s === 'iracing' ? 'iRacing' : s === 'lmu' ? 'LMU' : '');

  function statusText(s: RecorderStatus | null): string {
    if (!s || s.state === 'no_sim') return 'No sim detected';
    if (s.state === 'idle') return `${simName(s.sim)} · idle`;
    return `${simName(s.sim)} · recording · Lap ${s.lap ?? '?'}`;
  }

  async function refresh() {
    try {
      status = await invoke<RecorderStatus>('get_status');
      laps = await invoke<LapSummary[]>('recent_laps', { limit: 50 });
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  });
</script>

<main>
  <header>
    <h1>Tea Telemetry</h1>
    <span class="pill" class:live={status?.state === 'recording'} class:idle={status?.state === 'idle'}>
      {statusText(status)}
    </span>
  </header>

  {#if status?.last_error}
    <p class="error">Recorder: {status.last_error}</p>
  {/if}
  {#if error}
    <p class="error">{error}</p>
  {/if}

  <h2>Recent laps</h2>
  {#if laps.length === 0}
    <p class="muted">No laps yet. Start driving and completed laps will appear here.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Track</th><th>Car</th><th>Session</th><th>Lap</th><th>Time</th><th>Fuel</th><th></th></tr>
      </thead>
      <tbody>
        {#each laps as lap (lap.lap_id)}
          <tr class:invalid={!lap.valid}>
            <td>{lap.track_name}{lap.track_config ? ` · ${lap.track_config}` : ''}</td>
            <td>{lap.car_name}</td>
            <td>{lap.session_type}</td>
            <td class="num">{lap.lap_number}</td>
            <td class="num">{formatLapTime(lap.lap_time_ms)}</td>
            <td class="num">{lap.fuel_used_l != null ? `${lap.fuel_used_l.toFixed(2)} L` : '–'}</td>
            <td class="muted">{lap.valid ? '' : lap.invalid_reason?.replace('_', ' ')}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    background: #0e1116;
    color: #c9d1d9;
    font-family: 'Segoe UI', system-ui, sans-serif;
  }
  main { padding: 20px 24px; }
  header { display: flex; align-items: center; gap: 16px; }
  h1 { font-size: 20px; margin: 0; }
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: #6b7785; margin-top: 28px; }
  .pill { padding: 4px 12px; border-radius: 999px; font-size: 12px; background: #1f2630; color: #8b949e; }
  .pill.idle { background: rgba(56, 189, 248, 0.12); color: #38bdf8; }
  .pill.live { background: rgba(74, 222, 128, 0.14); color: #4ade80; }
  .error { color: #f87171; font-size: 13px; }
  .muted { color: #6b7785; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; color: #6b7785; font-weight: 600; padding: 6px 8px; border-bottom: 1px solid #1f2630; }
  td { padding: 6px 8px; border-bottom: 1px solid #161b22; }
  .num { font-variant-numeric: tabular-nums; }
  tr.invalid td:not(.muted) { color: #6b7785; }
</style>
```

- [ ] **Step 8: Create the mock capture generator** `crates/tea-core/examples/make_mock_capture.rs`

```rust
//! Writes a synthetic 5-lap iRacing-like capture for UI work without a sim:
//!   cargo run -p tea-core --example make_mock_capture -- mock.tcap
//! then run the app with TEA_MOCK=mock.tcap.

use std::collections::BTreeMap;
use tea_core::frame::{Channel, Frame};
use tea_core::sim::replay::CaptureWriter;
use tea_core::sim::{PollResult, SessionInfo, Sim};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "mock.tcap".into());
    let mut w = CaptureWriter::create(std::path::Path::new(&path), Sim::Iracing)?;
    w.write(&PollResult::Session(SessionInfo {
        sim: Sim::Iracing,
        track_id: "0".into(),
        track_name: "Mock Oval".into(),
        track_config: "".into(),
        track_length_m: 4000.0,
        car_id: "0".into(),
        car_name: "Mock Car".into(),
        session_type: "Practice".into(),
        sector_start_pcts: vec![0.0, 0.33, 0.66],
        conditions: BTreeMap::new(),
    }))?;
    let dt = 1.0 / 60.0;
    let mut dist = 0.8; // start mid-lap, like leaving the pits
    let mut t = 0.0;
    while dist < 5.99 {
        // Each lap is a little different so the laps aren't identical.
        let lap_time = 60.0 + (dist.floor() * 0.37) % 1.2;
        let pct: f64 = dist.fract();
        let slow = (pct * std::f64::consts::TAU * 2.0).cos().max(0.0);
        let mut f = Frame::new(t, 1 + dist.floor() as i32);
        f.set(Channel::LapDistPct, pct as f32);
        f.set(Channel::LapDistM, (pct * 4000.0) as f32);
        f.set(Channel::SpeedMs, (75.0 - 30.0 * slow) as f32);
        f.set(Channel::Throttle, (1.0 - slow) as f32);
        f.set(Channel::Brake, if slow > 0.7 { 0.8 } else { 0.0 });
        f.set(Channel::Gear, if slow > 0.5 { 3.0 } else { 5.0 });
        f.set(Channel::FuelL, (40.0 - t * 0.03) as f32);
        f.set(Channel::OnPitRoad, 0.0);
        f.set(Channel::OffTrack, 0.0);
        w.write(&PollResult::Frame(f))?;
        dist += dt / lap_time;
        t += dt;
    }
    w.write(&PollResult::Idle)?;
    println!("wrote {path}");
    Ok(())
}
```

- [ ] **Step 9: Build everything**

```bash
cargo test -p tea-core
cargo build -p tea-telemetry
npm test
```
Expected: all green, no build errors.

- [ ] **Step 10: Verify end to end with the mock sim** (no iRacing needed)

```bash
cargo run -p tea-core --example make_mock_capture -- "$TEMP/mock.tcap"
TEA_MOCK="$TEMP/mock.tcap" npm run tauri dev
```
Expected, in the app window (about 5 minutes of real-time replay):
- The status reads `iRacing · recording · Lap N`.
- Laps 2–5 appear in the list one by one, each about 1:00.xxx with fuel about 1.80 L.
- Lap 1 never appears, because it started mid-lap.
- `%APPDATA%\tea-telemetry\laps` holds one `.tlap` per listed lap, and `logs\tea.log` exists.

Stop the app. Clear the mock laps before real use:
```bash
rm -rf "$APPDATA/tea-telemetry"
```

- [ ] **Step 11: Real iRacing check (performed by the user; the agent cannot drive the sim)**

Ask the user to do this:
1. Run `TEA_CAPTURE=1 npm run tauri dev`.
2. In iRacing, start a Test Drive, leave the pits, and drive 3 or more timed laps. Include one lap with 4 wheels off.
3. Confirm:
   - The status pill goes `No sim detected` → `iRacing · idle` (in the garage) → `iRacing · recording · Lap N` (on track).
   - Each completed lap appears with a time matching iRacing's lap time to the millisecond.
   - The out-lap is flagged `out lap` (or absent if the pit box was past the line), and the off-track lap is flagged `off track`.
   - Exiting to the menu shows `idle`; closing iRacing shows `No sim detected`, and the app keeps running.
4. Keep the `.tcap` file from `%APPDATA%\tea-telemetry\captures\`. It becomes the real-data fixture for Phase 2.

Record the result (pass, or the exact mismatch) in the task report. A mismatch here is a bug to debug (superpowers:systematic-debugging), not a reason to change the tests.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "feat(app): wire recorder service into Tauri with status and recent laps

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
