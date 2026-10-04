//! Event names and payloads emitted to the frontend.
//!
//! Every long-running operation reports through an event rather than blocking a
//! command: streaming tokens, downloads, server traffic and benchmark stages.

use serde::Serialize;
use smollm_core::chat::{GenToken, TokenUsage};
use smollm_core::ErrorPayload;
use smollm_models::download::DownloadEvent;
use tauri::{AppHandle, Emitter};

pub const CHAT_TOKEN: &str = "chat-token";
pub const CHAT_DONE: &str = "chat-done";
pub const CHAT_ERROR: &str = "chat-error";
pub const SERVER_LOG: &str = "server-log";
pub const BENCHMARK_PROGRESS: &str = "benchmark-progress";

/// One streamed piece of assistant output, tagged with the request it belongs to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTokenPayload {
    pub request_id: String,
    pub token: GenToken,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDonePayload {
    pub request_id: String,
    pub text: String,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
    pub elapsed_ms: u64,
    pub tokens_per_second: f64,
    /// True when MockEngine produced the answer instead of real weights.
    pub simulated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatErrorPayload {
    pub request_id: String,
    #[serde(flatten)]
    pub error: ErrorPayload,
}

/// One line for the Server page's request log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLogPayload {
    pub timestamp_ms: u64,
    pub level: String,
    pub message: String,
}

pub fn emit_chat_token(app: &AppHandle, payload: ChatTokenPayload) {
    emit(app, CHAT_TOKEN, &payload);
}

pub fn emit_chat_done(app: &AppHandle, payload: ChatDonePayload) {
    emit(app, CHAT_DONE, &payload);
}

pub fn emit_chat_error(app: &AppHandle, payload: ChatErrorPayload) {
    emit(app, CHAT_ERROR, &payload);
}

pub fn emit_server_log(app: &AppHandle, payload: ServerLogPayload) {
    emit(app, SERVER_LOG, &payload);
}

/// Forward a download event using the names the `DownloadEvent` enum already
/// declares (`download-progress`, `download-complete`, …), so the event names
/// have exactly one source.
pub fn emit_download(app: &AppHandle, event: &DownloadEvent) {
    let Ok(value) = serde_json::to_value(event) else {
        return;
    };
    let Some(name) = value.get("event").and_then(serde_json::Value::as_str) else {
        return;
    };
    let payload = value
        .get("payload")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    emit(app, name, &payload);
}

fn emit<S: Serialize>(app: &AppHandle, name: &str, payload: &S) {
    if let Err(error) = app.emit(name, payload) {
        tracing::debug!(event = name, %error, "event could not be delivered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::chat::TokenUsage;
    use smollm_core::AppError;

    #[test]
    fn chat_payloads_use_camel_case() {
        let done = ChatDonePayload {
            request_id: "req-1".to_string(),
            text: "hello".to_string(),
            finish_reason: Some("stop".to_string()),
            usage: Some(TokenUsage::new(4, 2)),
            elapsed_ms: 120,
            tokens_per_second: 16.6,
            simulated: true,
        };
        let json = serde_json::to_value(&done).expect("serialisable");
        assert_eq!(json["requestId"], "req-1");
        assert_eq!(json["finishReason"], "stop");
        assert_eq!(json["usage"]["totalTokens"], 6);
        assert_eq!(json["simulated"], true);
    }

    #[test]
    fn chat_error_flattens_the_structured_error() {
        let error = AppError::ModelNotDownloaded("qwen2.5-0.5b".to_string());
        let payload = ChatErrorPayload {
            request_id: "req-2".to_string(),
            error: error.to_payload(),
        };
        let json = serde_json::to_value(&payload).expect("serialisable");
        assert_eq!(json["requestId"], "req-2");
        assert_eq!(json["code"], "model_not_downloaded");
        assert!(json["message"]
            .as_str()
            .is_some_and(|m| m.contains("qwen2.5-0.5b")));
    }

    #[test]
    fn download_event_names_come_from_the_enum() {
        let event = DownloadEvent::Progress(smollm_models::download::DownloadProgress {
            download_id: "d1".to_string(),
            model_id: "m1".to_string(),
            file_name: "m1.gguf".to_string(),
            state: smollm_models::download::DownloadState::Running,
            downloaded_bytes: 10,
            total_bytes: Some(20),
            percent: 50.0,
            bytes_per_second: 1024.0,
            error: None,
        });
        let json = serde_json::to_value(&event).expect("serialisable");
        assert_eq!(json["event"], "download-progress");
        assert_eq!(json["payload"]["downloadId"], "d1");
        assert_eq!(json["payload"]["state"], "running");
    }
}
