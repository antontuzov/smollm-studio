//! Managed state shared by every Tauri command.
//!
//! The engine lives behind one `Arc<Mutex<EngineManager>>` so the desktop app
//! and the local HTTP server drive the *same* loaded model: a chat in the UI and
//! an OpenAI request on `127.0.0.1` cannot diverge into two copies of a model.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use smollm_core::config::Settings;
use smollm_core::logs::LogStore;
use smollm_core::model::Relocation;
use smollm_core::paths::AppPaths;
use smollm_core::system::{HardwareReport, ServerConfig};
use smollm_core::{AppError, AppResult};
use smollm_engine::EngineManager;
use smollm_models::catalog::ModelCatalog;
use smollm_models::download::{DownloadEvent, DownloadManager};
use smollm_models::hf::HfClient;
use smollm_models::library::ModelLibrary;
use smollm_server::{ServerHandle, SharedState};
use tokio::sync::mpsc;

use crate::engine_kind::kind_for_backend;

/// The HTTP server, the state object its routes were built from, and the flag
/// that tells the traffic watcher to exit.
#[derive(Default)]
pub struct ServerSlot {
    pub handle: Option<ServerHandle>,
    pub state: Option<SharedState>,
    pub running: Arc<AtomicBool>,
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub paths: Mutex<AppPaths>,
    pub catalog: ModelCatalog,
    pub engine: Arc<Mutex<EngineManager>>,
    pub downloads: Mutex<DownloadManager>,
    pub download_sink: mpsc::UnboundedSender<DownloadEvent>,
    pub logs: LogStore,
    pub server: Mutex<ServerSlot>,
    pub hardware: Mutex<Option<HardwareReport>>,
}

impl AppState {
    /// Build the app state from persisted settings, creating data directories.
    pub fn bootstrap(
        settings: Settings,
        logs: LogStore,
    ) -> AppResult<(Self, mpsc::UnboundedReceiver<DownloadEvent>)> {
        let paths = settings.paths();
        paths.ensure()?;
        let engine_kind = kind_for_backend(settings.default_backend);
        let (sink, receiver) = mpsc::unbounded_channel();
        Ok((
            Self {
                downloads: Mutex::new(DownloadManager::new(
                    paths.clone(),
                    HfClient::new(),
                    sink.clone(),
                )),
                paths: Mutex::new(paths),
                settings: Mutex::new(settings),
                catalog: ModelCatalog::embedded(),
                engine: Arc::new(Mutex::new(EngineManager::with_kind(engine_kind))),
                download_sink: sink,
                logs,
                server: Mutex::new(ServerSlot::default()),
                hardware: Mutex::new(None),
            },
            receiver,
        ))
    }

    /// Poison-tolerant lock: a panic inside one command must not brick the app,
    /// so the guard is recovered and the incident is logged.
    pub fn lock<'a, T>(mutex: &'a Mutex<T>, what: &str) -> AppResult<MutexGuard<'a, T>> {
        match mutex.lock() {
            Ok(guard) => Ok(guard),
            Err(poisoned) => {
                tracing::warn!(lock = what, "recovered a poisoned app state lock");
                Ok(poisoned.into_inner())
            }
        }
    }

    pub fn settings(&self) -> AppResult<Settings> {
        Ok(Self::lock(&self.settings, "settings")?.clone())
    }

    pub fn paths(&self) -> AppPaths {
        self.paths
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| AppPaths::default())
    }

    pub fn engine(&self) -> AppResult<MutexGuard<'_, EngineManager>> {
        Self::lock(&self.engine, "engine")
    }

    pub fn library(&self) -> ModelLibrary {
        ModelLibrary::new(self.paths())
    }

    pub fn downloads(&self) -> AppResult<DownloadManager> {
        Ok(Self::lock(&self.downloads, "downloads")?.clone())
    }

    /// Remember a started server; returns the watcher's running flag.
    pub fn store_server(
        &self,
        handle: ServerHandle,
        state: SharedState,
    ) -> AppResult<Arc<AtomicBool>> {
        let mut slot = Self::lock(&self.server, "server")?;
        slot.running = Arc::new(AtomicBool::new(true));
        slot.handle = Some(handle);
        slot.state = Some(state);
        Ok(Arc::clone(&slot.running))
    }

    /// Take the handle so the caller can await a graceful stop.
    pub fn take_server(&self) -> AppResult<Option<ServerHandle>> {
        let mut slot = Self::lock(&self.server, "server")?;
        slot.running.store(false, Ordering::Relaxed);
        Ok(slot.handle.take())
    }

    pub fn clear_server_state(&self) {
        if let Ok(mut slot) = self.server.lock() {
            slot.state = None;
            slot.running.store(false, Ordering::Relaxed);
        }
    }
    pub fn server_state(&self) -> Option<SharedState> {
        self.server.lock().ok().and_then(|slot| slot.state.clone())
    }

    pub fn server_is_running(&self) -> bool {
        self.server
            .lock()
            .ok()
            .and_then(|slot| slot.handle.as_ref().map(ServerHandle::is_running))
            .unwrap_or(false)
    }

    pub fn cached_hardware(&self) -> Option<HardwareReport> {
        self.hardware.lock().ok().and_then(|cache| cache.clone())
    }

    pub fn store_hardware(&self, report: HardwareReport) {
        if let Ok(mut cache) = self.hardware.lock() {
            *cache = Some(report);
        }
    }

    /// Move the engine to a different backend, refusing to drop live work.
    ///
    /// The `Arc` identity is preserved so a running server keeps pointing at the
    /// same manager.
    pub fn use_engine_kind(&self, kind: smollm_engine::EngineKind) -> AppResult<()> {
        let mut manager = self.engine()?;
        if manager.engine_name() == kind.as_str() {
            return Ok(());
        }
        if manager.active_requests() > 0 || self.server_is_running() {
            return Err(AppError::InvalidRequest(format!(
                "stop the local server and any active generations before switching to the {} engine",
                kind.as_str()
            )));
        }
        *manager = EngineManager::with_kind(kind);
        Ok(())
    }

    /// Point the app at a new model directory: paths, library and downloads.
    ///
    /// With `migrate`, everything the old folder holds is relocated into the new
    /// one first. Without it the app simply looks elsewhere, which leaves the
    /// models behind in a folder nothing reads — fine for a deliberate re-point,
    /// surprising as the answer to "move my models".
    ///
    /// A migration refuses to run while it could break live work: a resident
    /// model would lose the file the engine has open, and a transfer mid-flight
    /// would resume against a folder that no longer holds its bytes.
    pub fn apply_model_dir(
        &self,
        model_dir: Option<String>,
        migrate: bool,
    ) -> AppResult<(AppPaths, Option<Relocation>)> {
        let paths = AppPaths::new(model_dir.map(std::path::PathBuf::from));
        let previous_dir = self.paths().models_dir;
        let moving = migrate && paths.models_dir != previous_dir;
        if moving {
            self.guard_model_dir_move()?;
        }
        paths.ensure()?;
        let relocation = moving
            .then(|| ModelLibrary::new(paths.clone()).relocate_from(&previous_dir))
            .transpose()?;

        let manager =
            DownloadManager::new(paths.clone(), HfClient::new(), self.download_sink.clone());
        if let Ok(mut guard) = self.downloads.lock() {
            *guard = manager;
        }
        if let Ok(mut guard) = self.paths.lock() {
            *guard = paths.clone();
        }
        Ok((paths, relocation))
    }

    /// Why a model folder cannot be moved right now, if that is the case.
    fn guard_model_dir_move(&self) -> AppResult<()> {
        if self.engine()?.loaded_handle().is_some() {
            return Err(AppError::InvalidRequest(
                "unload the model before moving the model folder".into(),
            ));
        }
        if self.downloads()?.has_active() {
            return Err(AppError::InvalidRequest(
                "a download is still running; wait for it or cancel it first".into(),
            ));
        }
        Ok(())
    }

    pub fn server_config(&self) -> AppResult<ServerConfig> {
        let settings = self.settings()?;
        Ok(ServerConfig {
            host: settings.server_host.clone(),
            port: settings.server_port,
            default_model_id: settings.default_model_id.clone(),
        })
    }

    /// Effective listen configuration: an explicit request wins over settings.
    pub fn server_config_for(&self, requested: Option<ServerConfig>) -> AppResult<ServerConfig> {
        match requested {
            Some(config) => Ok(config),
            None => self.server_config(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::chat::LoadModelRequest;
    use smollm_engine::EngineKind;

    fn temp_state(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!(
            "smollm-state-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let settings = Settings {
            model_dir: Some(dir.display().to_string()),
            // Mock keeps the assertion independent of which native engines this
            // build happens to compile in.
            default_backend: smollm_core::system::Backend::Mock,
            ..Settings::default()
        };
        let (state, _receiver) =
            AppState::bootstrap(settings, LogStore::new(64)).expect("bootstraps");
        state
    }

    /// A move target that belongs to no previous run: reusing a fixed folder
    /// would make a leftover file from an earlier test a duplicate, not a move.
    fn temp_target(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "smollm-target-{}-{name}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn bootstrap_creates_dirs_and_a_mock_engine() {
        let state = temp_state("bootstrap");
        let paths = state.paths();
        assert!(paths.models_dir.exists(), "model dir is created");
        assert_eq!(state.engine().expect("engine").engine_name(), "mock");
        assert_eq!(state.server_config().expect("config").port, 8080);
        assert_eq!(
            state.downloads().expect("downloads").model_dir(),
            paths.models_dir
        );
    }

    #[test]
    fn switching_engine_kind_keeps_the_same_shared_manager_slot() {
        let state = temp_state("switch-kind");
        let shared = Arc::clone(&state.engine);
        state
            .use_engine_kind(EngineKind::Mock)
            .expect("no-op switch");
        assert!(
            Arc::ptr_eq(&state.engine, &shared),
            "the Arc identity stays"
        );

        state
            .use_engine_kind(EngineKind::GgufMetadata)
            .expect("switches");
        assert_eq!(
            shared
                .lock()
                .map(|manager| manager.engine_name())
                .unwrap_or_default(),
            "gguf-metadata"
        );
        // The old handle sees the new engine, which is what the server relies on.
        assert_eq!(
            state.engine().expect("engine").engine_name(),
            "gguf-metadata"
        );
    }

    #[test]
    fn applying_a_model_dir_rebuilds_paths_and_downloads() {
        let state = temp_state("model-dir");
        let moved = temp_target("elsewhere").display().to_string();
        let (paths, relocation) = state
            .apply_model_dir(Some(moved.clone()), false)
            .expect("applied");
        assert_eq!(paths.models_dir.display().to_string(), format!("{moved}"));
        assert!(paths.models_dir.exists());
        assert!(
            relocation.is_none(),
            "a plain re-point does not touch any file"
        );
        assert_eq!(
            state.downloads().expect("downloads").model_dir(),
            paths.models_dir
        );
    }

    #[test]
    fn a_model_dir_move_carries_the_files_into_the_new_folder() {
        let state = temp_state("model-move");
        let previous = state.paths().models_dir.clone();
        std::fs::write(previous.join("alpha.gguf"), b"model bytes").expect("write");
        std::fs::write(previous.join("beta.gguf.part"), b"half a model").expect("write");
        let target = temp_target("moved-models");

        let (paths, relocation) = state
            .apply_model_dir(Some(target.display().to_string()), true)
            .expect("moved");
        let report = relocation.expect("a move reports what it did");

        assert_eq!(report.moved, 2, "{report:?}");
        assert_eq!(report.copied, 0);
        assert!(report.is_complete(), "{report:?}");
        assert!(paths.models_dir.join("alpha.gguf").exists());
        assert!(
            paths.models_dir.join("beta.gguf.part").exists(),
            "a paused download moves with the folder it belongs to"
        );
        assert!(
            !previous.join("alpha.gguf").exists(),
            "and the old folder does not keep a second copy"
        );
        assert_eq!(
            state.downloads().expect("downloads").model_dir(),
            paths.models_dir,
            "the download manager follows the move"
        );
    }

    #[test]
    fn a_move_is_refused_while_live_work_would_break() {
        let state = temp_state("model-move-guard");
        let previous = state.paths().models_dir.clone();
        let target = temp_target("somewhere-else");

        state
            .engine()
            .expect("engine")
            .load(LoadModelRequest {
                model_id: "resident".into(),
                ..LoadModelRequest::default()
            })
            .expect("the mock engine takes anything");

        let error = state
            .apply_model_dir(Some(target.display().to_string()), true)
            .expect_err("a resident model's file must not be moved out from under it");
        assert!(matches!(error, AppError::InvalidRequest(_)), "{error:?}");
        assert!(error.to_string().contains("unload the model"), "{error}");
        assert_eq!(
            state.paths().models_dir,
            previous,
            "a refused move leaves the app where it was"
        );

        state.engine().expect("engine").unload().expect("unloaded");
        state
            .apply_model_dir(Some(target.display().to_string()), true)
            .expect("an empty library moves without argument");
    }

    #[test]
    fn server_state_is_absent_until_started() {
        let state = temp_state("no-server");
        assert!(state.server_state().is_none());
        assert!(!state.server_is_running());
    }
}
