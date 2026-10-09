//! Long-lived owner of the active [`Engine`] and of in-flight generations.
//!
//! The Tauri command layer and the Axum server each hold one manager, so
//! `stop_generation` can cancel a stream that was started by either entry point.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use smollm_core::chat::{
    CancelToken, ChatRequest, EngineMetrics, GenToken, GenerationRequest, LoadModelRequest,
    LoadModelResponse, ModelHandle, SamplingParams,
};
use smollm_core::model::ModelFamily;
use smollm_core::{AppError, AppResult};

use crate::{Engine, EngineKind, TokenStream};

/// Registers every live stream by request id so cancellation works from any
/// command, including one issued while a stream is mid-flight.
pub struct EngineManager {
    engine: Box<dyn Engine>,
    loaded: Option<ModelHandle>,
    family: ModelFamily,
    in_flight: Arc<Mutex<HashMap<String, CancelToken>>>,
}

impl Default for EngineManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineManager {
    pub fn new() -> Self {
        Self::with_kind(EngineKind::Mock)
    }

    pub fn with_kind(kind: EngineKind) -> Self {
        Self::with_engine(kind.build())
    }

    pub fn with_engine(engine: Box<dyn Engine>) -> Self {
        Self {
            engine,
            loaded: None,
            family: ModelFamily::default(),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn engine_name(&self) -> String {
        self.engine.name().to_string()
    }

    pub fn is_simulated(&self) -> bool {
        self.engine.is_simulated()
    }

    pub fn loaded_handle(&self) -> Option<ModelHandle> {
        self.loaded.clone()
    }

    pub fn metrics(&self) -> EngineMetrics {
        self.engine.metrics()
    }

    pub fn tokenize(&self, text: &str) -> AppResult<Vec<u32>> {
        self.engine.tokenize(text)
    }

    /// Generations currently streaming; shown on the Server page.
    pub fn active_requests(&self) -> usize {
        lock(&self.in_flight).len()
    }

    pub fn load(&mut self, request: LoadModelRequest) -> AppResult<LoadModelResponse> {
        let mut request = request;
        let mut warnings = Vec::new();
        if request.path.as_os_str().is_empty() {
            warnings.push(
                "no local .gguf file was supplied, so metadata comes from the catalog".to_string(),
            );
        }
        if self.engine.is_simulated() {
            warnings.push(format!(
                "{} engine is simulated: answers are not produced by model weights",
                self.engine.name()
            ));
        }
        if let Some((wanted, usable)) = cap_threads(&mut request) {
            warnings.push(format!(
                "{wanted} decode threads asked for, this machine runs {usable} at once, so the model loads with {usable}"
            ));
        }

        let family = family_for(&request);
        let handle = self.engine.load_model(request)?;
        let response = LoadModelResponse {
            handle: handle.clone(),
            engine: self.engine.name().to_string(),
            simulated: self.engine.is_simulated(),
            warnings,
        };
        self.family = family;
        self.loaded = Some(handle);
        Ok(response)
    }

    pub fn unload(&mut self) -> AppResult<()> {
        self.cancel_all();
        if let Some(handle) = self.loaded.take() {
            self.engine.unload_model(handle)?;
        }
        self.family = ModelFamily::default();
        Ok(())
    }

    /// Replace the loaded model, unloading the previous one first.
    pub fn switch(&mut self, request: LoadModelRequest) -> AppResult<LoadModelResponse> {
        if self.loaded.is_some() {
            self.unload()?;
        }
        self.load(request)
    }

    /// Start a chat generation and register its cancellation token.
    pub fn start_chat(&mut self, request: ChatRequest) -> AppResult<TokenStream> {
        request.validate()?;
        let prompt = self.render_prompt(&request);
        self.start_generation(request.request_id, prompt, request.params, request.stop)
    }

    /// Generate from a raw prompt, used by `/v1/completions` and benchmarks.
    pub fn start_generation(
        &mut self,
        request_id: String,
        prompt: String,
        params: SamplingParams,
        stop: Vec<String>,
    ) -> AppResult<TokenStream> {
        let loaded = self.loaded.is_some() || self.engine.metrics().loaded;
        if !loaded {
            return Err(AppError::ModelNotFound(
                "load a model before generating".to_string(),
            ));
        }
        if request_id.is_empty() {
            return Err(AppError::InvalidRequest(
                "requestId must not be empty".to_string(),
            ));
        }
        let cancel = CancelToken::new();
        lock(&self.in_flight).insert(request_id.clone(), cancel.clone());

        let generation = GenerationRequest {
            request_id: request_id.clone(),
            prompt,
            params,
            stop,
            cancel,
        };

        let stream = match self.engine.generate(generation) {
            Ok(stream) => stream,
            Err(error) => {
                lock(&self.in_flight).remove(&request_id);
                return Err(error);
            }
        };

        Ok(Box::pin(ManagedStream::new(
            stream,
            Arc::clone(&self.in_flight),
            request_id,
        )))
    }

    /// Cancel one in-flight generation. Returns false when the id is unknown,
    /// which lets the UI distinguish "already finished" from "never started".
    pub fn stop(&self, request_id: &str) -> bool {
        if let Some(cancel) = lock(&self.in_flight).remove(request_id) {
            cancel.cancel();
            return true;
        }
        false
    }

    /// Cancel everything, e.g. before unloading or on shutdown.
    pub fn cancel_all(&self) -> usize {
        let mut registry = lock(&self.in_flight);
        let count = registry.len();
        for cancel in registry.values() {
            cancel.cancel();
        }
        registry.clear();
        count
    }

    /// Flatten a transcript into the prompt text this model expects.
    ///
    /// A native engine that knows the model's own chat template wins over the
    /// architecture table, because the file's template is the real thing.
    pub fn render_prompt(&self, request: &ChatRequest) -> String {
        let messages = &request.messages;
        match self
            .engine
            .render_chat_prompt(request.system_prompt.as_deref(), messages)
        {
            Some(prompt) => prompt,
            None => self
                .family
                .render_prompt(request.system_prompt.as_deref(), messages),
        }
    }

    /// The prompt family in use, so the UI can explain why a model was prompted
    /// a particular way.
    pub fn family(&self) -> ModelFamily {
        self.family
    }
}

/// Lower a thread count the host cannot fill, returning what was changed.
///
/// A model loaded with 64 threads on a 10-core laptop is not faster: the extra
/// threads start, contend for the same cores and idle. The engine reports the
/// cap as a warning instead of quietly ignoring the setting the user just chose.
fn cap_threads(request: &mut LoadModelRequest) -> Option<(u32, u32)> {
    let wanted = request.options.threads.filter(|count| *count > 0)?;
    // No opinion from the platform means no cap to apply.
    let Ok(cores) = std::thread::available_parallelism() else {
        return None;
    };
    let usable = u32::try_from(cores.get()).unwrap_or(u32::MAX);
    if wanted <= usable {
        return None;
    }
    request.options.threads = Some(usable);
    Some((wanted, usable))
}

/// One loaded model per manager: llama.cpp keeps weights resident, and the
/// product scope is small local models, not multi-model serving.
fn family_for(request: &LoadModelRequest) -> ModelFamily {
    let architecture = request
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.architecture.as_deref())
        .unwrap_or("");
    let from_arch = ModelFamily::from_hint(architecture);
    if from_arch != ModelFamily::Other {
        return from_arch;
    }
    ModelFamily::from_hint(&request.model_id)
}

/// Wrapper that removes its request from the cancellation registry as soon as
/// the stream terminates, errors, or is dropped mid-flight.
struct ManagedStream {
    inner: TokenStream,
    in_flight: Arc<Mutex<HashMap<String, CancelToken>>>,
    request_id: String,
}

impl ManagedStream {
    fn new(
        inner: TokenStream,
        in_flight: Arc<Mutex<HashMap<String, CancelToken>>>,
        request_id: String,
    ) -> Self {
        Self {
            inner,
            in_flight,
            request_id,
        }
    }

    fn deregister(&self) {
        lock(&self.in_flight).remove(&self.request_id);
    }
}

impl Stream for ManagedStream {
    type Item = AppResult<GenToken>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(item)) => {
                let terminal = item
                    .as_ref()
                    .map(|token| token.finish_reason.is_some())
                    .unwrap_or(true);
                if terminal {
                    this.deregister();
                }
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                this.deregister();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ManagedStream {
    fn drop(&mut self) {
        // A consumer that abandons the stream still must not leak its token.
        self.deregister();
    }
}

fn lock<T>(mutex: &Arc<Mutex<T>>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Manager behaviour is exercised through MockEngine, the only engine in this
/// crate that streams, so the suite exists only where that feature does. The
/// crate still builds and its other tests still run with `mock` disabled.
#[cfg(all(test, feature = "mock"))]
mod tests {
    use super::*;
    use futures::StreamExt;
    use smollm_core::chat::{chat_message, ChatMessage, LoadModelOptions, Role, SamplingParams};
    use smollm_core::model::ModelMetadata;
    use smollm_core::system::Backend;

    fn chat(model_id: &str, text: &str) -> ChatRequest {
        ChatRequest {
            request_id: uuid::Uuid::new_v4().simple().to_string(),
            model_id: model_id.to_string(),
            messages: vec![chat_message(Role::User, text)],
            params: SamplingParams {
                max_tokens: 64,
                ..SamplingParams::default()
            },
            ..ChatRequest::default()
        }
    }

    fn load(manager: &mut EngineManager, model_id: &str) {
        manager
            .load(LoadModelRequest {
                model_id: model_id.to_string(),
                display_name: "Test Model".to_string(),
                ..LoadModelRequest::default()
            })
            .expect("mock loads without a file");
    }

    async fn drain(stream: TokenStream) -> Vec<GenToken> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    #[tokio::test]
    async fn load_reports_the_engine_and_its_warnings() {
        let mut manager = EngineManager::new();
        let response = manager
            .load(LoadModelRequest {
                model_id: "qwen2.5-0.5b-instruct-gguf".to_string(),
                options: LoadModelOptions {
                    backend: Backend::Metal,
                    ..LoadModelOptions::default()
                },
                metadata: Some(ModelMetadata {
                    architecture: Some("qwen2".to_string()),
                    ..Default::default()
                }),
                ..LoadModelRequest::default()
            })
            .expect("loads");
        assert_eq!(response.engine, "mock");
        assert!(response.simulated);
        assert!(response.warnings.iter().any(|w| w.contains("simulated")));
        assert!(manager.is_simulated());
        assert_eq!(
            manager.loaded_handle().map(|h| h.model_id).as_deref(),
            Some("qwen2.5-0.5b-instruct-gguf")
        );
        assert_eq!(manager.family(), ModelFamily::Qwen2);
    }

    fn request_with_threads(threads: Option<u32>) -> LoadModelRequest {
        LoadModelRequest {
            model_id: "smollm2-135m-gguf".to_string(),
            options: LoadModelOptions {
                threads,
                ..LoadModelOptions::default()
            },
            ..LoadModelRequest::default()
        }
    }

    /// The cap is judged against this machine, not a hard-coded core count, so
    /// the same assertion holds on a laptop and on CI.
    #[test]
    fn more_threads_than_the_host_has_are_lowered_and_the_rest_pass_through() {
        let cores = std::thread::available_parallelism()
            .expect("the host reports usable parallelism")
            .get()
            .try_into()
            .unwrap_or(u32::MAX);

        assert_eq!(cap_threads(&mut request_with_threads(None)), None);
        assert_eq!(cap_threads(&mut request_with_threads(Some(0))), None);
        assert_eq!(
            cap_threads(&mut request_with_threads(Some(cores))),
            None,
            "exactly what the host runs is not a cap"
        );

        let wanted = cores.saturating_mul(4).saturating_add(4);
        let mut request = request_with_threads(Some(wanted));
        assert_eq!(cap_threads(&mut request), Some((wanted, cores)));
        assert_eq!(request.options.threads, Some(cores));
    }

    #[tokio::test]
    async fn streaming_a_chat_completes_and_clears_the_registry() {
        let mut manager = EngineManager::new();
        load(&mut manager, "qwen2.5-0.5b-instruct-gguf");
        let request = chat("qwen2.5-0.5b-instruct-gguf", "explain ownership");
        let id = request.request_id.clone();
        let tokens = drain(manager.start_chat(request).expect("streams")).await;
        assert!(tokens.len() > 4);
        assert_eq!(manager.active_requests(), 0, "terminal token deregisters");
        assert!(
            !manager.stop(&id),
            "a finished request cannot be stopped twice"
        );
    }

    #[tokio::test]
    async fn stop_cancels_a_live_stream() {
        let mut manager = EngineManager::with_kind(EngineKind::Mock);
        load(&mut manager, "smollm2-135m-gguf");
        let request = chat("smollm2-135m-gguf", "write at length please");
        let id = request.request_id.clone();
        let mut stream = manager.start_chat(request).expect("streams");
        assert_eq!(manager.active_requests(), 1);

        stream.next().await.expect("first chunk").expect("ok");
        assert!(manager.stop(&id));
        // Cancelling the registry entry also flips the token the engine holds.
        let rest = drain(stream).await;
        let last = rest.last().expect("terminal token");
        assert_eq!(last.finish_reason.as_deref(), Some("stop"));
        assert_eq!(manager.active_requests(), 0);
    }

    #[tokio::test]
    async fn dropping_a_stream_deregisters_it() {
        let mut manager = EngineManager::new();
        load(&mut manager, "llama-3.2-1b-gguf");
        let stream = manager
            .start_chat(chat("llama-3.2-1b-gguf", "hello there"))
            .expect("streams");
        assert_eq!(manager.active_requests(), 1);
        drop(stream);
        assert_eq!(manager.active_requests(), 0);
    }

    #[tokio::test]
    async fn generating_without_a_loaded_model_errors() {
        let mut manager = EngineManager::new();
        let error = manager
            .start_chat(chat("nope", "hello"))
            .err()
            .expect("needs a load first");
        assert!(matches!(error, AppError::ModelNotFound(_)));
        assert_eq!(manager.active_requests(), 0);
    }

    #[tokio::test]
    async fn empty_messages_are_rejected_before_the_engine_runs() {
        let mut manager = EngineManager::new();
        load(&mut manager, "phi-3.5-mini-gguf");
        let request = ChatRequest {
            request_id: "req-empty".to_string(),
            model_id: "phi-3.5-mini-gguf".to_string(),
            messages: Vec::<ChatMessage>::new(),
            ..ChatRequest::default()
        };
        assert!(manager
            .start_chat(request)
            .is_err_and(|error| matches!(error, AppError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn unload_cancels_everything_in_flight() {
        let mut manager = EngineManager::new();
        load(&mut manager, "gemma-2-2b-gguf");
        let _stream = manager
            .start_chat(chat("gemma-2-2b-gguf", "keep going"))
            .expect("streams");
        assert_eq!(manager.active_requests(), 1);
        manager.unload().expect("unloads");
        assert_eq!(manager.active_requests(), 0);
        assert!(manager.loaded_handle().is_none());
        assert!(manager
            .start_chat(chat("gemma-2-2b-gguf", "after unload"))
            .is_err());
    }

    #[tokio::test]
    async fn switch_replaces_the_loaded_model() {
        let mut manager = EngineManager::new();
        load(&mut manager, "qwen2.5-0.5b-instruct-gguf");
        let response = manager
            .switch(LoadModelRequest {
                model_id: "ling-mini-1.7b-gguf".to_string(),
                ..LoadModelRequest::default()
            })
            .expect("switch loads");
        assert_eq!(response.handle.model_id, "ling-mini-1.7b-gguf");
        assert_eq!(manager.active_requests(), 0);
    }

    #[test]
    fn prompts_follow_the_detected_family() {
        let mut manager = EngineManager::new();
        manager
            .load(LoadModelRequest {
                model_id: "SmolLM2-360M-Instruct".to_string(),
                ..LoadModelRequest::default()
            })
            .expect("loads");
        let prompt = manager.render_prompt(&chat("x", "hi"));
        assert!(prompt.contains("<user>"), "got {prompt}");
        assert!(prompt.ends_with("<assistant>\n"));
    }

    #[test]
    fn metrics_and_tokenizer_pass_through() {
        let manager = EngineManager::new();
        assert_eq!(manager.engine_name(), "mock");
        assert!(!manager.metrics().loaded);
        assert_eq!(manager.tokenize("a b c").expect("tokenises").len(), 3);
    }
}
