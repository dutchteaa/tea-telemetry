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
