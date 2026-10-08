//! Bridges the engine's token stream to OpenAI's JSON and SSE shapes.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderName, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{self, StreamExt};
use serde_json::json;
use smollm_core::chat::{chat_message, GenToken, Role, TokenUsage};
use smollm_core::error::AppError;
use smollm_engine::TokenStream;

use crate::openai::{
    chunk_to_string, now_epoch, ChatChoice, ChatCompletion, ChatCompletionChunk, ChunkChoice,
    Completion as TextCompletion, CompletionChunk, Delta, OpenAiErrorBody, TextChoice,
    TextChunkChoice, Usage,
};

/// What a completed generation gives back.
#[derive(Debug, Default)]
pub struct Aggregated {
    pub text: String,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
    pub error: Option<AppError>,
}

impl Aggregated {
    /// Engine reason wins; a stream that ended without one still looks finished.
    pub fn finish(&self) -> String {
        self.finish_reason
            .clone()
            .unwrap_or_else(|| "stop".to_string())
    }

    /// Usage as reported by the engine, or a stable estimate when it abstained.
    pub fn usage_for(&self, prompt: &str) -> TokenUsage {
        self.usage.unwrap_or_else(|| {
            TokenUsage::new(
                smollm_core::chat::approx_token_count(prompt).max(1),
                smollm_core::chat::approx_token_count(&self.text),
            )
        })
    }
}

/// OpenAI uses `stop` and `length`; our engines may also say `eos`.
pub fn map_finish(reason: Option<&str>) -> Option<String> {
    match reason {
        None => None,
        Some("length") => Some("length".to_string()),
        Some(_) => Some("stop".to_string()),
    }
}

/// Pull a stream to completion for the non-streaming JSON path.
pub async fn aggregate(mut stream: TokenStream) -> Aggregated {
    let mut out = Aggregated::default();
    while let Some(item) = stream.next().await {
        match item {
            Ok(token) => {
                if token.finish_reason.is_some() {
                    out.finish_reason = map_finish(token.finish_reason.as_deref());
                    out.usage = token.usage;
                } else {
                    out.text.push_str(&token.text);
                }
            }
            Err(error) => {
                out.error = Some(error);
                break;
            }
        }
    }
    out
}

pub fn chat_response(
    id: String,
    model: String,
    fingerprint: String,
    prompt: &str,
    aggregated: &Aggregated,
) -> ChatCompletion {
    ChatCompletion {
        id,
        object: "chat.completion",
        created: now_epoch(),
        model,
        choices: vec![ChatChoice {
            index: 0,
            message: chat_message(Role::Assistant, aggregated.text.clone()),
            finish_reason: Some(aggregated.finish()),
        }],
        usage: Usage::from(aggregated.usage_for(prompt)),
        system_fingerprint: fingerprint,
    }
}

pub fn text_response(
    id: String,
    model: String,
    fingerprint: String,
    prompt: &str,
    aggregated: &Aggregated,
) -> TextCompletion {
    TextCompletion {
        id,
        object: "text_completion",
        created: now_epoch(),
        model,
        choices: vec![TextChoice {
            index: 0,
            text: aggregated.text.clone(),
            logprobs: None,
            finish_reason: Some(aggregated.finish()),
        }],
        usage: Usage::from(aggregated.usage_for(prompt)),
        system_fingerprint: fingerprint,
    }
}

/// One `chat.completion.chunk` as a JSON string, ready for `data:`.
fn chat_chunk_json(
    id: &str,
    model: &str,
    fingerprint: &str,
    delta: Delta,
    finish: Option<String>,
    usage: Option<Usage>,
) -> String {
    chunk_to_string(json!(ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk",
        created: now_epoch(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta,
            finish_reason: finish,
        }],
        usage,
        system_fingerprint: fingerprint.to_string(),
    }))
}

fn text_chunk_json(id: &str, model: &str, fingerprint: &str, token: &GenToken) -> String {
    chunk_to_string(json!(CompletionChunk {
        id: id.to_string(),
        object: "text_completion",
        created: now_epoch(),
        model: model.to_string(),
        choices: vec![TextChunkChoice {
            index: 0,
            text: token.text.clone(),
            logprobs: None,
            finish_reason: map_finish(token.finish_reason.as_deref()),
        }],
        usage: token.usage.map(Usage::from),
        system_fingerprint: fingerprint.to_string(),
    }))
}

fn error_event(body: &OpenAiErrorBody) -> Event {
    Event::default()
        .event("error")
        .data(chunk_to_string(json!(body)))
}

/// `data: {json}` with no event name, which is what OpenAI clients parse.
fn data_event(payload: String) -> Event {
    Event::default().data(payload)
}

/// The stream's item type. It is infallible by construction: an engine error
/// becomes an SSE error frame rather than a dropped connection.
fn event(value: Event) -> Result<Event, Infallible> {
    Ok(value)
}

/// Chat SSE stream: role hint, content deltas, finish marker, an optional usage
/// chunk, then `[DONE]`.
///
/// `include_usage` follows OpenAI: usage is reported only when the client asked
/// for it, and then as a final chunk whose `choices` is empty. Content chunks
/// never carry it, so a strict client sees exactly the shape it expects.
pub fn sse_chat(
    id: String,
    model: String,
    fingerprint: String,
    include_usage: bool,
    prompt: String,
    stream: TokenStream,
) -> Response {
    let opener_payload = data_event(chat_chunk_json(
        &id,
        &model,
        &fingerprint,
        Delta {
            role: Some(Role::Assistant),
            content: Some(String::new()),
        },
        None,
        None,
    ));
    let opener = stream::once(async move { event(opener_payload) });

    // The engine reports usage on its finish token, but the usage chunk is only
    // built after the stream is gone, so both halves share a cell. A poisoned
    // lock still yields whatever was captured before the panic.
    let seen: Arc<Mutex<(Option<TokenUsage>, String)>> =
        Arc::new(Mutex::new((None, String::new())));
    let capture = Arc::clone(&seen);
    let (usage_id, usage_model, usage_fingerprint) =
        (id.clone(), model.clone(), fingerprint.clone());
    let tokens = stream.map(move |item| match item {
        Ok(token) => {
            if let Ok(mut slot) = capture.lock() {
                match token.usage {
                    Some(usage) => slot.0 = Some(usage),
                    None => slot.1.push_str(&token.text),
                }
            }
            event(data_event(chat_chunk_json(
                &id,
                &model,
                &fingerprint,
                Delta {
                    role: None,
                    content: (!token.text.is_empty()).then_some(token.text.clone()),
                },
                map_finish(token.finish_reason.as_deref()),
                None,
            )))
        }
        Err(error) => event(error_event(&crate::routes::ApiError::from(error).body)),
    });

    let usage_chunk = stream::once(async move {
        if !include_usage {
            return Vec::new();
        }
        let (reported, text) = seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let aggregated = Aggregated {
            text,
            finish_reason: None,
            usage: reported,
            error: None,
        };
        let payload = chunk_to_string(json!(ChatCompletionChunk {
            id: usage_id,
            object: "chat.completion.chunk",
            created: now_epoch(),
            model: usage_model,
            choices: Vec::new(),
            usage: Some(Usage::from(aggregated.usage_for(&prompt))),
            system_fingerprint: usage_fingerprint,
        }));
        vec![event(data_event(payload))]
    })
    .flat_map(stream::iter);

    let done = stream::once(async { event(Event::default().data("[DONE]")) });
    sse_response(
        Sse::new(opener.chain(tokens).chain(usage_chunk).chain(done))
            .keep_alive(KeepAlive::default()),
    )
}

/// Legacy completions SSE: plain `text` deltas, no role hint.
pub fn sse_text(id: String, model: String, fingerprint: String, stream: TokenStream) -> Response {
    let tokens = stream.map(move |item| match item {
        Ok(token) => event(data_event(text_chunk_json(
            &id,
            &model,
            &fingerprint,
            &token,
        ))),
        Err(error) => event(error_event(&crate::routes::ApiError::from(error).body)),
    });
    let done = stream::once(async { event(Event::default().data("[DONE]")) });
    sse_response(Sse::new(tokens.chain(done)).keep_alive(KeepAlive::default()))
}

/// A token stream must never be buffered between us and the client.
fn sse_response<B: IntoResponse>(body: B) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}
