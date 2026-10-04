//! OpenAI-compatible wire types.
//!
//! These structs deliberately use OpenAI's own snake_case field names: the
//! whole point is that existing SDKs can talk to this server unchanged.

use serde::{Deserialize, Serialize};
use smollm_core::chat::{ChatMessage, Role, TokenUsage};

/// `stop` may be a single string or a list, in both directions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StopSequences {
    One(String),
    Many(Vec<String>),
}

impl Default for StopSequences {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl StopSequences {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value.clone()],
            Self::Many(values) => values.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::One(value) => value.is_empty(),
            Self::Many(values) => values.is_empty(),
        }
    }
}

/// `POST /v1/chat/completions` body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Alias used by the newer SDKs.
    #[serde(default, alias = "max_completion_tokens")]
    pub max_tokens_alias: Option<u32>,
    #[serde(default)]
    pub stop: Option<StopSequences>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub n: Option<u8>,
    /// Ignored, accepted for SDK compatibility.
    #[serde(default)]
    pub user: Option<String>,
    /// Some clients send a top-level system prompt instead of a message.
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: Option<bool>,
}

impl ChatCompletionRequest {
    /// Reject the combinations this server genuinely cannot honour.
    pub fn validate(&self) -> Result<(), String> {
        if self.messages.is_empty() {
            return Err("'messages' must contain at least one message".to_string());
        }
        if matches!(self.n, Some(n) if n > 1) {
            return Err(
                "'n' greater than 1 is not supported: one completion per request".to_string(),
            );
        }
        if matches!(self.max_tokens, Some(0)) {
            return Err("'max_tokens' must be at least 1".to_string());
        }
        Ok(())
    }

    /// System prompt: explicit field wins, otherwise the leading system message.
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt
            .as_deref()
            .or(self.instructions.as_deref())
            .or_else(|| {
                self.messages
                    .iter()
                    .find(|message| message.role == Role::System)
                    .map(|message| message.content.as_str())
            })
    }

    pub fn max_tokens(&self) -> Option<u32> {
        self.max_tokens.or(self.max_tokens_alias)
    }

    pub fn stop_sequences(&self) -> Vec<String> {
        self.stop
            .as_ref()
            .map(StopSequences::to_vec)
            .unwrap_or_default()
    }
}

/// `POST /v1/completions` body (legacy text API).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompletionRequest {
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: PromptField,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stop: Option<StopSequences>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub echo: Option<bool>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub n: Option<u8>,
    #[serde(default)]
    pub user: Option<String>,
}

/// `prompt` accepts a string or a list of strings; only one item is served.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum PromptField {
    #[default]
    Empty,
    Text(String),
    List(Vec<String>),
}

impl PromptField {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Empty => "",
            Self::Text(value) => value.as_str(),
            // Single-value serving: the first prompt wins.
            Self::List(values) => values.first().map(String::as_str).unwrap_or(""),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty) || self.as_str().is_empty()
    }
}

/// OpenAI's `usage` block.
///
/// Our internal [`TokenUsage`] is camelCase like every other app DTO, so the
/// snake_case wire form needs its own type rather than a shared one.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl From<TokenUsage> for Usage {
    fn from(usage: TokenUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

/// Chat completion response.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletion {
    pub id: String,
    pub object: &'static str,
    pub created: u32,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
    pub system_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Streaming chunk.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: u32,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub system_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Text completion response (legacy).
#[derive(Debug, Clone, Serialize)]
pub struct Completion {
    pub id: String,
    pub object: &'static str,
    pub created: u32,
    pub model: String,
    pub choices: Vec<TextChoice>,
    pub usage: Usage,
    pub system_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextChoice {
    pub index: u32,
    pub text: String,
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextChunkChoice {
    pub index: u32,
    pub text: String,
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: Option<String>,
}

/// Streaming chunk for the legacy text API.
#[derive(Debug, Clone, Serialize)]
pub struct CompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: u32,
    pub model: String,
    pub choices: Vec<TextChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub system_fingerprint: String,
}

/// `GET /v1/models`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<ModelCard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCard {
    pub id: String,
    pub object: &'static str,
    pub created: u32,
    pub owned_by: String,
}

impl ModelCard {
    pub fn new(id: impl Into<String>, created: u32, owned_by: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            object: "model",
            created,
            owned_by: owned_by.into(),
        }
    }
}

/// OpenAI-shaped error envelope.
#[derive(Debug, Clone, Serialize)]
pub struct OpenAiErrorBody {
    pub error: OpenAiErrorFields,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiErrorFields {
    pub message: String,
    /// `invalid_request_error`, `not_found_error`, `server_error`, …
    pub r#type: &'static str,
    pub param: Option<String>,
    pub code: Option<String>,
}

impl OpenAiErrorBody {
    pub fn new(message: impl Into<String>, kind: &'static str, code: Option<String>) -> Self {
        Self {
            error: OpenAiErrorFields {
                message: message.into(),
                r#type: kind,
                param: None,
                code,
            },
        }
    }
}

/// Seconds since the Unix epoch, as OpenAI reports in `created`.
pub fn now_epoch() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as u32)
        .unwrap_or_default()
}

/// `chatcmpl-<uuid>`, matching the prefix real clients look for.
pub fn completion_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Serialise an SSE payload; a failure becomes a visible error object rather
/// than an aborted connection, so the stream itself stays infallible.
pub fn chunk_to_string(value: serde_json::Value) -> String {
    match serde_json::to_string(&value) {
        Ok(text) => text,
        Err(error) => format!("{{\"error\":\"{error}\"}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::chat::chat_message;

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("valid json")
    }

    #[test]
    fn parses_the_documented_example_request() {
        let body = json(
            r#"{"model":"qwen2.5-0.5b-instruct-gguf","messages":[{"role":"user","content":"Hello"}],"stream":true,"temperature":0.7,"max_tokens":512}"#,
        );
        let request: ChatCompletionRequest = serde_json::from_value(body).expect("deserialises");
        assert_eq!(request.model.as_deref(), Some("qwen2.5-0.5b-instruct-gguf"));
        assert!(request.stream);
        assert_eq!(request.max_tokens(), Some(512));
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.messages.len(), 1);
        assert!(request.validate().is_ok());
    }

    #[test]
    fn empty_messages_are_rejected() {
        let request: ChatCompletionRequest =
            serde_json::from_str(r#"{"model":"x","messages":[]}"#).expect("parses");
        let error = request.validate().expect_err("needs a message");
        assert!(error.contains("messages"), "got {error}");
    }

    #[test]
    fn stop_accepts_a_string_or_a_list() {
        let one: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"a"}],"stop":"END"}"#)
                .expect("parses");
        assert_eq!(one.stop_sequences(), vec!["END".to_string()]);

        let many: ChatCompletionRequest = serde_json::from_str(
            r#"{"messages":[{"role":"user","content":"a"}],"stop":["</s>","\n\n"]}"#,
        )
        .expect("parses");
        assert_eq!(many.stop_sequences(), vec!["</s>", "\n\n"]);

        let none: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"a"}]}"#)
                .expect("parses");
        assert!(none.stop_sequences().is_empty());
    }

    #[test]
    fn system_prompt_falls_back_to_the_leading_system_message() {
        let mut request: ChatCompletionRequest = serde_json::from_str(
            r#"{"messages":[{"role":"system","content":"be brief"},{"role":"user","content":"hi"}]}"#,
        )
        .expect("parses");
        assert_eq!(request.system_prompt(), Some("be brief"));

        request.system_prompt = Some("explicit".to_string());
        assert_eq!(request.system_prompt(), Some("explicit"));

        request.system_prompt = None;
        request.instructions = Some("preferred over none".to_string());
        assert_eq!(request.system_prompt(), Some("preferred over none"));

        let user_only: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"hi"}]}"#)
                .expect("parses");
        assert_eq!(user_only.system_prompt(), None);
    }

    #[test]
    fn max_completion_tokens_is_an_alias() {
        let request: ChatCompletionRequest = serde_json::from_str(
            r#"{"messages":[{"role":"user","content":"hi"}],"max_completion_tokens":64}"#,
        )
        .expect("parses");
        assert_eq!(request.max_tokens(), Some(64));
    }

    #[test]
    fn n_gt_one_is_refused_rather_than_ignored() {
        let request: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"hi"}],"n":3}"#)
                .expect("parses");
        assert!(request.validate().is_err());
    }

    #[test]
    fn prompt_field_accepts_string_and_list() {
        let text: CompletionRequest =
            serde_json::from_str(r#"{"prompt":"hello"}"#).expect("parses");
        assert_eq!(text.prompt.as_str(), "hello");

        let list: CompletionRequest =
            serde_json::from_str(r#"{"prompt":["first","second"]}"#).expect("parses");
        assert_eq!(list.prompt.as_str(), "first");

        let missing: CompletionRequest = serde_json::from_str(r#"{}"#).expect("parses");
        assert_eq!(missing.prompt.as_str(), "");
    }

    #[test]
    fn responses_use_openai_field_names() {
        let completion = ChatCompletion {
            id: completion_id("chatcmpl"),
            object: "chat.completion",
            created: now_epoch(),
            model: "m".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: chat_message(Role::Assistant, "hi"),
                finish_reason: Some("stop".to_string()),
            }],
            usage: Usage::from(TokenUsage::new(3, 4)),
            system_fingerprint: "fp".to_string(),
        };
        let value = serde_json::to_value(&completion).expect("serialises");
        assert_eq!(value["object"], "chat.completion");
        assert!(value["choices"][0]["message"]["content"].is_string());
        assert_eq!(value["usage"]["total_tokens"], 7);
        assert!(value["id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("chatcmpl-"));

        let list = ModelList {
            object: "list",
            data: vec![ModelCard::new("m", now_epoch(), "smollm-studio")],
        };
        let value = serde_json::to_value(&list).expect("serialises");
        assert_eq!(value["data"][0]["owned_by"], "smollm-studio");
    }

    #[test]
    fn chunks_omit_absent_delta_fields() {
        let chunk = ChatCompletionChunk {
            id: "c".to_string(),
            object: "chat.completion.chunk",
            created: 1,
            model: "m".to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content: Some("x".to_string()),
                },
                finish_reason: None,
            }],
            usage: None,
            system_fingerprint: "fp".to_string(),
        };
        let value = serde_json::to_value(&chunk).expect("serialises");
        assert!(value["choices"][0]["delta"].get("role").is_none());
        assert!(value["usage"].is_null(), "usage is skipped when absent");
        assert_eq!(value["choices"][0]["delta"]["content"], "x");
    }

    #[test]
    fn errors_carry_the_openai_envelope() {
        let body = OpenAiErrorBody::new("bad input", "invalid_request_error", Some("400".into()));
        let value = serde_json::to_value(&body).expect("serialises");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["message"], "bad input");
        assert!(value["error"]["param"].is_null());
    }
}
