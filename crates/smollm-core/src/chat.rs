//! Chat, sampling and generation types shared by engine, server and UI.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::ModelMetadata;
use crate::system::Backend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

pub fn chat_message(role: Role, content: impl Into<String>) -> ChatMessage {
    ChatMessage {
        role,
        content: content.into(),
    }
}

/// Sampling controls exposed in the parameters panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,
    pub max_tokens: u32,
    pub repeat_penalty: f32,
    pub presence_penalty: f32,
    /// `None` means "random per request".
    pub seed: Option<i64>,
}

impl Default for SamplingParams {
    /// Safe, fast defaults tuned for 0.5B-4B models.
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            min_p: 0.05,
            max_tokens: 512,
            repeat_penalty: 1.1,
            presence_penalty: 0.0,
            seed: Some(42),
        }
    }
}

impl SamplingParams {
    pub fn preset(name: &str) -> Self {
        let mut params = Self::default();
        match name {
            "fast" => {
                params.temperature = 0.4;
                params.top_p = 0.85;
                params.max_tokens = 256;
                params.seed = Some(42);
            }
            "balanced" => params.temperature = 0.7,
            "creative" => {
                params.temperature = 1.1;
                params.top_p = 0.95;
                params.top_k = 80;
                params.min_p = 0.03;
                params.presence_penalty = 0.4;
                params.seed = None;
            }
            "coding" => {
                params.temperature = 0.2;
                params.top_p = 0.9;
                params.max_tokens = 1024;
                params.repeat_penalty = 1.05;
            }
            "precise" => {
                params.temperature = 0.1;
                params.top_p = 0.7;
                params.min_p = 0.1;
            }
            _ => {}
        }
        params
    }

    pub fn preset_names() -> [&'static str; 5] {
        ["fast", "balanced", "creative", "coding", "precise"]
    }

    pub fn validate(&self) -> Result<(), crate::AppError> {
        let invalid = [
            ("temperature", self.temperature, 0.0f32..=2.0f32),
            ("topP", self.top_p, 0.0f32..=1.0f32),
            ("minP", self.min_p, 0.0f32..=1.0f32),
            ("repeatPenalty", self.repeat_penalty, 0.8f32..=2.0f32),
            ("presencePenalty", self.presence_penalty, -2.0f32..=2.0f32),
        ];
        for (name, value, range) in invalid {
            if !range.contains(&value) {
                return Err(crate::AppError::InvalidRequest(format!(
                    "{name} must be within {}..={}, got {value}",
                    range.start(),
                    range.end()
                )));
            }
        }
        if self.max_tokens == 0 || self.max_tokens > 32_768 {
            return Err(crate::AppError::InvalidRequest(format!(
                "maxTokens must be between 1 and 32768, got {}",
                self.max_tokens
            )));
        }
        Ok(())
    }
}

/// Per-model runtime options chosen when loading a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoadModelOptions {
    pub context_length: u32,
    pub gpu_layers: i32,
    pub backend: Backend,
    pub threads: Option<u32>,
}

impl Default for LoadModelOptions {
    fn default() -> Self {
        Self {
            context_length: 4096,
            gpu_layers: -1,
            backend: Backend::default(),
            threads: None,
        }
    }
}

/// What an engine needs to put a model into memory.
#[derive(Debug, Clone, Default)]
pub struct LoadModelRequest {
    pub model_id: String,
    pub display_name: String,
    /// Absolute path to the local `.gguf` file, when one exists.
    pub path: PathBuf,
    pub options: LoadModelOptions,
    /// Catalog metadata, used as a fallback when the file cannot be parsed.
    pub metadata: Option<ModelMetadata>,
}

/// Rough token count for UI estimates and OpenAI `usage` blocks.
///
/// Real tokenisers are model specific; ~4 characters per token is the usual
/// heuristic for English text on BPE models and is honest enough for an estimate.
pub fn approx_token_count(text: &str) -> u32 {
    let chars = text.chars().count() as f64;
    // Count whitespace-separated words too: word-based estimates are closer for
    // code and CJK, where characters carry more meaning.
    let words = text.split_whitespace().count() as f64;
    let estimate = (chars / 4.0).max(words * 1.3).round();
    // A prompt of only whitespace still costs nothing.
    if text.trim().is_empty() {
        0
    } else {
        estimate.max(1.0) as u32
    }
}

/// Request as sent by the frontend / OpenAI-compatible server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatRequest {
    pub request_id: String,
    pub model_id: String,
    pub messages: Vec<ChatMessage>,
    pub system_prompt: Option<String>,
    pub params: SamplingParams,
    /// Stop sequences; the engine trims generated text at the first match.
    pub stop: Vec<String>,
}

impl ChatRequest {
    pub fn validate(&self) -> Result<(), crate::AppError> {
        if self.messages.is_empty() {
            return Err(crate::AppError::InvalidRequest(
                "messages must not be empty".into(),
            ));
        }
        if self
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.content.is_empty())
        {
            return Err(crate::AppError::InvalidRequest(
                "assistant messages must not be empty".into(),
            ));
        }
        self.params.validate()?;
        Ok(())
    }
}

/// A streamed piece of assistant output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenToken {
    pub text: String,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}

impl GenToken {
    pub fn token(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            finish_reason: None,
            usage: None,
        }
    }

    pub fn finish(reason: &str, usage: TokenUsage) -> Self {
        Self {
            text: String::new(),
            finish_reason: Some(reason.to_string()),
            usage: Some(usage),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }
}

/// Cancellation handle handed to engines; `stop_generation` flips it.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Fully resolved request handed to an [`crate::Engine`].
#[derive(Debug, Clone)]
pub struct GenerationRequest {
    pub request_id: String,
    pub prompt: String,
    pub params: SamplingParams,
    pub stop: Vec<String>,
    pub cancel: CancelToken,
}

/// Handle to a loaded model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelHandle {
    pub id: String,
    pub model_id: String,
    pub display_name: String,
    pub path: String,
    pub engine: String,
    pub context_length: u32,
    pub metadata: crate::model::ModelMetadata,
}

/// Engine-level counters surfaced to the UI and `/v1/engine/metrics`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineMetrics {
    pub engine: String,
    pub simulated: bool,
    pub model_id: Option<String>,
    pub requests: u64,
    pub tokens_generated: u64,
    pub total_generation_ms: u64,
    pub tokens_per_second: f64,
    pub loaded: bool,
    pub backend: Backend,
}

/// Which engine the app resolved for a model id.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadModelResponse {
    pub handle: ModelHandle,
    pub engine: String,
    pub simulated: bool,
    pub warnings: Vec<String>,
}
