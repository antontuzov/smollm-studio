//! Background tasks: token streaming, download events and the server watcher.
//!
//! All three run on Tauri's Tokio runtime, never on the main thread, and all
//! three report through events so a slow model cannot freeze the UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use smollm_core::chat::approx_token_count;
use smollm_core::AppError;
use smollm_engine::TokenStream;
use smollm_models::download::DownloadEvent;
use smollm_server::SharedState;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use crate::events::{
    emit_chat_done, emit_chat_error, emit_chat_token, emit_download, emit_server_log,
    ChatDonePayload, ChatErrorPayload, ChatTokenPayload, ServerLogPayload,
};

/// Forward every token of one generation to the frontend.
pub(crate) fn pump_tokens(
    app: AppHandle,
    request_id: String,
    stream: TokenStream,
    simulated: bool,
) {
    tauri::async_runtime::spawn(async move {
        let mut stream = stream;
        let started = Instant::now();
        let mut text = String::new();
        let mut finish_reason: Option<String> = None;
        let mut usage = None;
        let mut failure: Option<AppError> = None;

        while let Some(item) = stream.next().await {
            match item {
                Ok(token) => {
                    if token.finish_reason.is_some() {
                        finish_reason = token.finish_reason;
                        usage = token.usage;
                        break;
                    }
                    if !token.text.is_empty() {
                        text.push_str(&token.text);
                        emit_chat_token(
                            &app,
                            ChatTokenPayload {
                                request_id: request_id.clone(),
                                token,
                            },
                        );
                    }
                }
                Err(source) => {
                    failure = Some(source);
                    break;
                }
            }
        }

        // Dropping the stream here releases its slot in the cancellation registry.
        drop(stream);

        let elapsed_ms = started.elapsed().as_millis() as u64;
        if let Some(error) = failure {
            emit_chat_error(
                &app,
                ChatErrorPayload {
                    request_id,
                    error: error.to_payload(),
                },
            );
            return;
        }

        let generated = usage.map_or_else(
            || approx_token_count(&text),
            |reported| reported.completion_tokens,
        );
        let tokens_per_second = if elapsed_ms > 0 && generated > 0 {
            f64::from(generated) * 1000.0 / elapsed_ms as f64
        } else {
            0.0
        };
        emit_chat_done(
            &app,
            ChatDonePayload {
                request_id,
                text,
                finish_reason,
                usage,
                elapsed_ms,
                tokens_per_second,
                simulated,
            },
        );
    });
}

/// Relay download progress to the frontend using the enum's own event names.
pub(crate) fn pump_downloads(app: AppHandle, mut receiver: mpsc::UnboundedReceiver<DownloadEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = receiver.recv().await {
            emit_download(&app, &event);
        }
    });
}

/// Report server traffic until `stop_server` clears the flag.
pub(crate) fn watch_server(app: AppHandle, state: SharedState, running: Arc<AtomicBool>) {
    tauri::async_runtime::spawn(async move {
        let mut served = state.request_count();
        while running.load(Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let current = state.request_count();
            if current != served {
                served = current;
                let message = format!(
                    "{current} request{} served · {}s uptime",
                    if current == 1 { "" } else { "s" },
                    state.uptime_seconds()
                );
                emit_server_log(
                    &app,
                    ServerLogPayload {
                        timestamp_ms: crate::now_millis(),
                        level: "info".to_string(),
                        message,
                    },
                );
            }
        }
        let _ = app.emit("server-watch-stopped", ());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use smollm_core::chat::{GenToken, TokenUsage};
    use smollm_core::AppResult;

    /// The pump needs an `AppHandle`, which a test cannot build; the pieces it
    /// relies on are exercised directly instead.
    #[tokio::test]
    async fn a_stream_terminates_on_its_finish_token() {
        let tokens: Vec<AppResult<GenToken>> = vec![
            Ok(GenToken::token("hello")),
            Ok(GenToken::token(" there")),
            Ok(GenToken::finish("stop", TokenUsage::new(3, 2))),
            Ok(GenToken::token("never seen")),
        ];
        let mut stream = Box::pin(stream::iter(tokens));
        let mut text = String::new();
        let mut finish = None;
        let mut usage = None;
        while let Some(item) = stream.next().await {
            let token = item.expect("token");
            if token.finish_reason.is_some() {
                finish = token.finish_reason;
                usage = token.usage;
                break;
            }
            text.push_str(&token.text);
        }
        assert_eq!(text, "hello there");
        assert_eq!(finish.as_deref(), Some("stop"));
        assert_eq!(usage.map(|value| value.total_tokens), Some(5));
    }

    #[test]
    fn rate_math_handles_zero_elapsed() {
        let rate = |generated: u32, elapsed_ms: u64| {
            if elapsed_ms > 0 && generated > 0 {
                f64::from(generated) * 1000.0 / elapsed_ms as f64
            } else {
                0.0
            }
        };
        assert_eq!(rate(10, 0), 0.0);
        assert_eq!(rate(0, 100), 0.0);
        assert_eq!(rate(10, 1000), 10.0);
    }
}
