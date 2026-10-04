//! Shared state for the HTTP layer and the desktop app.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use smollm_core::logs::{LogStore, DEFAULT_CAPACITY};
use smollm_core::system::{LogEntry, LogStream, ServerConfig};
use smollm_core::{AppError, AppResult};
use smollm_engine::EngineManager;

/// One manager, many entry points: the desktop app and the HTTP server drive the
/// same loaded model, so `/v1/models` always matches the Chat page.
pub struct ServerState {
    pub engine: Arc<Mutex<EngineManager>>,
    pub logs: LogStore,
    pub config: ServerConfig,
    pub app_version: String,
    requests: AtomicU64,
    started_ms: AtomicU64,
    /// Model ids advertised on `/v1/models`: loaded first, then local library.
    advertised: RwLock<Vec<String>>,
}

impl ServerState {
    pub fn new(
        engine: Arc<Mutex<EngineManager>>,
        config: ServerConfig,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            engine,
            logs: LogStore::new(DEFAULT_CAPACITY),
            config,
            app_version: app_version.into(),
            requests: AtomicU64::new(0),
            started_ms: AtomicU64::new(0),
            advertised: RwLock::new(Vec::new()),
        }
    }

    /// With an explicit log store, so the app's Logs page sees server traffic.
    pub fn with_logs(mut self, logs: LogStore) -> Self {
        self.logs = logs;
        self
    }

    pub fn record_request(&self) -> u64 {
        self.requests.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn request_count(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn mark_started(&self) {
        self.started_ms.store(now_millis(), Ordering::Relaxed);
    }

    pub fn uptime_seconds(&self) -> u64 {
        let started = self.started_ms.load(Ordering::Relaxed);
        if started == 0 {
            return 0;
        }
        now_millis().saturating_sub(started) / 1000
    }

    /// Replace the advertised model list, e.g. after a library scan.
    pub fn advertise(&self, model_ids: &[String]) {
        let mut current = self.write_advertised();
        current.clear();
        for id in model_ids {
            if !id.is_empty() && !current.iter().any(|seen| seen == id) {
                current.push(id.clone());
            }
        }
    }

    pub fn advertised_models(&self) -> Vec<String> {
        self.read_advertised().clone()
    }

    /// Model ids offered to clients, always starting with the loaded model.
    pub fn served_model_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Ok(guard) = self.engine.lock() {
            if let Some(handle) = guard.loaded_handle() {
                ids.push(handle.model_id);
            }
        }
        for id in self.read_advertised().iter() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    /// Map a client's `model` onto something this server can actually serve.
    ///
    /// `None`, `""`, `"default"` and `"auto"` all mean "whatever is loaded",
    /// which is what most single-model OpenAI clients send.
    pub fn resolve_model(&self, requested: Option<&str>) -> AppResult<String> {
        let trimmed = requested.unwrap_or_default().trim();
        let general = matches!(trimmed, "" | "default" | "auto");
        let served = self.served_model_ids();
        let loaded = served.first().cloned();

        if general {
            return loaded
                .or_else(|| self.config.default_model_id.clone())
                .ok_or_else(|| {
                    AppError::ModelNotFound("no model is loaded; load one in the app".to_string())
                });
        }

        if served.iter().any(|id| id == trimmed) {
            return Ok(trimmed.to_string());
        }
        if self
            .config
            .default_model_id
            .as_deref()
            .is_some_and(|id| id == trimmed)
        {
            return Ok(trimmed.to_string());
        }

        Err(AppError::ModelNotFound(format!(
            "model `{trimmed}` is not served here; available: {}",
            if served.is_empty() {
                "none loaded".to_string()
            } else {
                served.join(", ")
            }
        )))
    }

    /// Append a line to the server log stream shown on the Server page.
    pub fn log(&self, level: &str, message: impl Into<String>) {
        let message = message.into();
        self.logs.push(LogEntry {
            timestamp_ms: now_millis(),
            level: level.to_string(),
            target: "smollm_server".to_string(),
            stream: LogStream::Server,
            message: message.clone(),
        });
        match level {
            "error" => tracing::error!("{message}"),
            "warn" => tracing::warn!("{message}"),
            "debug" => tracing::debug!("{message}"),
            _ => tracing::info!("{message}"),
        }
    }

    fn read_advertised(&self) -> std::sync::RwLockReadGuard<'_, Vec<String>> {
        match self.advertised.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn write_advertised(&self) -> std::sync::RwLockWriteGuard<'_, Vec<String>> {
        match self.advertised.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

pub type SharedState = Arc<ServerState>;

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_engine::EngineKind;

    fn state(config: ServerConfig) -> ServerState {
        ServerState::new(
            Arc::new(Mutex::new(EngineManager::with_kind(EngineKind::Mock))),
            config,
            "0.1.0",
        )
    }

    #[test]
    fn request_counter_is_monotonic() {
        let state = state(ServerConfig::default());
        assert_eq!(state.request_count(), 0);
        assert_eq!(state.record_request(), 1);
        assert_eq!(state.record_request(), 2);
    }

    #[test]
    fn uptime_is_zero_until_started() {
        let state = state(ServerConfig::default());
        assert_eq!(state.uptime_seconds(), 0);
        state.mark_started();
        assert!(state.uptime_seconds() < 5);
    }

    #[test]
    fn advertised_models_are_deduplicated() {
        let state = state(ServerConfig::default());
        state.advertise(&[
            "a-gguf".to_string(),
            "a-gguf".to_string(),
            String::new(),
            "b-gguf".to_string(),
        ]);
        assert_eq!(state.advertised_models(), vec!["a-gguf", "b-gguf"]);
    }

    #[test]
    fn model_resolution_prefers_the_loaded_model() {
        let state = state(ServerConfig::default());
        {
            let mut manager = state.engine.lock().expect("unlocked");
            manager
                .load(smollm_core::chat::LoadModelRequest {
                    model_id: "qwen2.5-0.5b-instruct-gguf".to_string(),
                    ..Default::default()
                })
                .expect("mock loads");
        }
        assert_eq!(
            state.resolve_model(None).expect("resolves"),
            "qwen2.5-0.5b-instruct-gguf"
        );
        assert_eq!(
            state.resolve_model(Some("default")).expect("resolves"),
            "qwen2.5-0.5b-instruct-gguf"
        );
        assert_eq!(
            state
                .resolve_model(Some("qwen2.5-0.5b-instruct-gguf"))
                .expect("resolves"),
            "qwen2.5-0.5b-instruct-gguf"
        );
        let error = state
            .resolve_model(Some("gpt-4o"))
            .expect_err("unknown model");
        assert!(matches!(error, AppError::ModelNotFound(_)));
        assert!(error.to_string().contains("gpt-4o"));
    }

    #[test]
    fn model_resolution_falls_back_to_the_configured_default() {
        let config = ServerConfig {
            default_model_id: Some("smollm2-135m-gguf".to_string()),
            ..ServerConfig::default()
        };
        let state = state(config);
        assert_eq!(
            state.resolve_model(None).expect("default"),
            "smollm2-135m-gguf"
        );
        assert_eq!(
            state.resolve_model(Some("smollm2-135m-gguf")).expect("ok"),
            "smollm2-135m-gguf"
        );
    }

    #[test]
    fn resolution_errors_when_nothing_is_available() {
        let state = state(ServerConfig::default());
        assert!(state.resolve_model(Some("x")).is_err());
        assert!(state.resolve_model(None).is_err());
    }

    #[test]
    fn log_lines_land_in_the_server_stream() {
        let state = state(ServerConfig::default());
        state.log("info", "GET /health 200");
        state.log("error", "boom");
        let entries = state.logs.snapshot();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "GET /health 200");
        assert_eq!(entries[1].level, "error");
        assert!(entries
            .iter()
            .all(|entry| entry.stream == LogStream::Server));
    }
}
