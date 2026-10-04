//! Axum handlers for the OpenAI-compatible surface.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use smollm_core::chat::{ChatRequest, Role, SamplingParams};
use smollm_core::error::AppError;
use smollm_engine::EngineManager;
use tower_http::cors::CorsLayer;

use crate::openai::{
    completion_id, now_epoch, ChatCompletionRequest, CompletionRequest, ModelCard, ModelList,
    OpenAiErrorBody, OpenAiErrorFields,
};
use crate::state::{now_millis, SharedState};
use crate::stream::{aggregate, chat_response, sse_chat, sse_text, text_response};

/// Every client-visible failure, shaped like OpenAI's error envelope.
pub struct ApiError {
    pub status: StatusCode,
    pub body: OpenAiErrorBody,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>, kind: &'static str) -> Self {
        Self {
            status,
            body: OpenAiErrorBody::new(message, kind, None),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message, "invalid_request_error")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message, "not_found_error")
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            message,
            "internal_server_error",
        )
    }

    /// Map an app error onto an HTTP status, keeping the stable code for clients.
    pub fn from_app_error(error: AppError) -> Self {
        let message = error.to_string();
        let code = Some(error.code().to_string());
        let (status, kind) = match &error {
            AppError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            AppError::ModelNotFound(_) | AppError::ModelNotDownloaded(_) => {
                (StatusCode::NOT_FOUND, "not_found_error")
            }
            AppError::UnsupportedBackend(_) | AppError::NotImplemented(_) => {
                (StatusCode::NOT_IMPLEMENTED, "model_not_supported")
            }
            AppError::ServerNotRunning => (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_server_error"),
        };
        Self {
            status,
            body: OpenAiErrorBody {
                error: OpenAiErrorFields {
                    message,
                    r#type: kind,
                    param: None,
                    code,
                },
            },
        }
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        Self::from_app_error(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// The full router. Binding to loopback happens in [`crate::serve`], not here.
pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/models/{model_id}", get(retrieve_model))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/engine/metrics", get(engine_metrics))
        .fallback(not_found)
        // The server only ever listens on loopback, so permissive CORS just lets
        // local tools (browsers, notebooks, SDKs) reach it without a proxy.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let (engine, model, simulated, engine_requests) = describe_engine(&state);
    Json(serde_json::json!({
        "status": "ok",
        "version": state.app_version,
        "engine": engine,
        "simulated": simulated,
        "model": model,
        "engineRequests": engine_requests,
        "httpRequests": state.request_count(),
        "uptimeSeconds": state.uptime_seconds(),
        "servedModels": state.served_model_ids(),
        "endpoints": [
            "/health",
            "/v1/models",
            "/v1/chat/completions",
            "/v1/completions",
            "/v1/engine/metrics"
        ],
        "timestamp": now_millis(),
    }))
}

async fn list_models(State(state): State<SharedState>) -> Json<ModelList> {
    let created = now_epoch();
    let owned_by = engine_label(&state);
    Json(ModelList {
        object: "list",
        data: state
            .served_model_ids()
            .into_iter()
            .map(|id| ModelCard::new(id, created, owned_by.clone()))
            .collect(),
    })
}

async fn retrieve_model(
    State(state): State<SharedState>,
    Path(model_id): Path<String>,
) -> Result<Json<ModelCard>, ApiError> {
    if state
        .served_model_ids()
        .iter()
        .any(|served| served == &model_id)
    {
        return Ok(Json(ModelCard::new(
            model_id,
            now_epoch(),
            engine_label(&state),
        )));
    }
    Err(ApiError::not_found(format!(
        "model `{model_id}` is not served"
    )))
}

async fn engine_metrics(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let manager = lock_engine(&state)?;
    let metrics = manager.metrics();
    let active_requests = manager.active_requests();
    drop(manager);
    Ok(Json(serde_json::json!({
        "engine": metrics,
        "activeRequests": active_requests,
        "httpRequests": state.request_count(),
        "uptimeSeconds": state.uptime_seconds(),
    })))
}

async fn chat_completions(
    State(state): State<SharedState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    request.validate().map_err(ApiError::invalid)?;
    let model = state.resolve_model(request.model.as_deref())?;

    let messages: Vec<_> = request
        .messages
        .iter()
        .filter(|message| message.role != Role::Assistant || !message.content.is_empty())
        .cloned()
        .collect();
    if messages.is_empty() {
        return Err(ApiError::invalid(
            "'messages' must contain at least one non-empty message",
        ));
    }

    let params = build_params(
        request.temperature,
        request.top_p,
        request.max_tokens(),
        request.seed,
        request.presence_penalty,
    );
    let chat = ChatRequest {
        request_id: completion_id("chatcmpl"),
        model_id: model.clone(),
        messages,
        system_prompt: request.system_prompt().map(str::to_string),
        params,
        stop: request.stop_sequences(),
    };
    let id = chat.request_id.clone();
    let prompt = prompt_summary(&chat);
    let stream = {
        let mut manager = lock_engine(&state)?;
        manager.start_chat(chat)?
    };

    let index = state.record_request();
    state.log(
        "info",
        format!(
            "POST /v1/chat/completions model={model} stream={} (#{index})",
            request.stream
        ),
    );

    if request.stream {
        return Ok(sse_chat(id, model, engine_fingerprint(&state), stream).into_response());
    }
    let aggregated = aggregate(stream).await;
    if let Some(error) = aggregated.error {
        return Err(ApiError::from_app_error(error));
    }
    Ok(Json(chat_response(
        id,
        model,
        engine_fingerprint(&state),
        &prompt,
        &aggregated,
    ))
    .into_response())
}

async fn completions(
    State(state): State<SharedState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Response, ApiError> {
    let prompt = request.prompt.as_str().to_string();
    if prompt.trim().is_empty() {
        return Err(ApiError::invalid("'prompt' must not be empty"));
    }
    if matches!(request.n, Some(n) if n > 1) {
        return Err(ApiError::invalid("'n' greater than 1 is not supported"));
    }
    let model = state.resolve_model(request.model.as_deref())?;
    let params = build_params(
        request.temperature,
        request.top_p,
        request.max_tokens,
        request.seed,
        None,
    );
    let stop = request
        .stop
        .as_ref()
        .map(|stop| stop.to_vec())
        .unwrap_or_default();

    let id = completion_id("cmpl");
    let stream = {
        let mut manager = lock_engine(&state)?;
        manager.start_generation(id.clone(), prompt.clone(), params, stop)?
    };

    let index = state.record_request();
    state.log(
        "info",
        format!(
            "POST /v1/completions model={model} stream={} (#{index})",
            request.stream
        ),
    );

    if request.stream {
        return Ok(sse_text(id, model, engine_fingerprint(&state), stream).into_response());
    }
    let aggregated = aggregate(stream).await;
    if let Some(error) = aggregated.error {
        return Err(ApiError::from_app_error(error));
    }
    Ok(Json(text_response(
        id,
        model,
        engine_fingerprint(&state),
        &prompt,
        &aggregated,
    ))
    .into_response())
}

async fn not_found() -> ApiError {
    ApiError::not_found("unknown endpoint; try GET /health or POST /v1/chat/completions")
}

/// Sampling params from OpenAI fields, on top of the app's small-model defaults.
///
/// `frequency_penalty` is accepted for SDK compatibility but not applied: our
/// backends expose presence penalty only, and pretending otherwise would lie.
fn build_params(
    temperature: Option<f32>,
    top_p: Option<f32>,
    max_tokens: Option<u32>,
    seed: Option<i64>,
    presence_penalty: Option<f32>,
) -> SamplingParams {
    let mut params = SamplingParams::default();
    if let Some(temperature) = temperature {
        params.temperature = temperature;
    }
    if let Some(top_p) = top_p {
        params.top_p = top_p;
    }
    if let Some(max_tokens) = max_tokens {
        params.max_tokens = max_tokens;
    }
    if let Some(seed) = seed {
        params.seed = Some(seed);
    }
    if let Some(presence_penalty) = presence_penalty {
        params.presence_penalty = presence_penalty;
    }
    params
}

/// Cheap stand-in for tokenizer counts when an engine reports no usage.
fn prompt_summary(request: &ChatRequest) -> String {
    let mut text = request.system_prompt.clone().unwrap_or_default();
    for message in &request.messages {
        text.push(' ');
        text.push_str(&message.content);
    }
    text
}

fn lock_engine(state: &SharedState) -> Result<std::sync::MutexGuard<'_, EngineManager>, ApiError> {
    state
        .engine
        .lock()
        .map_err(|_| ApiError::internal("engine is unavailable after a panic"))
}

fn engine_label(state: &SharedState) -> String {
    lock_engine(state)
        .map(|manager| manager.engine_name())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// `system_fingerprint` advertises the backend, so a client can tell these
/// answers came from SmolLLM Studio rather than a hosted API.
fn engine_fingerprint(state: &SharedState) -> String {
    format!("fp-{}-{}", engine_label(state), state.app_version)
}

pub(crate) fn describe_engine(state: &SharedState) -> (String, Option<String>, bool, u64) {
    match lock_engine(state) {
        Ok(manager) => {
            let metrics = manager.metrics();
            (
                metrics.engine,
                manager.loaded_handle().map(|handle| handle.model_id),
                manager.is_simulated(),
                metrics.requests,
            )
        }
        Err(_) => ("unavailable".to_string(), None, true, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::chat::chat_message;

    #[test]
    fn openai_fields_override_the_app_defaults() {
        let params = build_params(Some(0.2), Some(0.5), Some(32), Some(7), Some(0.3));
        assert_eq!(params.temperature, 0.2);
        assert_eq!(params.top_p, 0.5);
        assert_eq!(params.max_tokens, 32);
        assert_eq!(params.seed, Some(7));
        assert_eq!(params.presence_penalty, 0.3);

        let defaults = build_params(None, None, None, None, None);
        assert_eq!(defaults.max_tokens, SamplingParams::default().max_tokens);
        assert_eq!(defaults.temperature, SamplingParams::default().temperature);
    }

    #[test]
    fn prompt_summary_covers_the_system_prompt() {
        let request = ChatRequest {
            request_id: "r".to_string(),
            model_id: "m".to_string(),
            messages: vec![
                chat_message(Role::System, "be brief"),
                chat_message(Role::User, "hello there"),
            ],
            system_prompt: Some("be brief".to_string()),
            ..ChatRequest::default()
        };
        let summary = prompt_summary(&request);
        assert!(summary.contains("be brief"));
        assert!(summary.contains("hello there"));
    }

    #[test]
    fn app_errors_map_onto_openai_statuses() {
        let cases = [
            (
                AppError::InvalidRequest("x".into()),
                StatusCode::BAD_REQUEST,
            ),
            (AppError::ModelNotFound("x".into()), StatusCode::NOT_FOUND),
            (
                AppError::ModelNotDownloaded("x".into()),
                StatusCode::NOT_FOUND,
            ),
            (
                AppError::UnsupportedBackend("x".into()),
                StatusCode::NOT_IMPLEMENTED,
            ),
            (AppError::ServerNotRunning, StatusCode::SERVICE_UNAVAILABLE),
            (
                AppError::GenerationFailed("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let mapped = ApiError::from_app_error(error);
            assert_eq!(mapped.status, status);
            assert!(mapped.body.error.message.len() > 3);
        }
    }
}
