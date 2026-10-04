//! Bridges the engine's token stream to OpenAI's JSON and SSE shapes.

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{self, Stream, StreamExt};
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

/// Chat SSE stream: role hint, content deltas, finish marker, then `[DONE]`.
pub fn sse_chat(
    id: String,
    model: String,
    fingerprint: String,
    stream: TokenStream,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
    let opener = stream::once(async move { Ok(opener_payload) });

    let tokens = stream.map(move |item| match item {
        Ok(token) => Ok(data_event(chat_chunk_json(
            &id,
            &model,
            &fingerprint,
            Delta {
                role: None,
                content: (!token.text.is_empty()).then_some(token.text.clone()),
            },
            map_finish(token.finish_reason.as_deref()),
            token.usage.map(Usage::from),
        ))),
        Err(error) => Ok(error_event(&crate::routes::ApiError::from(error).body)),
    });

    let done = stream::once(async { Ok(Event::default().data("[DONE]")) });
    Sse::new(opener.chain(tokens).chain(done)).keep_alive(KeepAlive::default())
}

/// Legacy completions SSE: plain `text` deltas, no role hint.
pub fn sse_text(
    id: String,
    model: String,
    fingerprint: String,
    stream: TokenStream,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let tokens = stream.map(move |item| match item {
        Ok(token) => Ok(data_event(text_chunk_json(
            &id,
            &model,
            &fingerprint,
            &token,
        ))),
        Err(error) => Ok(error_event(&crate::routes::ApiError::from(error).body)),
    });
    let done = stream::once(async { Ok(Event::default().data("[DONE]")) });
    Sse::new(tokens.chain(done)).keep_alive(KeepAlive::default())
}
