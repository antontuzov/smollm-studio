//! Persisted user settings.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chat::SamplingParams;
use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;
use crate::system::Backend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// Light is how the app is designed to open; dark stays fully tuned.
    #[default]
    Light,
    Dark,
    System,
}

/// Everything the Settings page can change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: ThemeMode,
    /// `None` means the platform default from [`AppPaths`].
    pub model_dir: Option<String>,
    pub default_model_id: Option<String>,
    pub default_context_length: u32,
    pub default_gpu_layers: i32,
    pub default_backend: Backend,
    /// Decode threads for a load that does not name its own. `None` leaves the
    /// count to the engine, which uses every core it is allowed to see.
    pub default_threads: Option<u32>,
    pub sampling: SamplingParams,
    pub server_host: String,
    pub server_port: u16,
    /// Stored preference for a future opt-in update check. Nothing in this
    /// version reads it: the app makes no request except user-initiated
    /// downloads.
    pub auto_update_checks: bool,
    pub onboarding_complete: bool,
    pub chat_preset: String,
    /// Stop sequences a new transcript starts with, applied by the engine to
    /// every answer in it. Kept next to `chat_preset` because both are the
    /// starting state of a conversation rather than of one request.
    pub chat_stops: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            model_dir: None,
            default_model_id: None,
            default_context_length: 4096,
            default_gpu_layers: -1,
            default_backend: Backend::default(),
            default_threads: None,
            sampling: SamplingParams::default(),
            server_host: "127.0.0.1".to_string(),
            server_port: 8080,
            auto_update_checks: false,
            onboarding_complete: false,
            chat_preset: "balanced".to_string(),
            chat_stops: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load(paths: &AppPaths) -> AppResult<Self> {
        Self::load_at(&paths.settings_path)
    }

    pub fn load_at(path: &Path) -> AppResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        // Unknown keys from newer versions must not brick an install.
        serde_json::from_str::<Self>(&text).map_err(|source| {
            AppError::Config(format!(
                "{} is not valid settings JSON: {source}",
                path.display()
            ))
        })
    }

    pub fn save(&self, paths: &AppPaths) -> AppResult<()> {
        paths.ensure()?;
        self.save_at(&paths.settings_path)
    }

    pub fn save_at(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // Write-then-rename keeps a crash from truncating settings mid-save.
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }

    pub fn paths(&self) -> AppPaths {
        AppPaths::new(self.model_dir.as_ref().map(std::path::PathBuf::from))
    }

    pub fn server_config(&self) -> crate::system::ServerConfig {
        crate::system::ServerConfig {
            host: self.server_host.clone(),
            port: self.server_port,
            default_model_id: self.default_model_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_settings_path(name: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "smollm-settings-{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("settings.json")
    }

    #[test]
    fn round_trips_through_disk() {
        let path = temp_settings_path("round-trip");
        let settings = Settings {
            theme: ThemeMode::Light,
            server_port: 9099,
            model_dir: Some("/tmp/models".into()),
            ..Settings::default()
        };
        settings.save_at(&path).expect("saved");

        let loaded = Settings::load_at(&path).expect("loaded");
        assert_eq!(loaded.theme, ThemeMode::Light);
        assert_eq!(loaded.server_port, 9099);
        assert_eq!(loaded.model_dir.as_deref(), Some("/tmp/models"));
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let loaded = Settings::load_at(Path::new("/nonexistent/smollm/settings.json"))
            .expect("defaults when absent");
        assert_eq!(loaded.default_context_length, 4096);
        assert!(!loaded.onboarding_complete);
    }

    /// Settings written before a field existed have to keep loading: an install
    /// that predates thread counts and chat stop sequences still starts.
    #[test]
    fn an_older_file_fills_the_new_settings_from_defaults() {
        let path = temp_settings_path("older-version");
        std::fs::write(
            &path,
            r#"{"theme":"dark","defaultContextLength":2048,"sampling":{"temperature":0.3,"seed":null}}"#,
        )
        .expect("written");

        let loaded = Settings::load_at(&path).expect("loads without the newer keys");
        assert_eq!(loaded.default_context_length, 2048);
        assert_eq!(loaded.default_threads, None);
        assert!(loaded.chat_stops.is_empty());
        assert_eq!(loaded.sampling.seed, None);
    }

    #[test]
    fn corrupt_file_reports_config_error() {
        let path = temp_settings_path("corrupt");
        std::fs::write(&path, "{ not json").expect("written");
        let error = Settings::load_at(&path).expect_err("should fail");
        assert!(matches!(error, AppError::Config(_)));
    }
}
