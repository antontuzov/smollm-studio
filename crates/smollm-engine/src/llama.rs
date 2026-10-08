//! llama.cpp adapter — feature flagged and deliberately not linked by default.
//!
//! TODO(llama-cpp adapter): this is the seam where native inference plugs in.
//! The plan is `llama-cpp-2` (safe bindings over the C library) or a thin FFI
//! shim built against a vendored llama.cpp, both behind this feature so a
//! missing toolchain never blocks the rest of the app. Until that lands this
//! type keeps the trait shape honest: it reports that it cannot generate.
//!
//! Enabling `--features llama-cpp` compiles the adapter; it does not yet fetch
//! or build the C++ library. See `docs/models.md` for what linking needs and
//! `docs/hardware.md` for how backends resolve to engines.

use smollm_core::chat::{
    EngineMetrics, GenerationRequest, LoadModelOptions, LoadModelRequest, ModelHandle,
};
use smollm_core::{AppError, AppResult};

use crate::{Engine, TokenStream};

/// Shown whenever a caller reaches an unlinked native backend.
pub const ADAPTER_NOTE: &str =
    "llama.cpp backend is compiled as an adapter only: no native library is linked yet";

/// Placeholder engine that keeps the `llama-cpp` feature building everywhere.
#[derive(Debug, Default)]
pub struct LlamaCppEngine {
    options: LoadModelOptions,
}

impl LlamaCppEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The error every unimplemented method returns, kept in one place.
    pub fn unavailable() -> AppError {
        AppError::UnsupportedBackend(ADAPTER_NOTE.to_string())
    }

    /// True once a native build has been linked into this feature.
    pub fn is_linked() -> bool {
        false
    }
}

impl Engine for LlamaCppEngine {
    fn name(&self) -> &str {
        "llama-cpp"
    }

    fn supports_gguf(&self) -> bool {
        // It is the GGUF backend; it just is not linked yet.
        true
    }

    fn is_simulated(&self) -> bool {
        false
    }

    fn load_model(&mut self, request: LoadModelRequest) -> AppResult<ModelHandle> {
        self.options = request.options;
        Err(Self::unavailable())
    }

    fn unload_model(&mut self, _handle: ModelHandle) -> AppResult<()> {
        Err(Self::unavailable())
    }

    fn generate(&mut self, _request: GenerationRequest) -> AppResult<TokenStream> {
        Err(Self::unavailable())
    }

    fn tokenize(&self, _text: &str) -> AppResult<Vec<u32>> {
        Err(Self::unavailable())
    }

    fn metrics(&self) -> EngineMetrics {
        EngineMetrics {
            engine: self.name().to_string(),
            simulated: false,
            model_id: None,
            requests: 0,
            tokens_generated: 0,
            total_generation_ms: 0,
            tokens_per_second: 0.0,
            loaded: false,
            backend: self.options.backend,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_is_named_and_unlinked() {
        let engine = LlamaCppEngine::new();
        assert_eq!(engine.name(), "llama-cpp");
        assert!(engine.supports_gguf());
        assert!(!engine.is_simulated());
        assert!(!LlamaCppEngine::is_linked());
        assert!(!engine.metrics().loaded);
    }

    #[test]
    fn every_capability_reports_the_adapter_note() {
        let mut engine = LlamaCppEngine::new();
        let generation = GenerationRequest {
            request_id: "r".to_string(),
            prompt: "p".to_string(),
            params: Default::default(),
            stop: Vec::new(),
            cancel: Default::default(),
        };
        let handle = ModelHandle {
            id: "x".to_string(),
            model_id: "x".to_string(),
            display_name: "x".to_string(),
            path: String::new(),
            engine: "llama-cpp".to_string(),
            context_length: 512,
            metadata: Default::default(),
        };

        let notes = vec![
            note(&engine.load_model(LoadModelRequest::default())),
            note(&engine.unload_model(handle)),
            note(&engine.generate(generation)),
            note(&engine.tokenize("hello")),
        ];
        assert_eq!(notes.len(), 4);
        assert!(
            notes
                .iter()
                .all(|found| found.as_deref() == Some(ADAPTER_NOTE)),
            "every method refuses: {notes:?}"
        );
    }

    /// Pull the unsupported-backend note out of any engine result.
    fn note<T>(result: &AppResult<T>) -> Option<String> {
        match result {
            Err(AppError::UnsupportedBackend(message)) => Some(message.clone()),
            _ => None,
        }
    }
}
