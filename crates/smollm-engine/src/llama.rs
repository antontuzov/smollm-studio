//! llama.cpp engine: real GGUF inference, behind the `llama-cpp` cargo feature.
//!
//! Enabling the feature links llama.cpp through the `llama-cpp-2` bindings,
//! which vendor and build the library at compile time. That needs a native
//! toolchain (cmake plus a C/C++ compiler), which is why it stays opt-in and
//! MockEngine remains the default: see `docs/models.md` for the build matrix
//! and `docs/hardware.md` for how a requested backend resolves to an engine.
//!
//! Decoding blocks llama.cpp's own call stack for as long as a reply takes, so
//! every generation runs on a dedicated worker thread and hands tokens to the
//! async layers through a channel. Nothing in here may run on the Tauri thread.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::{BatchAddError, LlamaBatch};
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::vocab::LlamaVocab;
use llama_cpp_2::{
    list_llama_ggml_backend_devices, send_logs_to_tracing, DecodeError, LlamaBackendDevice,
    LlamaBackendDeviceType, LogOptions,
};
use smollm_core::chat::{
    ChatMessage, EngineMetrics, GenToken, GenerationRequest, LoadModelOptions, LoadModelRequest,
    ModelHandle, Role, SamplingParams, TokenUsage,
};
use smollm_core::gguf::GgufHeader;
use smollm_core::model::ModelMetadata;
use smollm_core::system::{Accelerator, Backend};
use smollm_core::{AppError, AppResult};

use crate::gguf_meta::{merge_metadata, pick_context};
use crate::{Engine, TokenStream};

/// Tokens per `llama_decode` call while prefilling the prompt.
const PREFILL_BATCH: u32 = 512;
/// llama.cpp's "use all the history you have" sentinel for the penalty window.
const PENALTY_HISTORY: i32 = -1;

/// Process-wide llama.cpp runtime: the library rejects a second initialisation.
static BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();
/// llama.cpp forbids two contexts on one model at the same time, so generations
/// queue for this lock rather than corrupting each other.
static DECODE_GATE: Mutex<()> = Mutex::new(());
/// Serialises the one-time `llama_backend_init` call.
static INIT_GATE: Mutex<()> = Mutex::new(());

/// How a generation ended, which decides the terminal `finish_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// End-of-generation token, a stop sequence, or the user pressed stop.
    Stop,
    /// `max_tokens` or the context window ran out first.
    Length,
}

impl Finish {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
        }
    }
}

/// A model in memory, plus the handles a worker thread needs to decode with it.
struct Loaded {
    handle: ModelHandle,
    backend: Arc<LlamaBackend>,
    /// The running generation holds its own strong count, so dropping this one
    /// at unload can never free a model llama.cpp is still reading.
    model: Arc<LlamaModel>,
    /// The GGUF file's chat template, when it has one.
    template: Option<LlamaChatTemplate>,
}

#[derive(Debug, Default)]
struct Stats {
    requests: u64,
    tokens_generated: u64,
    total_generation_ms: u64,
    last_tokens_per_second: f64,
}

/// Native inference on the linked llama.cpp library.
#[derive(Default)]
pub struct LlamaCppEngine {
    loaded: Option<Loaded>,
    options: LoadModelOptions,
    stats: Arc<Mutex<Stats>>,
    /// Compute device the last load actually got, cleared on unload.
    device: Option<Backend>,
}

impl LlamaCppEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// True: unlike the other adapters, the `llama-cpp` feature links the real
    /// library, so an engine built from it can answer a generation request.
    pub fn is_linked() -> bool {
        true
    }

    /// Layers llama.cpp should move to the GPU for this load.
    ///
    /// `gpu_layers` counts layers and `-1` means "all of them". Offloading any
    /// needs a GPU backend that is both compiled into llama.cpp and present on
    /// this machine, which is asked of the library here rather than assumed from
    /// the settings; `docs/hardware.md` explains how a request degrades to CPU.
    pub fn offloaded_layers(&self, options: &LoadModelOptions) -> i32 {
        let gpu_ready = backend()
            .map(|backend| backend.supports_gpu_offload())
            .unwrap_or(false);
        if !gpu_ready {
            return 0;
        }
        if options.gpu_layers < 0 {
            i32::MAX
        } else {
            options.gpu_layers
        }
    }
}

impl Engine for LlamaCppEngine {
    fn name(&self) -> &str {
        "llama-cpp"
    }

    fn supports_gguf(&self) -> bool {
        true
    }

    fn is_simulated(&self) -> bool {
        false
    }

    fn load_model(&mut self, request: LoadModelRequest) -> AppResult<ModelHandle> {
        if request.path.as_os_str().is_empty() || !request.path.exists() {
            let location = if request.path.as_os_str().is_empty() {
                "no local file has been chosen".to_string()
            } else {
                request.path.display().to_string()
            };
            return Err(AppError::ModelNotDownloaded(format!(
                "{} ({location})",
                request.model_id
            )));
        }

        let backend = backend()?;
        let options = request.options.clone();
        let gpu_layers = self.offloaded_layers(&options);
        let params =
            LlamaModelParams::default().with_n_gpu_layers(u32::try_from(gpu_layers).unwrap_or(0));
        let model = LlamaModel::load_from_file(backend.as_ref(), &request.path, &params).map_err(
            |source| {
                AppError::EngineLoadFailed(format!(
                    "llama.cpp could not load {}: {source}",
                    request.path.display()
                ))
            },
        )?;
        let model = Arc::new(model);
        let template = model.chat_template(None).ok();
        let chat_template = template.is_some();

        let metadata = header_metadata(&request);
        // What the file advertises and what the model was trained for: the
        // smaller of the two is the only context llama.cpp will accept.
        let trained = model.n_ctx_train();
        let ceiling = metadata
            .context_length
            .filter(|value| *value > 0)
            .map_or(trained, |from_file| from_file.min(trained.max(1)));
        let context_length = pick_context((ceiling > 0).then_some(ceiling), &options);

        let display_name = if request.display_name.is_empty() {
            metadata
                .name
                .clone()
                .unwrap_or_else(|| request.model_id.clone())
        } else {
            request.display_name.clone()
        };
        let handle = ModelHandle {
            id: uuid::Uuid::new_v4().simple().to_string(),
            model_id: request.model_id.clone(),
            display_name,
            path: request.path.display().to_string(),
            engine: self.name().to_string(),
            context_length,
            metadata,
        };

        self.options = options;
        self.loaded = Some(Loaded {
            handle: handle.clone(),
            backend,
            model,
            template,
        });
        let device = accelerated_device(gpu_layers);
        if gpu_layers > 0 && device.is_none() {
            tracing::warn!(
                gpu_layers,
                "llama.cpp found no accelerator to offload to, so every layer stays on the CPU"
            );
        }
        self.device = device.as_ref().map(|device| {
            let mapped = Backend::from_ggml_backend(&device.backend);
            if mapped == Backend::Cpu {
                tracing::warn!(
                    ggml_backend = %device.backend,
                    "llama.cpp offloaded layers to a backend this app does not model, so metrics report cpu"
                );
            }
            mapped
        });
        let device_label = device.as_ref().map_or_else(
            || "cpu".to_string(),
            |device| format!("{} ({})", device.description, device.backend),
        );
        tracing::info!(
            model_id = %handle.model_id,
            context = handle.context_length,
            gpu_layers,
            chat_template,
            device = %device_label,
            "llama.cpp loaded a model"
        );
        Ok(handle)
    }

    fn unload_model(&mut self, handle: ModelHandle) -> AppResult<()> {
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.handle.id == handle.id)
        {
            // A generation still decoding keeps the weights alive through its
            // own handle and frees them when it finishes, so this never waits.
            self.loaded = None;
            self.device = None;
        }
        Ok(())
    }

    fn generate(&mut self, request: GenerationRequest) -> AppResult<TokenStream> {
        let Some(loaded) = self.loaded.as_ref() else {
            return Err(AppError::ModelNotFound(
                "load a model with the llama-cpp engine before generating".to_string(),
            ));
        };
        if request.prompt.is_empty() {
            return Err(AppError::InvalidRequest(
                "llama-cpp was handed an empty prompt".to_string(),
            ));
        }

        let (sender, receiver) = unbounded();
        let worker = Worker {
            backend: Arc::clone(&loaded.backend),
            model: Arc::clone(&loaded.model),
            options: self.options.clone(),
            context_length: loaded.handle.context_length,
            stats: Arc::clone(&self.stats),
        };
        // Detached on purpose: the thread owns everything it needs and stops as
        // soon as llama.cpp stops or the stream's consumer disappears.
        thread::spawn(move || worker.run(request, sender));

        Ok(Box::pin(receiver))
    }

    fn tokenize(&self, text: &str) -> AppResult<Vec<u32>> {
        let Some(loaded) = self.loaded.as_ref() else {
            return Err(AppError::ModelNotFound(
                "tokenisation needs a loaded llama.cpp model".to_string(),
            ));
        };
        Ok(loaded
            .model
            .vocab()
            .tokenize(text.as_bytes(), false, true)
            .into_iter()
            .map(|token| u32::try_from(token.0).unwrap_or(0))
            .collect())
    }

    /// Use the model's own chat template when its file carries one, which beats
    /// the app's hand-rolled markup for any model llama.cpp knows about.
    fn render_chat_prompt(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
    ) -> Option<String> {
        let loaded = self.loaded.as_ref()?;
        let template = loaded.template.as_ref()?;
        let mut chat = Vec::with_capacity(messages.len() + 1);
        if let Some(system) = system_prompt.filter(|text| !text.trim().is_empty()) {
            chat.push(llama_message(Role::System, system.to_string())?);
        }
        for message in messages {
            chat.push(llama_message(message.role, message.content.clone())?);
        }
        loaded.model.apply_chat_template(template, &chat, true).ok()
    }

    fn metrics(&self) -> EngineMetrics {
        let stats = lock(&self.stats);
        EngineMetrics {
            engine: self.name().to_string(),
            simulated: false,
            model_id: self
                .loaded
                .as_ref()
                .map(|loaded| loaded.handle.model_id.clone()),
            requests: stats.requests,
            tokens_generated: stats.tokens_generated,
            total_generation_ms: stats.total_generation_ms,
            tokens_per_second: stats.last_tokens_per_second,
            loaded: self.loaded.is_some(),
            // What llama.cpp really ran on, not what the settings asked for.
            backend: self.device.unwrap_or(Backend::Cpu),
        }
    }
}

/// Everything a decode thread needs, owned so it can outlive the borrow that
/// started it.
struct Worker {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    options: LoadModelOptions,
    context_length: u32,
    stats: Arc<Mutex<Stats>>,
}

impl Worker {
    fn run(self, request: GenerationRequest, sender: UnboundedSender<AppResult<GenToken>>) {
        // llama.cpp forbids two contexts on one model at a time, so decoders
        // queue for the gate instead of corrupting each other.
        let _gate = gate();
        let outcome = self.decode(&request, &sender);
        match outcome {
            Ok(generation) => {
                let mut stats = lock(&self.stats);
                stats.requests += 1;
                stats.tokens_generated += generation.tokens;
                stats.total_generation_ms +=
                    u64::try_from(generation.elapsed.as_millis()).unwrap_or(u64::MAX);
                stats.last_tokens_per_second = per_second(generation.tokens, generation.elapsed);
                tracing::debug!(
                    tokens = generation.tokens,
                    ms = stats.total_generation_ms,
                    per_second = stats.last_tokens_per_second,
                    reason = generation.finish.as_str(),
                    "llama.cpp generation finished"
                );
            }
            Err(error) => {
                tracing::warn!(%error, "llama.cpp generation failed");
                let _ = sender.unbounded_send(Err(error));
            }
        }
    }

    fn decode(
        &self,
        request: &GenerationRequest,
        sender: &UnboundedSender<AppResult<GenToken>>,
    ) -> AppResult<Generation> {
        let vocab = self.model.vocab();
        let mut context_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.context_length.max(1)))
            .with_n_batch(PREFILL_BATCH);
        if let Some(threads) = self.options.threads.filter(|count| *count > 0) {
            let threads = i32::try_from(threads).unwrap_or(1);
            context_params = context_params
                .with_n_threads(threads)
                .with_n_threads_batch(threads);
        }
        let mut context = self
            .model
            .new_context(self.backend.as_ref(), context_params)
            .map_err(|source| {
                AppError::EngineLoadFailed(format!("llama.cpp could not build a context: {source}"))
            })?;

        let batch_size = usize::try_from(context.n_batch()).unwrap_or(1).max(1);
        let capacity = usize::try_from(context.n_ctx()).unwrap_or(0);
        let mut batch = LlamaBatch::new(batch_size, 1);
        let mut sampler = sampler_for(self.model.n_vocab(), &request.params);
        let prompt = vocab.tokenize(request.prompt.as_bytes(), true, true);
        if prompt.is_empty() {
            return Err(AppError::GenerationFailed(
                "the prompt tokenised to nothing".to_string(),
            ));
        }
        // One slot has to stay free for the token sampled from the prompt.
        if prompt.len() + 1 > capacity {
            return Err(AppError::InvalidRequest(format!(
                "this prompt needs {} tokens and the context holds {capacity}; shorten the \
                 conversation or lower the context length",
                prompt.len()
            )));
        }

        let started = Instant::now();
        let prompt_tokens = u32::try_from(prompt.len()).unwrap_or(u32::MAX);
        let mut position = 0i32;
        let mut cursor = 0usize;
        // llama.cpp hands back logits by batch row, so the sampler has to ask
        // for the row that asked for them: the last prompt token while
        // prefilling, and row 0 once the loop feeds a single token at a time.
        let mut logits_row = 0i32;
        while cursor < prompt.len() {
            let end = (cursor + batch_size).min(prompt.len());
            let chunk = &prompt[cursor..end];
            let last = chunk.len() - 1;
            batch.clear();
            for (offset, token) in chunk.iter().enumerate() {
                let position = position + i32::try_from(offset).unwrap_or(0);
                batch
                    .add(*token, position, &[0], offset == last)
                    .map_err(batch_error)?;
            }
            context.decode(&mut batch).map_err(decode_error)?;
            logits_row = i32::try_from(last).unwrap_or(0);
            position += i32::try_from(chunk.len()).unwrap_or(0);
            cursor = end;
        }

        let mut emitter = Emitter::default();
        let max_tokens = request.params.max_tokens.max(1);
        let mut emitted = 0u32;
        let mut stopped = false;
        let finish = loop {
            if request.cancel.is_cancelled() {
                break Finish::Stop;
            }
            let token = sampler.sample(&context, logits_row);
            logits_row = 0;
            if vocab.is_eog(token) {
                break Finish::Stop;
            }
            let piece = emitter.push(&piece_of(&vocab, token), &request.stop);
            if !piece.text.is_empty() && !send(sender, GenToken::token(piece.text)) {
                break Finish::Stop;
            }
            emitted += 1;
            if piece.stopped {
                stopped = true;
                break Finish::Stop;
            }
            if emitted >= max_tokens || usize::try_from(position).unwrap_or(0) + 1 >= capacity {
                break Finish::Length;
            }
            // Feed the sampled token back so llama.cpp predicts the next one.
            batch.clear();
            batch
                .add(token, position, &[0], true)
                .map_err(batch_error)?;
            context.decode(&mut batch).map_err(decode_error)?;
            position += 1;
        };

        // A tail held back as a possible stop marker is only worth showing when
        // no marker actually matched.
        if !stopped {
            let tail = emitter.take_tail();
            if !tail.is_empty() {
                let _ = send(sender, GenToken::token(tail));
            }
        }

        let usage = TokenUsage::new(prompt_tokens, emitted);
        send(sender, GenToken::finish(finish.as_str(), usage));
        Ok(Generation {
            tokens: u64::from(emitted),
            finish,
            elapsed: started.elapsed(),
        })
    }
}

/// What one completed generation reports to the metrics panel.
struct Generation {
    tokens: u64,
    finish: Finish,
    elapsed: Duration,
}

/// Streams token text while holding back any tail that could still grow into a
/// stop sequence, so a marker is never half-shown in the chat.
#[derive(Default)]
struct Emitter {
    pending: String,
}

/// What a token piece turned into once stop sequences were applied.
struct Pushed {
    text: String,
    /// A stop sequence matched, so the reply ends here.
    stopped: bool,
}

impl Emitter {
    fn push(&mut self, piece: &str, stops: &[String]) -> Pushed {
        self.pending.push_str(piece);
        if let Some(hit) = stops
            .iter()
            .filter(|stop| !stop.is_empty())
            .filter_map(|stop| self.pending.find(stop))
            .min()
        {
            let text = self.pending[..hit].trim_end().to_string();
            self.pending.clear();
            return Pushed {
                text,
                stopped: true,
            };
        }
        let held = ambiguous_tail(&self.pending, stops);
        let cut = self.pending.len() - held;
        let text = self.pending[..cut].to_string();
        // The emitted part leaves the buffer, or every token would repeat.
        self.pending.drain(..cut);
        Pushed {
            text,
            stopped: false,
        }
    }

    fn take_tail(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

/// Byte length of the longest suffix of `text` that is a proper prefix of one
/// of `stops`, which is the part too risky to show yet.
fn ambiguous_tail(text: &str, stops: &[String]) -> usize {
    let shown: Vec<char> = text.chars().collect();
    let mut longest = 0;
    for stop in stops {
        let marker: Vec<char> = stop.chars().collect();
        for take in 1..marker.len() {
            if take > shown.len() {
                break;
            }
            let tail = &shown[shown.len() - take..];
            if tail == &marker[..take] {
                let bytes: usize = tail.iter().copied().map(char::len_utf8).sum();
                longest = longest.max(bytes);
            }
        }
    }
    longest
}

/// Build the sampler chain llama.cpp runs for these parameters, in the order
/// the reference CLI uses: penalties, truncation, then the choosing sampler.
fn sampler_for(n_vocab: i32, params: &SamplingParams) -> LlamaSampler {
    LlamaSampler::chain_simple(sampler_steps(n_vocab, params))
}

/// The samplers this request asks for, in execution order.
fn sampler_steps(n_vocab: i32, params: &SamplingParams) -> Vec<LlamaSampler> {
    let mut steps = Vec::new();
    if params.repeat_penalty != 1.0 || params.presence_penalty != 0.0 {
        steps.push(LlamaSampler::penalties(
            n_vocab,
            PENALTY_HISTORY,
            params.repeat_penalty.max(f32::EPSILON),
            params.presence_penalty,
            0.0,
        ));
    }
    if params.top_k > 0 {
        steps.push(LlamaSampler::top_k(
            i32::try_from(params.top_k).unwrap_or(i32::MAX),
        ));
    }
    if params.top_p < 1.0 {
        steps.push(LlamaSampler::top_p(params.top_p, 1));
    }
    if params.min_p > 0.0 {
        steps.push(LlamaSampler::min_p(params.min_p, 1));
    }
    // A zero temperature asks for the likeliest token, not a random one.
    if params.temperature <= 0.0 {
        steps.push(LlamaSampler::greedy());
        return steps;
    }
    steps.push(LlamaSampler::temp(params.temperature));
    steps.push(LlamaSampler::dist(seed_for(params.seed)));
    steps
}

/// llama.cpp wants a `u32` seed; "no seed" means a different one per request.
fn seed_for(seed: Option<i64>) -> u32 {
    if let Some(seed) = seed
        .filter(|value| *value >= 0)
        .and_then(|value| u32::try_from(value).ok())
    {
        return seed;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x9E37_79B9, |since| {
            since.subsec_nanos() ^ u32::try_from(since.as_secs()).unwrap_or(0)
        })
}

/// One token as displayable text, with control tokens asked not to render.
fn piece_of(vocab: &LlamaVocab<'_>, token: LlamaToken) -> String {
    String::from_utf8_lossy(&vocab.token_to_piece(token, false, None)).to_string()
}

fn llama_message(role: Role, content: String) -> Option<LlamaChatMessage> {
    let role = match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    LlamaChatMessage::new(role.to_string(), content).ok()
}

/// Header metadata, with catalog values filling the gaps the file leaves.
fn header_metadata(request: &LoadModelRequest) -> ModelMetadata {
    let mut metadata = GgufHeader::read(&request.path)
        .map(|header| header.summary())
        .unwrap_or_default();
    if let Some(catalog) = &request.metadata {
        merge_metadata(&mut metadata, catalog);
    }
    metadata
}

/// Push one token to the stream, reporting whether anyone is still listening.
fn send(sender: &UnboundedSender<AppResult<GenToken>>, token: GenToken) -> bool {
    sender.unbounded_send(Ok(token)).is_ok()
}

fn gate() -> MutexGuard<'static, ()> {
    DECODE_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock<T>(mutex: &Arc<Mutex<T>>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// llama.cpp can only initialise once per process, so the first caller wins and
/// everyone after it shares the same handle. A test harness runs loads on
/// several threads, hence the lock around the one-time initialisation.
fn backend() -> AppResult<Arc<LlamaBackend>> {
    if let Some(existing) = BACKEND.get() {
        return Ok(Arc::clone(existing));
    }
    let _guard = INIT_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = BACKEND.get() {
        return Ok(Arc::clone(existing));
    }
    // llama.cpp would otherwise write its scheduling and graph traces straight to
    // stderr, hundreds of lines per generation, past every `tracing` layer and
    // every log file. Redirected before `init` so even first-run device setup
    // lands in the app's own log; `RUST_LOG` then decides what is shown.
    send_logs_to_tracing(LogOptions::default());
    let created = LlamaBackend::init()
        .map(Arc::new)
        .map_err(|source| AppError::EngineLoadFailed(format!("llama.cpp: {source}")))?;
    Ok(Arc::clone(BACKEND.get_or_init(|| created)))
}

/// Accelerators llama.cpp reports, biggest memory budget first.
fn gpu_devices() -> Vec<LlamaBackendDevice> {
    list_llama_ggml_backend_devices()
        .into_iter()
        .filter(|device| {
            matches!(
                device.device_type,
                LlamaBackendDeviceType::Gpu
                    | LlamaBackendDeviceType::IntegratedGpu
                    | LlamaBackendDeviceType::Accelerator
            )
        })
        .collect()
}

/// The accelerator llama.cpp is actually running on, when layers are offloaded.
///
/// Asked of the loaded library rather than read from the settings: a CPU-only
/// build, or a machine whose GPU llama.cpp cannot open, reports no accelerator,
/// and then the app says `cpu` instead of promising Metal.
fn accelerated_device(gpu_layers: i32) -> Option<LlamaBackendDevice> {
    if gpu_layers <= 0 {
        return None;
    }
    // With two GPUs the one with the most room for weights gets the model.
    gpu_devices()
        .into_iter()
        .max_by_key(|device| device.memory_total)
}

/// The device llama.cpp would put weights on, with the memory it allows itself.
///
/// Worth asking before promising GPU layers: even on unified Apple Silicon the
/// budget llama.cpp takes from Metal (`recommendedMaxWorkingSetSize`) is
/// noticeably smaller than the machine's RAM. Initialises llama.cpp on first
/// call, so callers on a UI thread must run this off the main thread.
#[must_use]
pub fn largest_accelerator() -> Option<Accelerator> {
    let backend = backend().ok()?;
    if !backend.supports_gpu_offload() {
        return None;
    }
    let device = accelerated_device(1)?;
    let gib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    Some(Accelerator {
        name: device.name,
        description: device.description,
        backend: device.backend,
        usable_memory_gb: gib(device.memory_total),
        free_memory_gb: gib(device.memory_free),
    })
}

fn decode_error(source: DecodeError) -> AppError {
    AppError::GenerationFailed(format!("llama.cpp could not decode a batch: {source}"))
}

fn batch_error(source: BatchAddError) -> AppError {
    AppError::GenerationFailed(format!("llama.cpp rejected the batch: {source}"))
}

fn per_second(tokens: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        tokens as f64 / seconds
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use smollm_core::chat::{chat_message, CancelToken, SamplingParams};

    fn markers(seqs: &[&str]) -> Vec<String> {
        seqs.iter().map(|stop| stop.to_string()).collect()
    }

    #[test]
    fn a_linked_adapter_calls_itself_runnable() {
        assert!(LlamaCppEngine::is_linked());
        let engine = LlamaCppEngine::new();
        assert_eq!(engine.name(), "llama-cpp");
        assert!(engine.supports_gguf());
        assert!(!engine.is_simulated());
        assert!(!engine.metrics().loaded);
        assert_eq!(engine.metrics().requests, 0);
    }

    #[test]
    fn an_engine_without_a_model_refuses_to_infer() {
        let mut engine = LlamaCppEngine::new();
        let generation = GenerationRequest {
            request_id: "r".to_string(),
            prompt: "user: hi".to_string(),
            params: SamplingParams::default(),
            stop: Vec::new(),
            cancel: CancelToken::new(),
        };
        assert!(matches!(
            engine.generate(generation),
            Err(AppError::ModelNotFound(_))
        ));
        assert!(matches!(
            engine.tokenize("hi"),
            Err(AppError::ModelNotFound(_))
        ));
        // No model means no template, so the manager's family table stays in charge.
        assert!(engine
            .render_chat_prompt(None, &[chat_message(Role::User, "hi")])
            .is_none());
    }

    #[test]
    fn loading_needs_a_file_that_is_actually_local() {
        let mut engine = LlamaCppEngine::new();
        let error = engine
            .load_model(LoadModelRequest::default())
            .expect_err("there is nothing to read");
        assert!(matches!(error, AppError::ModelNotDownloaded(_)));
        assert!(!engine.metrics().loaded);
    }

    #[test]
    fn unloading_an_unknown_handle_harmlessly_does_nothing() {
        let mut engine = LlamaCppEngine::new();
        let handle = ModelHandle {
            id: "other".to_string(),
            model_id: "other".to_string(),
            display_name: "Other".to_string(),
            path: String::new(),
            engine: "llama-cpp".to_string(),
            context_length: 512,
            metadata: ModelMetadata::default(),
        };
        engine
            .unload_model(handle)
            .expect("unloading is idempotent");
        assert!(!engine.metrics().loaded);
    }

    #[test]
    fn metadata_falls_back_to_the_catalog_when_the_header_cannot_be_read() {
        let request = LoadModelRequest {
            path: PathBuf::from("/definitely/not/here/model.gguf"),
            metadata: Some(ModelMetadata {
                name: Some("SmolLM2 360M Instruct".to_string()),
                context_length: Some(32_768),
                ..ModelMetadata::default()
            }),
            ..LoadModelRequest::default()
        };
        let metadata = header_metadata(&request);
        assert_eq!(metadata.name.as_deref(), Some("SmolLM2 360M Instruct"));
        assert_eq!(metadata.context_length, Some(32_768));
    }

    #[test]
    fn a_stop_marker_ends_the_reply_and_its_tail_is_flushed_when_unused() {
        let stops = markers(&["END", "X"]);
        let mut emitter = Emitter::default();
        assert_eq!(emitter.push("hello", &stops).text, "hello");
        let cut = emitter.push(" world END never shown", &stops);
        assert!(cut.stopped);
        assert_eq!(cut.text, " world");
        assert!(emitter.take_tail().is_empty(), "a matched marker is gone");

        let mut held = Emitter::default();
        // "E" could still become "END", so it waits rather than leaking markup.
        assert_eq!(held.push("done E", &stops).text, "done ");
        assert_eq!(held.take_tail(), "E");
    }

    #[test]
    fn layer_counts_never_offload_more_than_the_device_allows() {
        let engine = LlamaCppEngine::new();
        let count = |gpu_layers| {
            engine.offloaded_layers(&LoadModelOptions {
                gpu_layers,
                ..LoadModelOptions::default()
            })
        };
        assert_eq!(count(0), 0, "a zero request is a zero request");
        assert!(count(-1) >= count(8));
        assert_eq!(
            count(-1) > 0,
            count(8) > 0,
            "one build gives one answer about the GPU"
        );
    }

    #[test]
    fn the_largest_accelerator_llama_cpp_offers_is_the_one_chosen() {
        let engine = LlamaCppEngine::new();
        assert_eq!(
            engine.metrics().backend,
            Backend::Cpu,
            "an engine with no model loaded runs nowhere"
        );
        // llama.cpp only answers device questions once it is initialised.
        let _ = backend().expect("the library initialises for its own device list");
        assert!(
            accelerated_device(0).is_none(),
            "no offload was asked for, so no device is claimed"
        );
        let offered = list_llama_ggml_backend_devices()
            .into_iter()
            .filter(|device| {
                !matches!(
                    device.device_type,
                    LlamaBackendDeviceType::Cpu | LlamaBackendDeviceType::Unknown
                )
            })
            .max_by_key(|device| device.memory_total)
            .map(|device| device.index);
        assert_eq!(
            accelerated_device(1).map(|device| device.index),
            offered,
            "the engine must report the device llama.cpp actually has"
        );

        // The standalone probe the hardware panel uses must agree with the
        // engine's own choice, or the app promises a device it will not use.
        if let Some(probe) = largest_accelerator() {
            let chosen = accelerated_device(1).expect("a device was named, so one exists");
            assert_eq!(probe.name, chosen.name);
            assert!(
                probe.usable_memory_gb > 0.0,
                "{probe:?} reports no memory budget"
            );
            assert!(
                !probe.description.is_empty() && !probe.backend.is_empty(),
                "{probe:?} must name the device and its backend"
            );
        } else {
            assert!(
                offered.is_none() || !backend().expect("initialised").supports_gpu_offload(),
                "llama.cpp offered a device but the probe reported none"
            );
        }
    }

    #[test]
    fn sampler_steps_follow_the_parameters_they_are_given() {
        // penalties, top-k, top-p, min-p, temp, dist
        assert_eq!(sampler_steps(100, &SamplingParams::default()).len(), 6);
        let greedy = SamplingParams {
            temperature: 0.0,
            ..SamplingParams::default()
        };
        assert_eq!(
            sampler_steps(100, &greedy).len(),
            5,
            "greedy replaces temp and dist"
        );
        let bare = SamplingParams {
            temperature: 0.8,
            top_p: 1.0,
            top_k: 0,
            min_p: 0.0,
            repeat_penalty: 1.0,
            presence_penalty: 0.0,
            ..SamplingParams::default()
        };
        assert_eq!(sampler_steps(100, &bare).len(), 2, "temp then dist");
    }

    #[test]
    fn the_requested_seed_survives_and_the_rest_fall_back_to_the_clock() {
        assert_eq!(seed_for(Some(7)), 7);
        assert_eq!(seed_for(Some(i64::from(u32::MAX))), u32::MAX);
        // A seed llama.cpp cannot represent is not an error, it means "not
        // specified", exactly like passing none at all.
        let _ = seed_for(Some(-4));
        let _ = seed_for(None);
    }
}
