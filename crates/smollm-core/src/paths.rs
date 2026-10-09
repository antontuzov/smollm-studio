//! Platform path resolution.
//!
//! Layout requested by the product spec:
//! - macOS: `~/Library/Application Support/SmolLLM Studio`
//! - Windows: `%APPDATA%/SmolLLM Studio`
//! - Linux: `~/.local/share/smollm-studio`
//!
//! `SMOLLM_STUDIO_DATA_DIR` overrides the root, which keeps tests and portable
//! installs hermetic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const APP_FOLDER_NAME: &str = if cfg!(target_os = "linux") {
    "smollm-studio"
} else {
    "SmolLLM Studio"
};

/// Resolved on-disk layout used by the whole app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub models_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub settings_path: PathBuf,
    /// One JSON file per chat transcript, so quitting loses nothing.
    pub sessions_dir: PathBuf,
}

impl AppPaths {
    /// Build the layout, optionally overriding only the model directory.
    pub fn new(models_dir: Option<PathBuf>) -> Self {
        let data_dir = data_dir();
        let models_dir = models_dir.unwrap_or_else(|| data_dir.join("models"));
        let logs_dir = data_dir.join("logs");
        let settings_path = data_dir.join("settings.json");
        let sessions_dir = data_dir.join("sessions");
        Self {
            data_dir,
            models_dir,
            logs_dir,
            settings_path,
            sessions_dir,
        }
    }

    /// Create missing directories; safe to call repeatedly.
    pub fn ensure(&self) -> AppResult<()> {
        for dir in [
            &self.data_dir,
            &self.models_dir,
            &self.logs_dir,
            &self.sessions_dir,
        ] {
            std::fs::create_dir_all(dir).map_err(|source| {
                AppError::Config(format!("cannot create {}: {source}", dir.display()))
            })?;
        }
        Ok(())
    }

    pub fn model_file(&self, file_name: &str) -> PathBuf {
        self.models_dir.join(file_name)
    }

    pub fn is_model_path(&self, path: &Path) -> bool {
        path.starts_with(&self.models_dir)
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Root data directory for the app, per platform.
pub fn data_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("SMOLLM_STUDIO_DATA_DIR") {
        let path = PathBuf::from(override_dir);
        if path.is_absolute() {
            return path;
        }
    }

    #[cfg(target_os = "macos")]
    let base = home_dir().map(|home| home.join("Library").join("Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".local").join("share")));

    base.unwrap_or_else(|| {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(format!(".{APP_FOLDER_NAME}"))
    })
    .join(APP_FOLDER_NAME)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_only_for_absolute_paths() {
        let absolute = std::env::temp_dir();
        std::env::set_var("SMOLLM_STUDIO_DATA_DIR", &absolute);
        assert_eq!(data_dir(), absolute);

        std::env::set_var("SMOLLM_STUDIO_DATA_DIR", "relative/path");
        assert!(!data_dir().ends_with("relative/path"));

        std::env::remove_var("SMOLLM_STUDIO_DATA_DIR");
    }

    #[test]
    fn paths_are_nested_under_data_dir() {
        let paths = AppPaths::new(None);
        assert!(paths.models_dir.starts_with(&paths.data_dir));
        assert!(paths.logs_dir.starts_with(&paths.data_dir));
        assert!(paths.sessions_dir.starts_with(&paths.data_dir));
        assert_eq!(paths.settings_path.parent(), Some(paths.data_dir.as_path()));
        assert_eq!(paths.model_file("a.gguf"), paths.models_dir.join("a.gguf"));
    }

    #[test]
    fn custom_model_dir_is_respected() {
        let custom = std::env::temp_dir().join("smollm-custom-models");
        let paths = AppPaths::new(Some(custom.clone()));
        assert_eq!(paths.models_dir, custom);
        assert!(paths.is_model_path(&custom.join("x.gguf")));
        assert!(!paths.is_model_path(Path::new("/tmp/other/x.gguf")));
    }
}
