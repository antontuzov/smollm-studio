//! SmolLLM Studio desktop shell.
//!
//! The window is a thin surface over the Rust crates: commands start work and
//! events report progress, so the main thread never waits on a download, a
//! model load or a generation.

mod commands;
mod engine_kind;
mod events;
mod file_logs;
mod state;
mod tasks;

use std::sync::Arc;

use smollm_core::config::Settings;
use smollm_core::logs::{CaptureLayer, LogStore, DEFAULT_CAPACITY};
use smollm_core::AppPaths;
use state::AppState;
use tauri::{Builder, Manager};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Milliseconds since the Unix epoch, used for log and event timestamps.
pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// Install `tracing`: stdout in dev, the in-memory log store for the Logs page,
/// and daily rotating files under `logs/`.
fn install_logging(logs: &LogStore, logs_dir: std::path::PathBuf) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,reqwest=warn,tower_http=info"));
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .with(CaptureLayer::new(logs))
        .with(file_logs::FileLogLayer::new(logs_dir));
    // A second call would mean the app was started twice; ignore it quietly.
    let _ = registry.try_init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = AppPaths::default();
    let _ = paths.ensure();
    let logs = LogStore::new(DEFAULT_CAPACITY);
    install_logging(&logs, paths.logs_dir.clone());

    let settings = match Settings::load(&paths) {
        Ok(settings) => settings,
        Err(error) => {
            // A broken settings file must not stop the app from opening.
            tracing::warn!(%error, "settings could not be read; starting from defaults");
            Settings::default()
        }
    };

    Builder::default()
        // Native open/save panels: a transcript export and a model import both
        // need a real filesystem path, which the webview will not hand out.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            let (bootstrapped, receiver) = AppState::bootstrap(settings, logs)?;
            let engine = bootstrapped.engine()?.engine_name();
            let state = Arc::new(bootstrapped);
            app.manage(Arc::clone(&state));

            tasks::pump_downloads(handle, receiver);
            // A credential lookup waits on the OS agent — macOS asks the user to
            // allow the first access an unsigned build makes — so it happens on
            // the blocking pool, not between launch and the first window.
            {
                let state = Arc::clone(&state);
                tauri::async_runtime::spawn_blocking(move || {
                    let found = state.sync_hf_token();
                    tracing::info!(
                        target: "app",
                        configured = found,
                        "hugging face credential checked"
                    );
                });
            }
            tracing::info!(
                target: "app",
                version = %app.package_info().version,
                engine = %engine,
                "SmolLLM Studio started"
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::detect_hardware,
            commands::get_app_info,
            commands::get_doctor_report,
            commands::list_catalog_models,
            commands::catalog_facets,
            commands::list_local_models,
            commands::pull_model,
            commands::cancel_download,
            commands::retry_download,
            commands::get_download_snapshot,
            commands::delete_local_model,
            commands::import_model,
            commands::verify_local_models,
            commands::load_model,
            commands::unload_model,
            commands::get_engine_metrics,
            commands::start_chat_stream,
            commands::stop_generation,
            commands::new_chat_session,
            commands::save_chat_session,
            commands::list_chat_sessions,
            commands::search_chat_sessions,
            commands::get_chat_session,
            commands::rename_chat_session,
            commands::delete_chat_session,
            commands::export_chat_session,
            commands::start_server,
            commands::stop_server,
            commands::get_server_status,
            commands::get_server_examples,
            commands::run_benchmark,
            commands::get_logs,
            commands::clear_logs,
            commands::get_settings,
            commands::save_settings,
            commands::set_model_dir,
            commands::get_hf_token_status,
            commands::set_hf_token,
            commands::clear_hf_token,
            commands::get_presets,
            commands::open_model_folder,
            commands::open_log_folder,
            commands::reset_app_data,
            commands::export_diagnostics,
        ])
        .run(tauri::generate_context!())
        .expect("SmolLLM Studio could not start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_store_captures_tracing_events() {
        let store = LogStore::new(32);
        // The capture layer is the bridge between `tracing` and the Logs page.
        let layer = CaptureLayer::new(&store);
        let registry = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(registry, || {
            tracing::info!(target: "app", "shell test line");
        });
        let entries = store.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "shell test line");
        assert_eq!(entries[0].stream, smollm_core::system::LogStream::App);
    }
}
