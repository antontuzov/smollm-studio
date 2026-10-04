use serde::Serialize;

/// Structured, machine-readable error shared by every layer.
///
/// The frontend receives `{ code, message, detail }` so it can render a
/// friendly message while the technical text stays available for logs.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Model is not available locally: {0}")]
    ModelNotDownloaded(String),

    #[error("Download failed: {0}")]
    DownloadFailed(String),

    #[error("Download cancelled: {0}")]
    DownloadCancelled(String),

    #[error("Engine failed to load model: {0}")]
    EngineLoadFailed(String),

    #[error("Generation failed: {0}")]
    GenerationFailed(String),

    #[error("Insufficient memory: required {required_gb:.1} GB, available {available_gb:.1} GB")]
    InsufficientMemory { required_gb: f64, available_gb: f64 },

    #[error(
        "Insufficient disk space: required {required_gb:.1} GB, available {available_gb:.1} GB"
    )]
    InsufficientDiskSpace { required_gb: f64, available_gb: f64 },

    #[error("Internal error: {0}")]
    Internal(&'static str),

    #[error("Unsupported platform backend: {0}")]
    UnsupportedBackend(String),

    #[error("Not implemented yet: {0}")]
    NotImplemented(&'static str),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Server is already running on {0}")]
    ServerAlreadyRunning(String),

    #[error("Server is not running")]
    ServerNotRunning,

    #[error("Unsupported GGUF file: {0}")]
    GgufParse(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Wire representation of [`AppError`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    /// Stable, machine-readable discriminator (e.g. `model_not_found`).
    pub code: &'static str,
    /// Human readable, already-localised-Enough sentence.
    pub message: String,
    /// Technical detail for the logs view.
    pub detail: Option<String>,
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ModelNotFound(_) => "model_not_found",
            Self::ModelNotDownloaded(_) => "model_not_downloaded",
            Self::DownloadFailed(_) => "download_failed",
            Self::DownloadCancelled(_) => "download_cancelled",
            Self::EngineLoadFailed(_) => "engine_load_failed",
            Self::GenerationFailed(_) => "generation_failed",
            Self::InsufficientMemory { .. } => "insufficient_memory",
            Self::InsufficientDiskSpace { .. } => "insufficient_disk_space",
            Self::Internal(_) => "internal_error",
            Self::UnsupportedBackend(_) => "unsupported_backend",
            Self::NotImplemented(_) => "not_implemented",
            Self::InvalidRequest(_) => "invalid_request",
            Self::ServerAlreadyRunning(_) => "server_already_running",
            Self::ServerNotRunning => "server_not_running",
            Self::GgufParse(_) => "gguf_parse",
            Self::Config(_) => "config_error",
            Self::Io(_) => "io_error",
            Self::Json(_) => "json_error",
        }
    }

    pub fn to_payload(&self) -> ErrorPayload {
        ErrorPayload {
            code: self.code(),
            message: self.to_string(),
            detail: std::error::Error::source(self).map(ToString::to_string),
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_payload().serialize(serializer)
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_keeps_code_and_message() {
        let error = AppError::InsufficientMemory {
            required_gb: 8.0,
            available_gb: 4.0,
        };
        let payload = error.to_payload();
        assert_eq!(payload.code, "insufficient_memory");
        assert!(payload.message.contains("required 8.0 GB"));

        let json = serde_json::to_value(&error).expect("serialises");
        assert_eq!(json["code"], "insufficient_memory");
        assert!(json["message"].is_string());
    }
}
