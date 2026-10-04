//! MockEngine: deterministic, streaming, cancellable fake inference.
//!
//! It exists so the whole product (chat, server, benchmarks, onboarding) is
//! usable and testable before native llama.cpp bindings land. It always reports
//! `simulated: true`, and the UI is expected to label it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::stream;
use smollm_core::chat::{
    approx_token_count, CancelToken, EngineMetrics, GenToken, GenerationRequest, LoadModelOptions,
    LoadModelRequest, ModelHandle, TokenUsage,
};
use smollm_core::gguf::GgufHeader;
use smollm_core::model::ModelMetadata;
use smollm_core::{AppError, AppResult};

use crate::{Engine, TokenStream};

/// Timing knobs for the simulated backend.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Delay between streamed chunks, so token-by-token rendering stays visible.
    pub chunk_delay_ms: u64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self { chunk_delay_ms: 18 }
    }
}

impl MockConfig {
    /// No artificial delay: used by tests and by the server's non-streaming path.
    pub fn instant() -> Self {
        Self { chunk_delay_ms: 0 }
    }
}

#[derive(Debug, Default)]
struct Stats {
    requests: u64,
    tokens_generated: u64,
    total_generation_ms: u64,
}

/// Simulated engine with real streaming semantics.
#[derive(Debug, Default)]
pub struct MockEngine {
    config: MockConfig,
    loaded: Option<ModelHandle>,
    options: LoadModelOptions,
    stats: Arc<Mutex<Stats>>,
    simulated_load_ms: u64,
}

impl MockEngine {
    pub fn new() -> Self {
        Self::with_config(MockConfig::default())
    }

    pub fn with_config(config: MockConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Simulated load time, stable per model id so benchmarks repeat.
    pub fn simulated_load_ms(&self) -> u64 {
        self.simulated_load_ms
    }

    /// Every mock answer names itself, so a screenshot cannot be mistaken for
    /// real inference.
    pub const DISCLAIMER: &'static str = "MockEngine simulated this answer; no weights were run.";
}

impl Engine for MockEngine {
    fn name(&self) -> &str {
        "mock"
    }

    fn supports_gguf(&self) -> bool {
        // It reads GGUF metadata truthfully; it just never runs the weights.
        true
    }

    fn is_simulated(&self) -> bool {
        true
    }

    fn load_model(&mut self, request: LoadModelRequest) -> AppResult<ModelHandle> {
        let mut metadata = request.metadata.unwrap_or_default();
        if request.path.exists() {
            if let Ok(header) = GgufHeader::read(&request.path) {
                merge_into(&mut metadata, &header.summary());
            }
        }

        let display_name = if request.display_name.is_empty() {
            metadata
                .name
                .clone()
                .unwrap_or_else(|| request.model_id.clone())
        } else {
            request.display_name.clone()
        };
        let context_length = match metadata.context_length {
            Some(max) if max > 0 => request.options.context_length.max(1).min(max),
            _ => request.options.context_length.max(1),
        };

        let handle = ModelHandle {
            id: uuid::Uuid::new_v4().simple().to_string(),
            model_id: request.model_id.clone(),
            display_name,
            path: if request.path.as_os_str().is_empty() {
                String::new()
            } else {
                request.path.display().to_string()
            },
            engine: self.name().to_string(),
            context_length,
            metadata,
        };

        self.simulated_load_ms = simulated_load_ms(&request.model_id);
        self.options = request.options;
        self.loaded = Some(handle.clone());
        Ok(handle)
    }

    fn unload_model(&mut self, handle: ModelHandle) -> AppResult<()> {
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.id == handle.id)
        {
            self.loaded = None;
        }
        Ok(())
    }

    fn generate(&mut self, request: GenerationRequest) -> AppResult<TokenStream> {
        if self.loaded.is_none() {
            return Err(AppError::ModelNotFound(
                "load a model before generating".to_string(),
            ));
        }

        let GenerationRequest {
            prompt,
            params,
            stop,
            cancel,
            ..
        } = request;

        let reply = apply_stop_sequences(&compose_reply(&prompt, params.seed), &stop);
        let mut chunks = split_for_streaming(&reply);
        // max_tokens is a token budget; each streamed chunk is ~one token here.
        let budget = params.max_tokens.max(1) as usize;
        let hit_budget = chunks.len() > budget;
        if hit_budget {
            chunks.truncate(budget);
        }

        let prompt_tokens = approx_token_count(&prompt);
        lock(&self.stats).requests += 1;

        let state = MockStream {
            chunks,
            index: 0,
            cancel,
            delay: Duration::from_millis(self.config.chunk_delay_ms),
            started: Instant::now(),
            prompt_tokens,
            stats: Arc::clone(&self.stats),
            finished: false,
            reason: if hit_budget { "length" } else { "eos" },
        };

        Ok(Box::pin(stream::unfold(state, |mut s| async move {
            if s.finished {
                return None;
            }
            if s.cancel.is_cancelled() {
                let item = s.seal("stop");
                return Some((item, s));
            }
            if s.index < s.chunks.len() {
                let chunk = s.chunks[s.index].clone();
                s.index += 1;
                if s.delay > Duration::ZERO {
                    tokio::time::sleep(s.delay).await;
                }
                return Some((Ok(GenToken::token(chunk)), s));
            }
            let reason = s.reason;
            let item = s.seal(reason);
            Some((item, s))
        })))
    }

    fn tokenize(&self, text: &str) -> AppResult<Vec<u32>> {
        Ok(split_for_streaming(text)
            .iter()
            .map(|chunk| hash64(chunk) as u32 % 100_000 + 1)
            .collect())
    }

    fn metrics(&self) -> EngineMetrics {
        let stats = lock(&self.stats);
        let tokens_per_second = if stats.total_generation_ms > 0 {
            stats.tokens_generated as f64 * 1000.0 / stats.total_generation_ms as f64
        } else {
            0.0
        };
        EngineMetrics {
            engine: self.name().to_string(),
            simulated: true,
            model_id: self.loaded.as_ref().map(|handle| handle.model_id.clone()),
            requests: stats.requests,
            tokens_generated: stats.tokens_generated,
            total_generation_ms: stats.total_generation_ms,
            tokens_per_second,
            loaded: self.loaded.is_some(),
            backend: self.options.backend,
        }
    }
}

/// Stream state. Stats are recorded when the terminal token is produced, so
/// `metrics()` stays honest even if a consumer abandons the stream.
struct MockStream {
    chunks: Vec<String>,
    index: usize,
    cancel: CancelToken,
    delay: Duration,
    started: Instant,
    prompt_tokens: u32,
    stats: Arc<Mutex<Stats>>,
    finished: bool,
    reason: &'static str,
}

impl MockStream {
    fn seal(&mut self, reason: &str) -> AppResult<GenToken> {
        self.finished = true;
        let completion_tokens = self.index as u32;
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        {
            let mut stats = lock(&self.stats);
            stats.tokens_generated += u64::from(completion_tokens);
            stats.total_generation_ms += elapsed_ms;
        }
        Ok(GenToken::finish(
            reason,
            TokenUsage::new(self.prompt_tokens, completion_tokens),
        ))
    }
}

fn lock(mutex: &Arc<Mutex<Stats>>) -> MutexGuard<'_, Stats> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Split text into whitespace-terminated pieces that concatenate back exactly.
fn split_for_streaming(text: &str) -> Vec<String> {
    text.split_inclusive(char::is_whitespace)
        .filter(|piece| !piece.is_empty())
        .map(str::to_string)
        .collect()
}

/// Deterministic 64-bit hash, so the same prompt always gives the same answer.
fn hash64(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// llama.cpp loads a small GGUF in well under a second on a modern laptop; the
/// mock reports a plausible, stable number instead of really sleeping.
fn simulated_load_ms(model_id: &str) -> u64 {
    180 + hash64(model_id) % 1400
}

/// Trim a reply at the first stop sequence, the way a real sampler would.
fn apply_stop_sequences(text: &str, stop: &[String]) -> String {
    let mut out = text.to_string();
    for needle in stop {
        if needle.is_empty() {
            continue;
        }
        if let Some(index) = out.find(needle.as_str()) {
            out.truncate(index);
        }
    }
    out.trim_end().to_string()
}

/// Fill gaps in GGUF-derived metadata with catalog values; GGUF wins.
fn merge_into(target: &mut ModelMetadata, from_file: &ModelMetadata) {
    if target.name.is_none() {
        target.name = from_file.name.clone();
    }
    if target.architecture.is_none() {
        target.architecture = from_file.architecture.clone();
    }
    if target.quantization.is_none() {
        target.quantization = from_file.quantization.clone();
    }
    if target.parameter_count.is_none() {
        target.parameter_count = from_file.parameter_count;
    }
    if target.parameters_b.is_none() {
        target.parameters_b = from_file.parameters_b;
    }
    if target.context_length.is_none() {
        target.context_length = from_file.context_length;
    }
    if target.train_type.is_none() {
        target.train_type = from_file.train_type.clone();
    }
    if target.license.is_none() {
        target.license = from_file.license.clone();
    }
    if target.vocab_size.is_none() {
        target.vocab_size = from_file.vocab_size;
    }
    if target.block_count.is_none() {
        target.block_count = from_file.block_count;
    }
    if target.embedding_length.is_none() {
        target.embedding_length = from_file.embedding_length;
    }
    if target.n_tensors == 0 {
        target.n_tensors = from_file.n_tensors;
    }
    if target.gguf_version == 0 {
        target.gguf_version = from_file.gguf_version;
    }
}

/// The five shapes below are deterministic, markdown-shaped and always admit
/// that nothing was inferred. One shape carries a fenced Rust block so the
/// highlighting path stays exercised in demo mode.
fn compose_reply(prompt: &str, seed: Option<i64>) -> String {
    let topic = last_user_line(prompt);
    let index = (hash64(&format!("{topic}|{}", seed.unwrap_or(0))) % 5) as usize;
    let body = match index {
        0 => format!("Here is a compact take on {topic}.\n\n- Start from the constraint that matters most.\n- Measure before optimising.\n- Keep any change small enough to review."),
        1 => format!("A tiny runnable shape for {topic}:\n\n```rust\nfn main() {{\n    let note = \"nothing was inferred here\";\n    println!(\"{{note}}\");\n}}\n```\n\nSwap the body for real logic once a GGUF backend is enabled."),
        2 => format!("Three options for {topic}, fastest first:\n\n1. Gather one number before changing anything.\n2. Try the smallest model that fits in free RAM.\n3. Only then move to a 3B model and re-measure."),
        3 => format!("Short answer about {topic}: keep it local, keep it small, and keep the prompt inside the context window you configured."),
        _ => format!("Reasoning about {topic} in steps:\n\n1. Identify the input.\n2. Bound the memory it needs.\n3. Pick a quantisation that fits with headroom.\n4. Run it and compare against step 2."),
    };
    let shape = index + 1;
    match seed {
        Some(seed) => format!(
            "{body}\n\n{} (shape {shape}, seed {seed})",
            MockEngine::DISCLAIMER
        ),
        None => format!("{body}\n\n{} (shape {shape})", MockEngine::DISCLAIMER),
    }
}

/// Quote the real question back, with chat-template markers stripped.
fn last_user_line(prompt: &str) -> String {
    let cleaned = strip_markers(prompt);
    let line = cleaned
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let clipped: String = line.chars().take(120).collect();
    if clipped.is_empty() {
        "your question".to_string()
    } else {
        clipped
    }
}

/// ChatML-style control tokens, plus the llama.cpp template suffix, replaced
/// with newlines so the mock can quote the actual question back.
const TEMPLATE_MARKERS: &[&str] = &[
    "<|system|>",
    "<|user|>",
    "<|assistant|>",
    "<|end|>",
    "im_start",
    "im_end",
    "### Instruction:",
    "### Response:",
];

/// Replace ChatML-style control tokens with newlines.
fn strip_markers(prompt: &str) -> String {
    let mut out = prompt.to_string();
    for marker in TEMPLATE_MARKERS {
        out = out.replace(marker, "\n");
    }
    // A ChatML token is `<|name|>`: the split above can leave the bare pipes.
    out.replace("<|", "\n").replace("|>", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use smollm_core::chat::SamplingParams;

    fn request(prompt: &str) -> GenerationRequest {
        GenerationRequest {
            request_id: "req-1".to_string(),
            prompt: prompt.to_string(),
            params: SamplingParams {
                max_tokens: 512,
                ..SamplingParams::default()
            },
            stop: Vec::new(),
            cancel: CancelToken::new(),
        }
    }

    fn loaded_engine(config: MockConfig) -> MockEngine {
        let mut engine = MockEngine::with_config(config);
        engine
            .load_model(LoadModelRequest {
                model_id: "qwen2.5-0.5b-instruct-gguf".to_string(),
                display_name: "Qwen2.5 0.5B Instruct".to_string(),
                ..LoadModelRequest::default()
            })
            .expect("mock always loads");
        engine
    }

    async fn collect(stream: TokenStream) -> Vec<GenToken> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    #[tokio::test]
    async fn streams_the_whole_reply_then_a_finish_token() {
        let mut engine = loaded_engine(MockConfig::instant());
        let chunks = engine
            .generate(request("user: explain Rust ownership"))
            .expect("generates");
        let tokens = collect(chunks).await;

        assert!(
            tokens.len() > 8,
            "expected many chunks, got {}",
            tokens.len()
        );
        let last = tokens.last().expect("finish token");
        assert_eq!(last.finish_reason.as_deref(), Some("eos"));
        let usage = last.usage.expect("usage");
        assert!(usage.prompt_tokens > 0);
        assert_eq!(usage.completion_tokens, (tokens.len() - 1) as u32);
        assert_eq!(
            usage.total_tokens,
            usage.prompt_tokens + usage.completion_tokens
        );

        let text: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(text.contains("ownership"), "got {text}");
        assert!(
            text.contains(MockEngine::DISCLAIMER),
            "must admit it is simulated"
        );
        assert!(
            text.starts_with("Here is a compact take") || text.contains('\n'),
            "replies are markdown-shaped: {text}"
        );
    }

    async fn answer(engine: &mut MockEngine, req: GenerationRequest) -> String {
        collect(engine.generate(req).expect("generates"))
            .await
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    #[tokio::test]
    async fn same_prompt_gives_the_same_answer() {
        let mut engine = loaded_engine(MockConfig::instant());
        let first = answer(&mut engine, request("user: what is quantisation")).await;
        let second = answer(&mut engine, request("user: what is quantisation")).await;
        assert_eq!(first, second, "mock answers must be deterministic");
    }

    #[tokio::test]
    async fn cancellation_stops_the_stream() {
        let mut engine = loaded_engine(MockConfig { chunk_delay_ms: 40 });
        let cancel = CancelToken::new();
        let req = GenerationRequest {
            request_id: "req-2".to_string(),
            prompt: "user: write something long".to_string(),
            params: SamplingParams {
                max_tokens: 512,
                ..SamplingParams::default()
            },
            stop: Vec::new(),
            cancel: cancel.clone(),
        };
        let mut stream = engine.generate(req).expect("generates");
        let first = stream.next().await.expect("first token").expect("ok");
        assert!(!first.text.is_empty());

        cancel.cancel();
        let rest = stream.collect::<Vec<_>>().await;
        let tokens: Vec<GenToken> = rest.into_iter().filter_map(Result::ok).collect();
        let last = tokens.last().expect("terminal token");
        assert_eq!(last.finish_reason.as_deref(), Some("stop"));
        assert_eq!(last.usage.expect("usage").completion_tokens, 1);
    }

    #[tokio::test]
    async fn max_tokens_bounds_the_reply() {
        let mut engine = loaded_engine(MockConfig::instant());
        let req = GenerationRequest {
            request_id: "req-3".to_string(),
            prompt: "user: tell me more".to_string(),
            params: SamplingParams {
                max_tokens: 4,
                ..SamplingParams::default()
            },
            stop: Vec::new(),
            cancel: CancelToken::new(),
        };
        let tokens = collect(engine.generate(req).expect("generates")).await;
        assert_eq!(tokens.len(), 5, "4 chunks plus the terminal token");
        let last = tokens.last().expect("terminal token");
        assert_eq!(last.finish_reason.as_deref(), Some("length"));
        assert_eq!(last.usage.expect("usage").completion_tokens, 4);
    }

    #[tokio::test]
    async fn stop_sequences_truncate_the_text() {
        let mut engine = loaded_engine(MockConfig::instant());
        let req = GenerationRequest {
            request_id: "req-4".to_string(),
            prompt: "user: hello".to_string(),
            params: SamplingParams::default(),
            stop: vec!["\n\n".to_string()],
            cancel: CancelToken::new(),
        };
        let text = answer(&mut engine, req).await;
        assert!(!text.contains("\n\n"), "stop sequence leaked: {text:?}");
    }

    #[test]
    fn tokenization_is_stable_and_bounded() {
        let engine = loaded_engine(MockConfig::instant());
        let a = engine.tokenize("hello local world").expect("tokenizes");
        let b = engine.tokenize("hello local world").expect("tokenizes");
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert!(a.iter().all(|id| *id > 0 && *id <= 100_000));
        assert!(engine.tokenize("").expect("empty").is_empty());
    }

    #[test]
    fn load_clamps_context_to_the_model_and_reports_metrics() {
        let mut engine = MockEngine::with_config(MockConfig::instant());
        let handle = engine
            .load_model(LoadModelRequest {
                model_id: "llama-3.2-1b-gguf".to_string(),
                options: LoadModelOptions {
                    context_length: 8192,
                    backend: smollm_core::system::Backend::Metal,
                    ..LoadModelOptions::default()
                },
                metadata: Some(ModelMetadata {
                    context_length: Some(2048),
                    name: Some("Llama 3.2 1B".to_string()),
                    ..Default::default()
                }),
                ..LoadModelRequest::default()
            })
            .expect("loads");
        assert_eq!(handle.context_length, 2048);
        assert_eq!(handle.display_name, "Llama 3.2 1B");
        assert_eq!(handle.engine, "mock");

        let metrics = engine.metrics();
        assert!(metrics.loaded);
        assert!(metrics.simulated);
        assert_eq!(metrics.backend, smollm_core::system::Backend::Metal);
        assert_eq!(metrics.model_id.as_deref(), Some("llama-3.2-1b-gguf"));

        engine.unload_model(handle).expect("unloads");
        assert!(!engine.metrics().loaded);
    }

    #[tokio::test]
    async fn generating_without_a_model_is_an_error() {
        let mut engine = MockEngine::default();
        let error = engine
            .generate(request("user: hi"))
            .err()
            .expect("needs a loaded model");
        assert!(matches!(error, AppError::ModelNotFound(_)));
    }

    #[tokio::test]
    async fn metrics_accumulate_across_requests() {
        let mut engine = loaded_engine(MockConfig::instant());
        for _ in 0..2 {
            let tokens = collect(
                engine
                    .generate(request("user: count me"))
                    .expect("generates"),
            )
            .await;
            assert!(!tokens.is_empty());
        }
        let metrics = engine.metrics();
        assert_eq!(metrics.requests, 2);
        assert!(metrics.tokens_generated > 0);
        assert!(metrics.total_generation_ms < 60_000);
    }

    #[test]
    fn simulated_load_time_is_stable() {
        assert_eq!(
            simulated_load_ms("qwen2.5-0.5b-instruct-gguf"),
            simulated_load_ms("qwen2.5-0.5b-instruct-gguf")
        );
        let value = simulated_load_ms("anything");
        assert!((180..1580).contains(&value), "got {value}");
    }

    #[test]
    fn helpers_trim_quote_and_split() {
        assert_eq!(split_for_streaming("a b"), vec!["a ", "b"]);
        assert_eq!(
            apply_stop_sequences("hello world", &["world".to_string()]),
            "hello"
        );
        assert_eq!(apply_stop_sequences("abc", &["".to_string()]), "abc");
        assert_eq!(strip_markers("<|user|>hi<|end|>"), "\nhi\n");
        assert!(!strip_markers("plain <|weird|> text").contains("<|"));
        assert_eq!(last_user_line(""), "your question");
        assert_eq!(
            last_user_line("<|user|>\nkeep it short\n<|end|>"),
            "keep it short"
        );
    }
}
