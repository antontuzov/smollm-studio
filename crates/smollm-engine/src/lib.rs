//! Engine abstraction layer.
//!
//! One trait, several backends. The app only ever talks to [`Engine`], so
//! MockEngine can be swapped for a real llama.cpp or Candle adapter without any
//! UI, server or download code changing. llama.cpp and Candle are feature
//! flagged and ship as clearly marked adapters: the first release must not
//! block on native bindings.

use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};
use smollm_core::chat::{
    EngineMetrics, GenToken, GenerationRequest, LoadModelRequest, ModelHandle,
};
use smollm_core::{AppError, AppResult};

pub mod benchmark;
#[cfg(feature = "candle")]
pub mod candle;
pub mod gguf_meta;
#[cfg(feature = "llama-cpp")]
pub mod llama;
pub mod manager;
#[cfg(feature = "mock")]
pub mod mock;

pub use benchmark::{run as run_benchmark, BenchmarkConfig, BenchmarkProgress, BenchmarkResult};
pub use gguf_meta::GgufMetadataEngine;
pub use manager::EngineManager;

/// A stream of generated tokens. Errors are terminal: the consumer stops.
pub type TokenStream = Pin<Box<dyn Stream<Item = AppResult<GenToken>> + Send>>;

/// Anything that can load a model and stream tokens.
///
/// Implementations must not block the calling thread for long: the Tauri layer
/// calls these methods from async commands.
pub trait Engine: Send + Sync {
    /// Stable, human readable engine name shown in the UI and `/v1/models`.
    fn name(&self) -> &str;

    /// Whether this engine can actually run a GGUF file.
    fn supports_gguf(&self) -> bool;

    /// Load a model and return the handle the rest of the app uses.
    fn load_model(&mut self, request: LoadModelRequest) -> AppResult<ModelHandle>;

    /// Release the model. Implementations may keep the file mmap'd.
    fn unload_model(&mut self, handle: ModelHandle) -> AppResult<()>;

    /// Start a generation. The returned stream must honour
    /// [`GenerationRequest::cancel`] promptly.
    fn generate(&mut self, request: GenerationRequest) -> AppResult<TokenStream>;

    /// Tokenise text the way this engine's model would.
    fn tokenize(&self, text: &str) -> AppResult<Vec<u32>>;

    /// Counters for the metrics panel and `/v1/engine/metrics`.
    fn metrics(&self) -> EngineMetrics;

    /// True when the engine fakes inference rather than running a model.
    fn is_simulated(&self) -> bool {
        true
    }
}

/// Engines the app knows how to construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    Mock,
    LlamaCpp,
    Candle,
    /// Metadata-only backend: reads GGUF headers, cannot generate.
    GgufMetadata,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::LlamaCpp => "llama-cpp",
            Self::Candle => "candle",
            Self::GgufMetadata => "gguf-metadata",
        }
    }

    /// Accepts the labels used in settings, engine names and CLI flags.
    pub fn parse(value: &str) -> Option<Self> {
        let normalised = value.trim().to_ascii_lowercase().replace('_', "-");
        match normalised.as_str() {
            "mock" | "mockengine" | "simulation" => Some(Self::Mock),
            "llama-cpp" | "llamacpp" | "llama" => Some(Self::LlamaCpp),
            "candle" => Some(Self::Candle),
            "gguf-metadata" | "metadata" | "gguf" => Some(Self::GgufMetadata),
            _ => None,
        }
    }

    /// Whether this build actually contains the engine.
    pub fn is_available(self) -> bool {
        match self {
            Self::Mock => cfg!(feature = "mock"),
            Self::LlamaCpp => cfg!(feature = "llama-cpp"),
            Self::Candle => cfg!(feature = "candle"),
            Self::GgufMetadata => true,
        }
    }

    /// Engines offered in the UI: available ones first, unavailable ones last.
    pub fn all() -> Vec<Self> {
        let mut kinds = [Self::Mock, Self::LlamaCpp, Self::Candle, Self::GgufMetadata];
        kinds.sort_by_key(|kind| !kind.is_available());
        kinds.to_vec()
    }

    pub fn build(self) -> Box<dyn Engine> {
        match self {
            #[cfg(feature = "mock")]
            Self::Mock => Box::new(mock::MockEngine::default()),
            #[cfg(feature = "llama-cpp")]
            Self::LlamaCpp => Box::new(llama::LlamaCppEngine::default()),
            #[cfg(feature = "candle")]
            Self::Candle => Box::new(candle::CandleEngine::default()),
            // Metadata reading is always available, and is the honest fallback
            // when a requested backend was not compiled into this build.
            _ => Box::new(GgufMetadataEngine::default()),
        }
    }

    /// Error surfaced when a caller asks for an engine this binary lacks.
    pub fn unavailable_error(self) -> AppError {
        AppError::UnsupportedBackend(format!(
            "{} engine was not compiled into this build (cargo feature `{}`)",
            self.as_str(),
            self.as_str()
        ))
    }
}

/// The engine the app starts with: Mock keeps the whole UI usable everywhere.
pub fn default_engine() -> Box<dyn Engine> {
    EngineKind::Mock.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_kind_parsing_accepts_common_spellings() {
        assert_eq!(EngineKind::parse("mock"), Some(EngineKind::Mock));
        assert_eq!(EngineKind::parse("LLAMA_CPP"), Some(EngineKind::LlamaCpp));
        assert_eq!(EngineKind::parse(" llama-cpp "), Some(EngineKind::LlamaCpp));
        assert_eq!(EngineKind::parse("candle"), Some(EngineKind::Candle));
        assert_eq!(
            EngineKind::parse("gguf-metadata"),
            Some(EngineKind::GgufMetadata)
        );
        assert_eq!(EngineKind::parse("triton"), None);
    }

    #[test]
    fn default_engine_is_constructible() {
        let engine = default_engine();
        assert!(!engine.name().is_empty());
        assert!(engine.is_simulated());
        assert!(engine.metrics().simulated);
    }

    #[test]
    fn every_kind_builds_something() {
        for kind in EngineKind::all() {
            let engine = kind.build();
            assert!(!engine.name().is_empty(), "{kind:?} needs a name");
        }
    }

    #[test]
    fn metadata_engine_is_always_available() {
        assert!(EngineKind::GgufMetadata.is_available());
        let error = EngineKind::LlamaCpp.unavailable_error();
        assert!(matches!(error, AppError::UnsupportedBackend(_)));
    }
}
