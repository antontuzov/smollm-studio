//! Engine abstraction layer.
//!
//! One trait, several backends. The app only ever talks to [`Engine`], so
//! MockEngine can be swapped for a real llama.cpp or Candle adapter without any
//! UI, server or download code changing. llama.cpp links for real behind the
//! `llama-cpp` feature; Candle is still a clearly marked adapter. Mock stays the
//! default so the app runs on any machine, with or without a native toolchain.

use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};
use smollm_core::chat::{
    ChatMessage, EngineMetrics, GenToken, GenerationRequest, LoadModelRequest, ModelHandle,
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

/// The accelerator the linked engine says it can offload weights to.
///
/// `smollm-hardware` deliberately measures a machine without a GPU runtime, so
/// the honest answer about offload has to come from the engine that would do it.
/// Apps merge this into [`smollm_core::system::HardwareReport`]; a build with no
/// native engine has nothing to ask and reports `None`.
#[cfg(feature = "llama-cpp")]
pub use llama::largest_accelerator;

#[cfg(not(feature = "llama-cpp"))]
#[must_use]
pub fn largest_accelerator() -> Option<smollm_core::system::Accelerator> {
    None
}

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

    /// Flatten a transcript into the prompt this engine's model expects.
    ///
    /// Engines that ship a real template (llama.cpp reads it out of the GGUF
    /// file) answer here; `None` means the manager falls back to the
    /// architecture table in `smollm_core::model::ModelFamily`.
    fn render_chat_prompt(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
    ) -> Option<String> {
        let _ = (system_prompt, messages);
        None
    }

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

    /// Whether this build actually contains the engine's code.
    pub fn compiled(self) -> bool {
        match self {
            Self::Mock => cfg!(feature = "mock"),
            Self::LlamaCpp => cfg!(feature = "llama-cpp"),
            Self::Candle => cfg!(feature = "candle"),
            Self::GgufMetadata => true,
        }
    }

    /// Whether a compiled engine has the native code it needs to run.
    ///
    /// `mock` and the metadata reader need none. `llama-cpp` builds llama.cpp
    /// itself, so once compiled it is linked too. The `candle` feature compiles
    /// an adapter without a library behind it, so being compiled there is not
    /// the same as being able to answer a request.
    pub fn is_linked(self) -> bool {
        match self {
            Self::Mock => cfg!(feature = "mock"),
            Self::GgufMetadata => true,
            #[cfg(feature = "llama-cpp")]
            Self::LlamaCpp => llama::LlamaCppEngine::is_linked(),
            #[cfg(not(feature = "llama-cpp"))]
            Self::LlamaCpp => false,
            #[cfg(feature = "candle")]
            Self::Candle => candle::CandleEngine::is_linked(),
            #[cfg(not(feature = "candle"))]
            Self::Candle => false,
        }
    }

    /// Whether an engine in this build can actually serve a request.
    ///
    /// Callers choose an engine with this, not with [`Self::compiled`]: picking
    /// an unlinked adapter over Mock would leave the app unable to load anything.
    pub fn is_available(self) -> bool {
        self.compiled() && self.is_linked()
    }

    /// Why `is_available` is false, in the words every surface shows.
    pub fn unavailability(self) -> Option<&'static str> {
        if self.is_available() {
            return None;
        }
        Some(if !self.compiled() {
            "was not compiled into this build"
        } else {
            "is compiled as an adapter only: no native library is linked yet"
        })
    }

    /// Engines offered in the UI: available ones first, unavailable ones last.
    pub fn all() -> Vec<Self> {
        let mut kinds = [Self::Mock, Self::LlamaCpp, Self::Candle, Self::GgufMetadata];
        kinds.sort_by_key(|kind| !kind.is_available());
        kinds.to_vec()
    }

    /// Construct an engine, falling back to the metadata reader.
    ///
    /// This builds whatever the feature flags contain, including an adapter that
    /// cannot run — check [`Self::is_available`] before choosing with it.
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

    /// Error surfaced when a caller asks for an engine this binary cannot serve.
    pub fn unavailable_error(self) -> AppError {
        AppError::UnsupportedBackend(format!(
            "{} engine {} (cargo feature `{}`)",
            self.as_str(),
            self.unavailability().unwrap_or("is not available"),
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
    fn only_an_engine_this_build_can_run_is_offered_as_available() {
        assert_eq!(
            EngineKind::Mock.is_available(),
            cfg!(feature = "mock"),
            "Mock is what keeps the app running"
        );
        assert!(EngineKind::GgufMetadata.is_available());

        // `llama-cpp` links the real library, so the answer tracks the feature.
        #[cfg(feature = "llama-cpp")]
        assert!(EngineKind::LlamaCpp.is_available());
        #[cfg(not(feature = "llama-cpp"))]
        assert!(!EngineKind::LlamaCpp.is_available());

        // Candle is an adapter on both sides of its flag, so it never displaces
        // an engine the app can actually generate with.
        assert!(!EngineKind::Candle.is_available());
        assert_eq!(
            EngineKind::all().first(),
            Some(&EngineKind::Mock),
            "available engines sort first and Mock is one of them"
        );
    }

    #[test]
    fn an_engine_that_cannot_run_explains_why() {
        let reason = EngineKind::Candle
            .unavailability()
            .expect("an unavailable engine explains itself");
        let expected = if EngineKind::Candle.compiled() {
            "adapter only"
        } else {
            "not compiled"
        };
        assert!(reason.contains(expected), "{reason}");
    }

    #[test]
    fn metadata_engine_is_always_available() {
        assert!(EngineKind::GgufMetadata.is_available());
        let error = EngineKind::Candle.unavailable_error();
        assert!(matches!(error, AppError::UnsupportedBackend(_)));
    }
}
