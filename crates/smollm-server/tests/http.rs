//! End-to-end checks against a real server on an ephemeral loopback port.
//!
//! These are the acceptance criteria for the local API: `/health`, `/v1/models`,
//! `/v1/chat/completions` in both JSON and SSE form, plus the error envelope.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use smollm_core::chat::LoadModelRequest;
use smollm_core::system::ServerConfig;
use smollm_engine::mock::{MockConfig, MockEngine};
use smollm_engine::EngineManager;
use smollm_server::{serve_at, ServerHandle, ServerState};

const MODEL: &str = "qwen2.5-0.5b-instruct-gguf";

/// A loaded manager with no artificial token delay, so tests stay fast while
/// still exercising the real streaming machinery.
fn loaded_manager() -> Arc<Mutex<EngineManager>> {
    let manager =
        EngineManager::with_engine(Box::new(MockEngine::with_config(MockConfig::instant())));
    let shared = Arc::new(Mutex::new(manager));
    shared
        .lock()
        .expect("unlocked")
        .load(LoadModelRequest {
            model_id: MODEL.to_string(),
            display_name: "Qwen2.5 0.5B".to_string(),
            ..LoadModelRequest::default()
        })
        .expect("mock loads");
    shared
}

struct Started {
    handle: ServerHandle,
    base: String,
    state: Arc<ServerState>,
}

async fn start() -> Started {
    let state = Arc::new(ServerState::new(
        loaded_manager(),
        ServerConfig::default(),
        "0.1.0-test",
    ));
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("loopback any port");
    let handle = serve_at(Arc::clone(&state), addr).await.expect("serves");
    let base = handle.base_url();
    Started {
        handle,
        base,
        state,
    }
}

fn chat_body(stream: bool) -> serde_json::Value {
    serde_json::json!({
        "model": MODEL,
        "messages": [{ "role": "user", "content": "Explain Rust ownership briefly." }],
        "stream": stream,
        "temperature": 0.7,
        "max_tokens": 48
    })
}

#[tokio::test]
async fn health_reports_the_engine_and_its_models() {
    let started = start().await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", started.base))
        .send()
        .await
        .expect("reaches the server");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], "0.1.0-test");
    assert_eq!(body["engine"], "mock");
    assert_eq!(
        body["simulated"], true,
        "the API must admit it is simulated"
    );
    assert_eq!(body["model"], MODEL);
    assert!(body["servedModels"]
        .as_array()
        .expect("array")
        .contains(&serde_json::json!(MODEL)));
    assert!(body["endpoints"].as_array().expect("array").len() >= 4);

    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn models_endpoint_matches_the_openai_shape() {
    let started = start().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/v1/models", started.base))
        .send()
        .await
        .expect("lists")
        .json()
        .await
        .expect("json");

    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().expect("array");
    assert!(!data.is_empty());
    assert_eq!(data[0]["id"], MODEL);
    assert_eq!(data[0]["object"], "model");
    assert!(data[0]["created"].as_u64().expect("created") > 0);
    assert_eq!(data[0]["owned_by"], "mock");

    let single: serde_json::Value = client
        .get(format!("{}/v1/models/{MODEL}", started.base))
        .send()
        .await
        .expect("fetches")
        .json()
        .await
        .expect("json");
    assert_eq!(single["id"], MODEL);

    let missing = client
        .get(format!("{}/v1/models/gpt-4o", started.base))
        .send()
        .await
        .expect("404s");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn chat_completions_returns_a_valid_openai_response() {
    let started = start().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/chat/completions", started.base))
        .header("Authorization", "Bearer local")
        .json(&chat_body(false))
        .send()
        .await
        .expect("posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        "application/json",
    );

    let body: serde_json::Value = response.json().await.expect("json");
    assert!(body["id"].as_str().expect("id").starts_with("chatcmpl-"));
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], MODEL);
    let choices = body["choices"].as_array().expect("choices");
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0]["message"]["role"], "assistant");
    let content = choices[0]["message"]["content"].as_str().expect("content");
    assert!(
        content.len() > 20,
        "expected a real answer, got {content:?}"
    );
    assert!(
        content.contains("MockEngine simulated this answer"),
        "answers must not pretend to be real inference: {content:?}"
    );
    assert_eq!(choices[0]["finish_reason"], "stop");
    assert!(body["usage"]["completion_tokens"].as_u64().expect("tokens") > 0);
    assert_eq!(
        body["usage"]["total_tokens"].as_u64().expect("total"),
        body["usage"]["prompt_tokens"].as_u64().expect("prompt")
            + body["usage"]["completion_tokens"]
                .as_u64()
                .expect("completion")
    );

    // The Server page counts requests for its status pill.
    assert_eq!(started.state.request_count(), 1);
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn streaming_chat_emits_sse_events_and_done() {
    let started = start().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/chat/completions", started.base))
        .json(&chat_body(true))
        .send()
        .await
        .expect("posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "text/event-stream"
    );
    // A proxy that buffers or rewrites turns a token stream into a wait.
    let no_buffering = response
        .headers()
        .get("x-accel-buffering")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(no_buffering, "no");

    let text = response.text().await.expect("body");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("data:"))
        .collect();
    assert!(lines.len() > 5, "expected many deltas, got {lines:?}");
    assert_eq!(lines.last().expect("last").trim(), "data: [DONE]");

    let first: serde_json::Value =
        serde_json::from_str(lines[0].trim_start_matches("data:").trim()).expect("first chunk");
    assert_eq!(first["object"], "chat.completion.chunk");
    assert_eq!(first["choices"][0]["delta"]["role"], "assistant");

    // Reassemble the answer from the deltas, the way a client would.
    let mut answer = String::new();
    let mut finish = None;
    for line in &lines[1..lines.len() - 1] {
        let chunk: serde_json::Value =
            serde_json::from_str(line.trim_start_matches("data:").trim()).expect("chunk");
        if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str() {
            answer.push_str(text);
        }
        finish = chunk["choices"][0]["finish_reason"]
            .as_str()
            .map(str::to_string);
    }
    assert!(
        answer.contains("MockEngine simulated this answer"),
        "{answer}"
    );
    assert_eq!(finish.as_deref(), Some("stop"));

    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn content_parts_and_null_are_accepted_over_the_socket() {
    // Real SDKs send `content` as a list of typed parts. Rejecting that with a
    // 422 would break them, so the parts must reach the engine joined.
    let started = start().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/chat/completions", started.base))
        .json(&serde_json::json!({
            "model": MODEL,
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "be brief" }] },
                { "role": "assistant", "content": null },
                { "role": "user", "content": [
                    { "type": "text", "text": "Explain " },
                    { "type": "image_url", "image_url": { "url": "https://example.invalid/x.png" } },
                    { "type": "text", "text": "token budgets" }
                ] }
            ]
        }))
        .send()
        .await
        .expect("posts");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("json");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .expect("content");
    assert!(
        content.contains("token budgets"),
        "the text parts must arrive joined: {content:?}"
    );
    assert!(
        !content.contains("image_url") && !content.contains("example.invalid"),
        "non-text parts are dropped, not stringified: {content:?}"
    );
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn streamed_usage_only_appears_when_the_client_asks() {
    let started = start().await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", started.base);

    let with_usage = client
        .post(&url)
        .json(&serde_json::json!({
            "model": MODEL,
            "messages": [{ "role": "user", "content": "Explain Rust ownership briefly." }],
            "stream": true,
            "stream_options": { "include_usage": true }
        }))
        .send()
        .await
        .expect("posts");
    let text = with_usage.text().await.expect("body");
    let lines: Vec<serde_json::Value> = data_chunks(&text);

    assert_eq!(lines.last().expect("done").as_str(), Some("[DONE]"));
    let usage = lines[lines.len() - 2].clone();
    assert_eq!(
        usage["choices"].as_array().expect("choices").len(),
        0,
        "OpenAI's usage chunk carries no choices"
    );
    assert!(usage["usage"].is_object(), "got {usage}");
    assert!(usage["usage"]["completion_tokens"].as_u64().expect("t") > 0);
    assert_eq!(
        usage["usage"]["total_tokens"].as_u64().expect("total"),
        usage["usage"]["prompt_tokens"].as_u64().expect("prompt")
            + usage["usage"]["completion_tokens"]
                .as_u64()
                .expect("completion")
    );

    // The default: a strict client that never asked for usage must not receive it.
    let without = client
        .post(&url)
        .json(&chat_body(true))
        .send()
        .await
        .expect("posts");
    let text = without.text().await.expect("body");
    let lines: Vec<serde_json::Value> = data_chunks(&text);
    assert_eq!(lines.last().expect("done").as_str(), Some("[DONE]"));
    for chunk in &lines[..lines.len() - 1] {
        assert!(
            chunk.get("usage").is_none(),
            "content chunks never carry usage: {chunk}"
        );
        assert!(!chunk["choices"].as_array().expect("choices").is_empty());
    }
    started.handle.stop().await.expect("stops");
}

/// Every `data:` payload of an SSE body, with the marker stripped.
fn data_chunks(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter(|line| line.starts_with("data:"))
        .map(|line| line.trim_start_matches("data:").trim())
        .map(|payload| {
            serde_json::from_str(payload)
                .unwrap_or_else(|_| serde_json::Value::String(payload.to_string()))
        })
        .collect()
}

#[tokio::test]
async fn legacy_completions_are_served_too() {
    let started = start().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .post(format!("{}/v1/completions", started.base))
        .json(&serde_json::json!({
            "model": MODEL,
            "prompt": "Write a haiku about local inference.",
            "max_tokens": 512
        }))
        .send()
        .await
        .expect("posts")
        .json()
        .await
        .expect("json");

    assert!(body["id"].as_str().expect("id").starts_with("cmpl-"));
    assert_eq!(body["object"], "text_completion");
    assert_eq!(body["choices"][0]["logprobs"], serde_json::Value::Null);
    assert!(!body["choices"][0]["text"]
        .as_str()
        .expect("text")
        .is_empty());
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn errors_use_the_openai_error_envelope() {
    let started = start().await;
    let client = reqwest::Client::new();

    let unknown = client
        .post(format!("{}/v1/chat/completions", started.base))
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("posts");
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = unknown.json().await.expect("json");
    assert_eq!(body["error"]["type"], "not_found_error");
    assert!(body["error"]["message"]
        .as_str()
        .expect("msg")
        .contains("gpt-4o"));
    assert_eq!(body["error"]["code"], "model_not_found");

    let empty = client
        .post(format!("{}/v1/chat/completions", started.base))
        .json(&serde_json::json!({ "model": MODEL, "messages": [] }))
        .send()
        .await
        .expect("posts");
    assert_eq!(empty.status(), reqwest::StatusCode::BAD_REQUEST);

    let malformed = client
        .post(format!("{}/v1/chat/completions", started.base))
        .header("Content-Type", "application/json")
        .body("not json at all")
        .send()
        .await
        .expect("posts");
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
    // A client that cannot parse the body still needs an envelope it can read.
    let body: serde_json::Value = malformed.json().await.expect("json envelope");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "body");
    assert!(body["error"]["message"]
        .as_str()
        .expect("msg")
        .contains("not valid OpenAI JSON"));

    let unknown_route = client
        .get(format!("{}/v1/nope", started.base))
        .send()
        .await
        .expect("404s");
    assert_eq!(unknown_route.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = unknown_route.json().await.expect("json envelope");
    assert!(body["error"]["message"]
        .as_str()
        .expect("msg")
        .contains("/v1/chat/completions"));

    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn engine_metrics_track_generated_requests() {
    let started = start().await;
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let _: serde_json::Value = client
            .post(format!("{}/v1/chat/completions", started.base))
            .json(&chat_body(false))
            .send()
            .await
            .expect("posts")
            .json()
            .await
            .expect("json");
    }

    let body: serde_json::Value = client
        .get(format!("{}/v1/engine/metrics", started.base))
        .send()
        .await
        .expect("metrics")
        .json()
        .await
        .expect("json");
    assert_eq!(body["engine"]["engine"], "mock");
    assert_eq!(body["engine"]["requests"], 2);
    assert_eq!(body["httpRequests"], 2);
    assert_eq!(body["activeRequests"], 0, "completed streams deregister");
    assert!(
        body["engine"]["tokensGenerated"]
            .as_u64()
            .expect("generated")
            > 0,
        "instant mock streams in well under a millisecond, so only counters are meaningful"
    );
    assert!(
        body["engine"]["loaded"].as_bool().expect("loaded"),
        "the loaded model is reported"
    );
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn max_tokens_caps_the_streamed_answer() {
    let started = start().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .post(format!("{}/v1/chat/completions", started.base))
        .json(&serde_json::json!({
            "model": MODEL,
            "messages": [{ "role": "user", "content": "say a lot" }],
            "max_tokens": 3
        }))
        .send()
        .await
        .expect("posts")
        .json()
        .await
        .expect("json");
    assert_eq!(body["choices"][0]["finish_reason"], "length");
    assert_eq!(body["usage"]["completion_tokens"], 3);
    started.handle.stop().await.expect("stops");
}

#[tokio::test]
async fn requests_without_a_loaded_model_are_not_served() {
    let state = Arc::new(ServerState::new(
        Arc::new(Mutex::new(EngineManager::with_engine(Box::new(
            MockEngine::with_config(MockConfig::instant()),
        )))),
        ServerConfig::default(),
        "0.1.0-test",
    ));
    let handle = serve_at(Arc::clone(&state), "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("serves");
    let client = reqwest::Client::new();

    // No model loaded and nothing advertised: even `default` cannot resolve.
    let response = client
        .post(format!("{}/v1/chat/completions", handle.base_url()))
        .json(&serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .expect("posts");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    let models: serde_json::Value = client
        .get(format!("{}/v1/models", handle.base_url()))
        .send()
        .await
        .expect("lists")
        .json()
        .await
        .expect("json");
    assert_eq!(models["data"].as_array().expect("empty list").len(), 0);

    handle.stop().await.expect("stops");
}
