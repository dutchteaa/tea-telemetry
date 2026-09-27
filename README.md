# Tea Telemetry

A Windows desktop app that records lap telemetry from iRacing (Le Mans Ultimate planned)
and saves each lap as a portable `.tlap` file. Built with a Rust `tea-core` crate for
recording/storage, a Tauri 2 shell, and a SvelteKit UI.

## Dev prerequisites

- Rust (stable, MSVC toolchain)
- Visual Studio C++ Build Tools (required by the MSVC toolchain)
- Node.js

## Commands

```sh
npm install        # install frontend dependencies
npm run tauri dev  # run the app
cargo test -p tea-core  # run the Rust test suite
npm test           # run the frontend test suite
```

## Running without a sim

- `TEA_MOCK=<file.tcap>` replays a capture file in real time instead of reading iRacing's
  shared memory.
- `TEA_CAPTURE=1` dumps raw adapter output to `%APPDATA%\tea-telemetry\captures\` for
  building fixtures.

To generate a synthetic capture for UI work when no sim is available:

```sh
cargo run -p tea-core --example make_mock_capture -- mock.tcap
```

then run the app with `TEA_MOCK=mock.tcap`.

## Data

Sessions, laps and the index database live in `%APPDATA%\tea-telemetry`.

## Note

Windows Smart App Control blocks locally built, unsigned binaries from running. On many
Windows 11 builds, once Smart App Control has been turned off it cannot be turned back
on without resetting Windows, so check whether it's on (Settings > Privacy & security >
Windows Security > App & browser control) and factor that in before disabling it.
