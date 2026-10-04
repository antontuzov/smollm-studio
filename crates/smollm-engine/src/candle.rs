//! Candle adapter — feature flagged, pure Rust, and not wired up yet.
//!
//! TODO(candle adapter): `candle-core` + `candle-flash-attn` would give us a
//! Rust-native GGUF runner with no C++ toolchain, which is attractive for
//! Windows. It is kept behind a feature because the GGUF loaders for the
//! Llama/Qwen/Phi architectures we ship are still moving upstream. Until then
//! this type only reserves the trait shape.
//!
//! `--features candle` compiles the adapter; it does not pull the crate yet.

use smollm_core::chat::{
    EngineMetrics, GenerationRequest, LoadModelOptions, LoadModelRequest, ModelHandle,
};
use smollm_core::{AppError, AppResult};

use crate::{Engine, TokenStream};

pub const ADAPTER_NOTE: &str =
    "candle backend is compiled as an adapter only: no GGUF runner is wired in yet";

/// Placeholder engine that keeps the `candle` feature building everywhere.
#[derive(Debug, Default)]
pub struct CandleEngine {
    options: LoadModelOptions,
}

impl CandleEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unavailable() -> AppError {
        AppError::UnsupportedBackend(ADAPTER_NOTE.to_string())
    }

    pub fn is_linked() -> bool {
        false
    }
}

impl Engine for CandleEngine {
    fn name(&self) -> &str {
        "candle"
    }

    fn supports_gguf(&self) -> bool {
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
        let engine = CandleEngine::new();
        assert_eq!(engine.name(), "candle");
        assert!(engine.supports_gguf());
        assert!(!engine.is_simulated());
        assert!(!CandleEngine::is_linked());
    }

    #[test]
    fn generation_is_refused_rather_than_faked() {
        let mut engine = CandleEngine::default();
        let error = engine
            .generate(GenerationRequest {
                request_id: "r".to_string(),
                prompt: "p".to_string(),
                params: Default::default(),
                stop: Vec::new(),
                cancel: Default::default(),
            })
            .err()
            .expect("adapter cannot run yet");
        assert!(matches!(error, AppError::UnsupportedBackend(_)));
    }
}
