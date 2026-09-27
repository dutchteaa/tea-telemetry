# Tea Telemetry: Design Spec

**Date:** 2026-09-27
**Status:** Draft, awaiting review
**Platform:** Windows 10/11 only

## 1. Purpose

A lightweight, good-looking Windows desktop tool, in the spirit of Go Fast, that records telemetry from **iRacing** and **Le Mans Ultimate (LMU)** automatically in the background. After a session you can compare laps to see exactly where you gain or lose time.

**Success looks like:** you drive, open the app, pick two laps, and immediately see where the time went: traces, delta, map and a per-corner breakdown.

### In scope (v1)
- Automatic background recording from iRacing and LMU (no button to press).
- Local lap library (sessions → laps) with filtering.
- Compare view: stacked traces vs. distance, time-delta graph, track map, sector/corner breakdown, one synced cursor.
- Recording **all** available channels, including fuel, tyres, brakes and ride heights, even though v1 only visualises the core ones.
- Manual `.tlap` export/import of single laps.

### Out of scope (v1), but the design must not block it
- **Endurance strategy** (fuel/stint/pit calculation). Fuel-used-per-lap and conditions are stored per lap so this later becomes a query.
- **Shared/cloud lap pool** (teammates or public). Laps have UUIDs and a portable file format so a backend can be added later.
- Live in-sim overlays (delta bar, relative, etc.).
- Visualising extra channels (tyres, suspension, etc.).
- Light theme, macOS/Linux.

### Assumptions
- Mostly solo practice/race analysis; no live team features.
- Single user per PC.

## 2. Stack

| Concern | Choice | Why |
|---|---|---|
| App shell | **Tauri 2** | Uses the system WebView2. ~10 MB installer, ~50–100 MB RAM. |
| Core / recorder / analysis | **Rust** | Negligible CPU cost at 60 Hz, safe native shared-memory access, testable pure functions. |
| UI | **Svelte + TypeScript** | Lightweight, simple component model. |
| Charts | **uPlot** | Handles 100k+ points smoothly; supports synced cursors. |
| Metadata | **SQLite** (`rusqlite`, bundled) | Simple embedded index. |
| Lap data | Custom `.tlap` (JSON header + columnar `f32`, zstd) | Compact, portable, no heavy dependencies. |

**Dev prerequisites to install:** Rust toolchain (MSVC), Visual Studio C++ Build Tools, Node (already present). WebView2 ships with Windows 11.

## 3. Architecture

```
 iRacing shared mem ─┐                                   ┌─ SQLite index (sessions, laps)
                     ├─► SimSource ─► Recorder ─► Store ─┤
 LMU shared mem ─────┘   (adapters)   (lap split)        └─ per-lap .tlap files
                                                                │
                              Analysis (Rust, pure fns) ◄───────┘
                                        │ Tauri commands (binary IPC)
                                        ▼
                         Svelte UI: Library · Compare · Settings
```

### Rust modules (`src-tauri/src/`)
- **`sim/`**: `SimSource` trait: `poll() -> PollResult` (a `Frame`, `NotConnected`, `Paused`, or `SessionChanged(SessionInfo)`).
  - `sim/iracing.rs`: opens the `Local\IRSDKMemMapFileName` memory map, waits on the `Local\IRSDKDataValidEvent`, parses the header and variable headers, reads the newest buffer by tick count, and parses the session-info YAML (track, car, sector split points, session type).
  - `sim/lmu.rs`: LMU's native shared-memory interface. The exact layout must be verified against LMU's shipped headers during implementation.
  - `sim/replay.rs`: `ReplaySource` plays back captured frame files, used by tests and the "mock sim" dev mode.
  - `sim/frame.rs`: the normalized `Frame` and channel registry.
- **`recorder/`**: a state machine that turns a stream of `PollResult` into sessions and completed laps.
- **`store/`**: SQLite index plus `.tlap` reader/writer, crash-safe writes, startup reconcile.
- **`analysis/`**: pure functions for resampling, delta, map reconstruction and corner detection.
- **`commands.rs`**: Tauri command handlers (the only UI-facing API).
- **`tray.rs`**: tray icon and status.

Each module depends only on those beneath it: `sim` knows nothing of storage, and `analysis` knows nothing of sims or disk.

## 4. Recording

### Sampling
- **iRacing:** block on the data-valid event (with timeout) and read the newest buffer at the native 60 Hz.
- **LMU:** poll at its update rate (~50–100 Hz).
- Samples are stored **at native rate with no resampling at record time**.
- The adapter checks for the sim every 2 s while not connected.

### Channels
A registry of named channels with units. Adapters fill whatever their sim provides; missing channels are simply absent from the lap file.

**Core (visualised in v1):** `session_time_s`, `lap`, `lap_dist_m`, `lap_dist_pct`, `speed_ms`, `throttle` (0–1), `brake` (0–1), `clutch` (0–1), `steering_rad`, `gear`, `rpm`, `pos_x`, `pos_z` (native in LMU; reconstructed in iRacing, see §6), `yaw_rad`, `vel_x`, `vel_z`, `on_pit_road`, `track_surface`/off-track flag.

**Recorded, not visualised in v1:** `fuel_l`, tyre temps (inner/mid/outer ×4), tyre pressures ×4, tyre wear ×4, brake temps ×4, ride heights ×4, `track_temp_c`, `air_temp_c`.

### Lap splitting and validity
- A lap closes when the sim's lap counter increments.
- **Lap time:** the sim's own last-lap-time value when available (authoritative), otherwise computed from samples.
- Laps are **kept but flagged** with an `invalid_reason`: `out_lap`, `in_lap`, `off_track`, `reset`/`tow`, `incomplete`, or sim-reported invalidation.
- A lap that never crosses the line (sim closed, session ended) is **discarded**.
- **Replays are never recorded.** Paused/menu states pause recording without splitting the lap.

## 5. Storage

Root: `%APPDATA%\tea-telemetry\` (configurable in Settings).

```
index.db
laps/<uuid>.tlap
logs/
```

### SQLite schema (v1)
- `sessions`: `id`, `sim` (`iracing`|`lmu`), `track_id`, `track_name`, `track_config`, `track_length_m`, `car_id`, `car_name`, `session_type`, `started_at`, `conditions_json`.
- `laps`: `id` (UUID), `session_id`, `lap_number`, `lap_time_ms`, `valid`, `invalid_reason`, `fuel_start_l`, `fuel_used_l`, `sector_times_json`, `file_path`, `created_at`.
- `track_cache`: `sim`, `track_id`, `track_config`, `outline_lap_id`, `outline_blob`, `corners_json`, `updated_at`.
- `schema_version` table for migrations.

### `.tlap` format
- zstd-compressed stream: `magic "TLAP"` + `u32 format_version` + `u32 header_len` + JSON header + channel data.
- The JSON header holds the lap UUID, sim, track/car identity, lap time, validity, sample count, and a channel list (`name`, `unit`, byte offset).
- Channel data: each channel is a contiguous little-endian `f32` array of `sample_count` values.
- ~200–400 KB per lap.
- The file is self-describing: a `.tlap` alone can rebuild its DB rows, which is the basis for import and reconcile.

### Write safety
1. Write to `laps/<uuid>.tlap.tmp`, fsync, rename to `.tlap`.
2. Insert the `laps` row in a transaction.
3. On startup, a reconcile pass re-indexes orphan `.tlap` files from their headers, flags rows whose file is missing, and deletes stale `.tmp` files.

A file with a newer `format_version` is refused with a clear message, never misread.

## 6. Analysis

All pure Rust functions, unit-tested.

- **Distance grid:** resample each lap onto a uniform 1 m grid using `lap_dist_pct × track_length_m` (linear interpolation). Clip samples straddling start/finish and force distance to be monotonic.
- **Delta:** build time-at-distance `t(d)` per lap; `delta(d) = t_lap(d) − t_ref(d)` (positive = slower).
- **Track map:**
  - LMU: world X/Z directly.
  - iRacing: live telemetry has no position, so integrate velocity rotated by yaw into X/Z, then spread the loop-closure error linearly across the lap.
  - The outline from the best valid lap is cached per track in `track_cache` and refreshed when a faster valid lap arrives.
- **Sectors:** official sim sectors (iRacing split points from session info; LMU's 3 sectors) → per-sector time.
- **Corners:** detected on the cached track outline from speed minima plus heading change. Each spans brake point → exit, numbered T1…Tn in lap order and cached per track so numbering is stable.
- **Corner breakdown:** time gained/lost, min-speed difference and brake-point difference per corner.
- **IPC:** analysis results go to the UI as raw binary `f32` arrays (Tauri raw IPC responses), not JSON.

## 7. UI

Dark theme; each compared lap gets a fixed colour.

### Shell
- Slim left nav: **Library**, **Compare**, **Settings**.
- Top-bar status pill: e.g. `● iRacing · recording · Lap 12` / `○ No sim detected`.
- Closing the window minimises to tray; recording continues. Optional start with Windows.

### Library
- Sessions grouped by date, each expanding into its laps. Columns: lap #, lap time, gap to session best, fuel used, valid/reason.
- Filters: sim, track, car, session type, "show invalid" (off by default).
- Select 2–4 laps → **Compare**. One-click **"vs my best"** pairs a lap with your personal best for that car and track.
- Right-click → export `.tlap`; drag-and-drop a `.tlap` to import.

### Compare (Layout A, "Analyst")
- **Left:** lap list showing the laps being compared (which may come from different sessions) followed by the other laps from the first comparison lap's session. Click to add or remove laps, or to set the reference.
- **Centre:** stacked uPlot panes vs. distance: Delta, Speed, Throttle/Brake, Steering, Gear/RPM (each toggleable). One reference plus up to 3 comparison laps.
- **Right:** track map (comparison lap line coloured green/red by gain/loss against the reference) and the corner/sector table.
- **Synced cursor** across all panes and the map.
- Drag to zoom on traces, double-click to reset; clicking a corner row zooms to that corner and highlights it on the map.

### Settings
Data folder, units (km/h|mph, °C|°F), auto-record on/off, start with Windows, minimise to tray.

## 8. Error handling

- **No sim:** idle status, no popups.
- **Pause/menu/replay:** recording pauses; replays are never saved.
- **Sim crash/close mid-lap:** partial lap discarded; completed laps are already persisted.
- **Recorder isolation:** the recorder runs on its own thread; a panic is caught, logged, and the recorder is restarted without affecting the UI.
- **Storage:** crash-safe writes plus startup reconcile (§5); refuses newer `.tlap` versions.
- **Logging:** rotating log files in `%APPDATA%\tea-telemetry\logs`.

## 9. Testing

- **Analysis:** unit tests on synthetic laps with known answers. For example, a lap exactly 0.5 s slower through one corner must show +0.5 s delta across that corner and 0 elsewhere. Recorded real laps are regression fixtures.
- **Recorder:** driven by `ReplaySource` to test lap splitting, validity flags, pause, replay rejection and mid-lap sim exit, with no sim needed.
- **Adapters:** parsing tests against captured shared-memory snapshots (iRacing header/var headers, session YAML; LMU structs).
- **Store:** `.tlap` round-trip, version refusal, crash/reconcile scenarios.
- **Dev "mock sim" mode:** `ReplaySource` feeds the running app as if a sim were live, so the full UI flow is exercised without a sim.
- **Raw frame capture:** a hidden debug toggle dumps raw frames from real sessions to build fixtures.
- **UI:** Vitest for data transforms. Real-sim verification by the user at each milestone.

## 10. Build order

1. Scaffold (Tauri + Svelte), `Frame`/channel registry, iRacing adapter, recorder, store.
2. Analysis (resample, delta, map reconstruction, sectors, corners).
3. Library UI.
4. Compare UI.
5. LMU adapter.
6. Tray, settings, `.tlap` export/import.

## 11. Open items to verify during implementation

- LMU shared-memory layout, update rate, and which channels/flags it exposes (pause/replay state, lap invalidation, sectors). Verify against LMU's shipped interface headers before building the adapter.
- iRacing sector split points and track length parsing from session-info YAML across track configs.
