//! What each `smollm` subcommand actually does.
//!
//! Every function here calls the same crates the desktop app calls, so the CLI
//! is a second surface rather than a second implementation.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{bail, Context as _, Result};
use futures::StreamExt;
use smollm_core::chat::{
    chat_message, ChatRequest, GenToken, LoadModelOptions, LoadModelRequest, Role, SamplingParams,
    TokenUsage,
};
use smollm_core::config::Settings;
use smollm_core::model::{
    estimate_ram_gb, CatalogStatus, LocalModel, ModelDescriptor, ModelMetadata,
};
use smollm_core::system::{HardwareReport, ServerConfig};
use smollm_core::AppPaths;
use smollm_engine::{benchmark, BenchmarkConfig, EngineKind, EngineManager};
use smollm_hardware::ModelCandidate;
use smollm_models::{DownloadEvent, DownloadManager, HfClient, ModelCatalog, ModelLibrary};
use smollm_server::{self as server, ServerState, SharedState};
use tracing_subscriber::EnvFilter;

use crate::report;
use crate::{BenchArgs, RunArgs, ServeArgs};

/// Everything a command needs: paths, catalog, library and saved settings.
pub struct Context {
    pub paths: AppPaths,
    pub catalog: ModelCatalog,
    pub library: ModelLibrary,
    pub settings: Settings,
    /// Detection costs a sysinfo pass, so each command pays for it at most once.
    hardware: Option<HardwareReport>,
}

impl Context {
    pub fn open(models_dir: Option<PathBuf>) -> Result<Self> {
        // Settings live in the platform data folder, so read them before the
        // model directory is decided.
        let base = AppPaths::new(None);
        base.ensure()?;
        let settings = Settings::load(&base).unwrap_or_default();
        let effective = models_dir.or_else(|| settings.model_dir.as_ref().map(PathBuf::from));
        let paths = AppPaths::new(effective);
        paths.ensure()?;
        let catalog = smollm_models::load_catalog(&paths);
        let library = ModelLibrary::new(paths.clone());
        Ok(Self {
            paths,
            catalog,
            library,
            settings,
            hardware: None,
        })
    }

    fn hardware(&mut self) -> HardwareReport {
        if let Some(report) = &self.hardware {
            return report.clone();
        }
        let report = smollm_hardware::detect_in(&self.paths);
        self.hardware = Some(report.clone());
        report
    }
}

/// `RUST_LOG` wins, then `--verbose`, then a quiet default.
pub fn install_tracing(verbose: bool) {
    let default = if verbose {
        "info,smollm=debug,reqwest=warn"
    } else {
        "warn,reqwest=warn"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------------------------------------------------------------------------
// hardware and doctor
// ---------------------------------------------------------------------------

pub fn hardware(ctx: &mut Context, json: bool) -> Result<()> {
    let report_data = ctx.hardware();
    if json {
        return print_json(&report_data);
    }
    let gpu = if let Some(nvidia) = &report_data.nvidia_gpu {
        format!("{nvidia} (CUDA)")
    } else if report_data.metal_available {
        format!("{} (Metal)", report_data.gpu_name)
    } else if report_data.vulkan_available {
        format!("{} (Vulkan)", report_data.gpu_name)
    } else {
        "none detected — CPU inference".to_string()
    };
    println!(
        "{}",
        report::key_values(&[
            (
                "Platform",
                format!("{} {}", report_data.platform.label(), report_data.arch)
            ),
            (
                "CPU",
                format!(
                    "{} ({} physical / {} logical cores)",
                    report_data.cpu_brand, report_data.physical_cores, report_data.logical_cores
                )
            ),
            (
                "Memory",
                format!(
                    "{:.1} GB total · {:.1} GB available",
                    report_data.total_ram_gb, report_data.available_ram_gb
                )
            ),
            ("GPU", gpu),
            (
                "Disk",
                format!(
                    "{:.1} GB free · {:.1} GB free where models live",
                    report_data.disk_free_gb, report_data.model_volume_free_gb
                )
            ),
            ("Models", ctx.paths.models_dir.display().to_string()),
            ("Logs", ctx.paths.logs_dir.display().to_string()),
        ])
    );
    Ok(())
}

pub fn doctor(ctx: &mut Context, json: bool) -> Result<()> {
    let hardware = ctx.hardware();
    let candidates: Vec<ModelCandidate> = ctx
        .catalog
        .models()
        .iter()
        .map(|model| ModelCandidate {
            id: model.id.clone(),
            parameters_b: model.parameters_b,
            size_mb: model.size_mb,
            context_length: model.context_length,
            placeholder: model.status == CatalogStatus::Placeholder,
        })
        .collect();
    let report_data = smollm_hardware::doctor_with(&hardware, &candidates);
    if json {
        return print_json(&report_data);
    }
    println!("{}", report_data.headline);
    println!(
        "  backend: {} · engine: {}",
        report_data.backend_recommendation.as_str(),
        engine_kind("auto")?.as_str()
    );
    if !report_data.recommended_models.is_empty() {
        println!(
            "  start with: {}",
            report_data.recommended_models.join(", ")
        );
    }
    for warning in &report_data.warnings {
        println!("  ! {warning}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// catalog, downloads and library
// ---------------------------------------------------------------------------

pub fn list_catalog(
    ctx: &Context,
    query: &str,
    sort: &str,
    max_params: Option<f32>,
    include_placeholders: bool,
    json: bool,
) -> Result<()> {
    use smollm_models::{CatalogFilters, SortKey};
    let filters = CatalogFilters {
        query: query.to_string(),
        max_parameters_b: max_params,
        min_parameters_b: None,
        quantization: None,
        tag: None,
        license: None,
        architecture: None,
        hide_placeholders: !include_placeholders,
        sort: match sort {
            "smallest" => SortKey::Smallest,
            "largest" => SortKey::Largest,
            "fastest" => SortKey::Fastest,
            "name" => SortKey::Name,
            _ => SortKey::Recommended,
        },
    };
    let models = ctx.catalog.filter(&filters);
    if json {
        return print_json(&models);
    }
    if models.is_empty() {
        println!("No catalog entry matched. Try `smollm models list --include-placeholders`.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = models
        .iter()
        .map(|model| {
            let state = if ctx.library.exists(model) {
                "downloaded"
            } else if model.status == CatalogStatus::Placeholder {
                "unverified"
            } else {
                "available"
            };
            vec![
                model.id.clone(),
                format!("{:.1}B", model.parameters_b),
                model.quantization.clone(),
                report::size_label(model.size_mb),
                state.to_string(),
            ]
        })
        .collect();
    print!(
        "{}",
        report::table(&["id", "params", "quant", "size", "state"], &rows)
    );
    println!(
        "\n{} model(s). Download one with `smollm models pull <id>`.",
        rows.len()
    );
    Ok(())
}

pub async fn pull(ctx: &Context, raw: &str, force: bool) -> Result<()> {
    let descriptor = resolve(ctx, raw)?;
    if descriptor.status == CatalogStatus::Placeholder {
        bail!(
            "`{}` has no confirmed Hugging Face file yet, so there is nothing to download.\n  \
             Run `smollm models list --include-placeholders` to see its notes, or point at a .gguf path.",
            descriptor.id
        );
    }
    let target = ctx.library.local_path(&descriptor);
    if target.exists() && !force {
        println!("Already downloaded: {}", target.display());
        return Ok(());
    }

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = DownloadManager::new(ctx.paths.clone(), HfClient::new(), sender);
    println!(
        "Downloading {} ({} MB) into {}",
        descriptor.filename,
        descriptor.size_mb,
        ctx.paths.models_dir.display()
    );
    let task = manager
        .pull(&descriptor, None)
        .await
        .with_context(|| format!("could not start the download of {}", descriptor.id))?;

    let tty = report::stdout_is_tty();
    while let Some(event) = receiver.recv().await {
        match event {
            DownloadEvent::Progress(progress) => {
                if tty {
                    report::progress_line(
                        progress.percent,
                        progress.downloaded_bytes,
                        progress.total_bytes.unwrap_or(0),
                        progress.bytes_per_second,
                    );
                }
            }
            DownloadEvent::Completed(completed) => {
                close_progress(tty);
                println!(
                    "Saved {} ({} bytes) in {} ms → {}",
                    completed.model_id, completed.size_bytes, completed.elapsed_ms, completed.path
                );
                return Ok(());
            }
            DownloadEvent::Failed(failure) => {
                close_progress(tty);
                bail!("download failed [{}]: {}", failure.code, failure.message);
            }
            DownloadEvent::Cancelled(cancelled) => {
                close_progress(tty);
                bail!(
                    "download cancelled; {} kept as a .part file for the next attempt",
                    report::human_size(cancelled.partial_bytes)
                );
            }
        }
    }
    close_progress(tty);
    bail!(
        "the transfer for {} ended without reporting a result (task {})",
        descriptor.id,
        task.id
    )
}

/// End an in-place progress line before printing a final message.
fn close_progress(tty: bool) {
    if tty {
        report::finish_line();
    }
}

pub fn list_local(ctx: &Context, json: bool) -> Result<()> {
    let models: Vec<LocalModel> = ctx.library.scan(&ctx.catalog)?;
    if json {
        return print_json(&models);
    }
    if models.is_empty() {
        println!(
            "Nothing downloaded yet. Try `smollm models list`, then `smollm models pull <id>`."
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = models
        .iter()
        .map(|model| {
            let note = model
                .parse_error
                .clone()
                .unwrap_or_else(|| model.metadata.quantization.clone().unwrap_or_default());
            vec![
                model.file_name.clone(),
                model.catalog_id.clone().unwrap_or_else(|| "-".to_string()),
                report::human_size(model.size_bytes),
                note,
            ]
        })
        .collect();
    print!(
        "{}",
        report::table(&["file", "catalog id", "size", "note"], &rows)
    );
    println!(
        "\n{} file(s), {} total in {}",
        models.len(),
        report::human_size(ctx.library.total_size_bytes(&models)),
        ctx.paths.models_dir.display()
    );
    Ok(())
}

pub fn remove(ctx: &Context, file: &str) -> Result<()> {
    let removed = ctx.library.delete(file)?;
    println!("Deleted {}", removed.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// chat
// ---------------------------------------------------------------------------

pub async fn run(ctx: &mut Context, args: &RunArgs) -> Result<()> {
    let kind = engine_kind(&args.engine)?;
    let options = LoadModelOptions {
        context_length: args.context.max(512),
        gpu_layers: args.gpu_layers,
        backend: ctx.settings.default_backend,
        threads: None,
    };
    let (request, _) = build_load_request(ctx, &args.model, options, kind)?;

    let mut manager = EngineManager::with_kind(kind);
    let loaded = manager.load(request)?;
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    if loaded.simulated {
        eprintln!(
            "note: the `{}` engine simulates inference, so the answer below is not real model output.",
            loaded.engine
        );
    }

    let prompt = read_prompt(args)?;
    let params = sampling(args);
    params.validate()?;
    let chat = ChatRequest {
        request_id: uuid::Uuid::new_v4().simple().to_string(),
        model_id: loaded.handle.model_id.clone(),
        messages: vec![chat_message(Role::User, prompt)],
        system_prompt: args.system.clone(),
        params,
        stop: Vec::new(),
    };

    let mut stream = manager.start_chat(chat)?;
    let mut answer = String::new();
    let mut usage: Option<TokenUsage> = None;
    let mut finish: Option<String> = None;
    let mut chunks = 0u32;
    let started = std::time::Instant::now();

    while let Some(item) = stream.next().await {
        match item {
            Err(error) => {
                if !args.buffer {
                    println!();
                }
                bail!("generation stopped: {error}");
            }
            Ok(GenToken {
                text,
                finish_reason,
                usage: token_usage,
            }) => {
                if !text.is_empty() {
                    chunks += 1;
                    answer.push_str(&text);
                    if !args.buffer && !args.json {
                        let mut stdout = std::io::stdout();
                        let _ = stdout.write_all(text.as_bytes());
                        let _ = stdout.flush();
                    }
                }
                if finish_reason.is_some() {
                    finish = finish_reason;
                }
                if token_usage.is_some() {
                    usage = token_usage;
                }
            }
        }
    }
    // Dropping the stream releases the engine's cancellation registry.
    drop(stream);

    let elapsed_ms = started.elapsed().as_millis() as u64;
    if args.json {
        return print_json(&serde_json::json!({
            "model": loaded.handle.model_id,
            "engine": loaded.engine,
            "simulated": loaded.simulated,
            "answer": answer,
            "finishReason": finish,
            "usage": usage,
            "elapsedMs": elapsed_ms,
            "chunks": chunks,
            "warnings": loaded.warnings,
        }));
    }
    if args.buffer {
        println!("{}", answer.trim_end());
    } else {
        println!();
    }
    let counted = usage.map_or(chunks, |usage| usage.completion_tokens.max(1));
    println!(
        "  {} · {} chunk(s), {} generated token(s) · {:.1} tok/s",
        loaded.engine,
        chunks,
        counted,
        rate(counted, elapsed_ms)
    );
    Ok(())
}

fn read_prompt(args: &RunArgs) -> Result<String> {
    match &args.prompt_file {
        Some(path) if path == Path::new("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("could not read the prompt from stdin")?;
            Ok(text)
        }
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("could not read the prompt file {}", path.display())),
        None => Ok(args.prompt.clone()),
    }
}

fn sampling(args: &RunArgs) -> SamplingParams {
    let mut params = SamplingParams::preset(&args.preset);
    if let Some(temperature) = args.temperature {
        params.temperature = temperature;
    }
    if let Some(top_p) = args.top_p {
        params.top_p = top_p;
    }
    if let Some(max_tokens) = args.max_tokens {
        params.max_tokens = max_tokens;
    }
    params
}

fn rate(tokens: u32, elapsed_ms: u64) -> f64 {
    if elapsed_ms == 0 {
        return 0.0;
    }
    tokens as f64 / (elapsed_ms as f64 / 1000.0)
}

// ---------------------------------------------------------------------------
// server
// ---------------------------------------------------------------------------

pub async fn serve(ctx: &mut Context, args: &ServeArgs) -> Result<()> {
    let config = ServerConfig {
        host: args.host.clone(),
        port: args.port,
        default_model_id: args
            .model
            .clone()
            .or_else(|| ctx.settings.default_model_id.clone()),
    };
    if args.examples_only {
        print_examples(&config, config.default_model_id.as_deref().unwrap_or(""));
        return Ok(());
    }

    let kind = engine_kind(&args.engine)?;
    let engine = Arc::new(Mutex::new(EngineManager::with_kind(kind)));
    if let Some(raw) = &args.model {
        let options = LoadModelOptions {
            context_length: args.context.max(512),
            gpu_layers: -1,
            backend: ctx.settings.default_backend,
            threads: None,
        };
        let (request, _) = build_load_request(ctx, raw, options, kind)?;
        let response = lock_engine(&engine).load(request)?;
        for warning in &response.warnings {
            eprintln!("warning: {warning}");
        }
        println!(
            "Loaded {} on the {} engine{}",
            response.handle.model_id,
            response.engine,
            if response.simulated {
                " (simulated)"
            } else {
                ""
            }
        );
    }

    let state: SharedState = Arc::new(ServerState::new(
        Arc::clone(&engine),
        config.clone(),
        env!("CARGO_PKG_VERSION"),
    ));
    advertise(&state, ctx);
    let handle = server::serve(Arc::clone(&state)).await?;

    println!("SmolLLM Studio server listening on {}", handle.base_url());
    println!("  GET  /health                 engine and model status");
    println!("  GET  /v1/models              what this server will answer for");
    println!("  GET  /v1/models/{{id}}         one model card");
    println!("  POST /v1/chat/completions    streaming and non-streaming");
    println!("  POST /v1/completions         raw completion");
    println!("  GET  /v1/engine/metrics      tokens per second, request counts");
    print_examples(&config, config.default_model_id.as_deref().unwrap_or(""));
    println!("Press ctrl-c to stop.");

    tokio::signal::ctrl_c()
        .await
        .context("could not listen for ctrl-c")?;
    println!("\nstopping…");
    let requests = state.request_count();
    handle.stop().await?;
    println!("Stopped after {requests} request(s).");
    Ok(())
}

fn print_examples(config: &ServerConfig, model_id: &str) {
    println!("health:\n{}\n", server::health_example(config));
    println!("\ncurl:\n{}\n", server::curl_example(config, model_id));
    println!(
        "\nStreaming (server-sent events):\n{}\n",
        server::curl_stream_example(config, model_id)
    );
    println!(
        "Python (OpenAI SDK):\n{}",
        server::python_example(config, model_id)
    );
}

fn advertise(state: &SharedState, ctx: &Context) {
    let mut ids: Vec<String> = ctx
        .catalog
        .models()
        .iter()
        .filter(|model| ctx.library.exists(model))
        .map(|model| model.id.clone())
        .collect();
    if let Some(handle) = lock_engine(&state.engine).loaded_handle() {
        if !ids.contains(&handle.model_id) {
            ids.insert(0, handle.model_id);
        }
    }
    state.advertise(&ids);
}

// ---------------------------------------------------------------------------
// benchmark
// ---------------------------------------------------------------------------

pub async fn bench(ctx: &mut Context, args: &BenchArgs) -> Result<()> {
    let kind = engine_kind(&args.engine)?;
    let mut config = BenchmarkConfig {
        model_id: String::new(),
        prompt_tokens: args.prompt_tokens,
        max_tokens: args.max_tokens,
        context_length: args.context.max(512),
        gpu_layers: args.gpu_layers,
        backend: ctx.settings.default_backend,
        runs: args.runs,
    };
    let (request, descriptor) = build_load_request(
        ctx,
        &args.model,
        LoadModelOptions {
            context_length: config.context_length,
            gpu_layers: config.gpu_layers,
            backend: config.backend,
            threads: None,
        },
        kind,
    )?;
    config.model_id = request.model_id.clone();
    config.validate().map_err(anyhow::Error::msg)?;

    // A dedicated manager keeps a benchmark away from anything else running.
    let mut manager = EngineManager::with_kind(kind);
    let result = benchmark::run(
        &mut manager,
        &config,
        request,
        smollm_hardware::detect::current_memory_mb,
        |progress| eprintln!("[{:.0}%] {}", progress.percent, progress.message),
    )
    .await?;

    if args.json {
        return print_json(&result);
    }
    println!("# Benchmark · {}", result.model_id);
    println!(
        "engine: {} · backend: {} · prompt: {} tokens · generated: {} tokens · runs: {}\n",
        result.engine,
        result.backend.as_str(),
        result.prompt_tokens,
        result.generated_tokens,
        result.runs
    );
    print!("{}", result.markdown_table());
    if let Some(model) = descriptor {
        println!(
            "\nCatalog estimate: {:.1} GB RAM at {} context.",
            model.estimated_ram_gb(config.context_length),
            config.context_length
        );
    }
    for warning in &result.warnings {
        println!("! {warning}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn resolve(ctx: &Context, raw: &str) -> Result<ModelDescriptor> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("a model id or file name is required");
    }
    ctx.catalog
        .find(trimmed)
        .or_else(|| ctx.catalog.find_by_filename(trimmed))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`{trimmed}` is not in the catalog. Run `smollm models list` for the ids this build knows."
            )
        })
}

/// Build the engine load request, preferring metadata read from the real file.
fn build_load_request(
    ctx: &mut Context,
    raw: &str,
    options: LoadModelOptions,
    kind: EngineKind,
) -> Result<(LoadModelRequest, Option<ModelDescriptor>)> {
    let descriptor = ctx
        .catalog
        .find(raw.trim())
        .or_else(|| ctx.catalog.find_by_filename(raw.trim()))
        .cloned();
    let id = descriptor
        .as_ref()
        .map(|model| model.id.clone())
        .unwrap_or_else(|| raw.trim().to_string());

    let mut path = ctx
        .library
        .resolve(&ctx.catalog, raw.trim())
        .unwrap_or_default();
    if path.as_os_str().is_empty() {
        let candidate = PathBuf::from(raw.trim());
        if candidate.exists() {
            path = candidate;
        }
    }
    let on_disk = path.exists();
    if !on_disk && kind != EngineKind::Mock {
        bail!(
            "`{id}` is not in the model folder yet.\n  download it with:  smollm models pull {id}\n  \
             or pass the path of a .gguf file you already have."
        );
    }

    let metadata = if on_disk {
        ctx.library
            .inspect(&path, &ctx.catalog)
            .ok()
            .map(|local| local.metadata)
            .or_else(|| catalog_metadata(descriptor.as_ref()))
    } else {
        catalog_metadata(descriptor.as_ref())
    };

    if on_disk {
        let size_mb = std::fs::metadata(&path)
            .map(|meta| meta.len() / 1_000_000)
            .unwrap_or(0);
        let needed = estimate_ram_gb(size_mb, options.context_length);
        let available = ctx.hardware().available_ram_gb;
        if needed > available {
            eprintln!(
                "warning: this model wants about {needed:.1} GB and only {available:.1} GB is free; \
                 expect swapping or a failure"
            );
        }
    }

    Ok((
        LoadModelRequest {
            model_id: id,
            display_name: descriptor
                .as_ref()
                .map(|model| model.display_name.clone())
                .unwrap_or_default(),
            path,
            options,
            metadata,
        },
        descriptor,
    ))
}

fn catalog_metadata(model: Option<&ModelDescriptor>) -> Option<ModelMetadata> {
    model.map(|model| ModelMetadata {
        name: Some(model.display_name.clone()),
        architecture: Some(model.family.label().to_string()),
        quantization: Some(model.quantization.clone()),
        parameters_b: Some(model.parameters_b),
        context_length: Some(model.context_length),
        train_type: Some("instruct".to_string()),
        license: Some(model.license.clone()),
        ..ModelMetadata::default()
    })
}

/// `--engine auto` prefers a real engine, then Mock, then metadata-only.
fn engine_kind(flag: &str) -> Result<EngineKind> {
    let trimmed = flag.trim();
    let requested = if trimmed.is_empty() || trimmed == "auto" {
        None
    } else {
        Some(EngineKind::parse(trimmed).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown engine `{trimmed}`; use auto | mock | llama-cpp | candle | gguf-metadata"
            )
        })?)
    };

    let fallback = [EngineKind::LlamaCpp, EngineKind::Candle, EngineKind::Mock]
        .into_iter()
        .find(|kind| kind.is_available())
        .unwrap_or(EngineKind::GgufMetadata);

    match requested {
        None => Ok(fallback),
        Some(kind) if kind.is_available() => Ok(kind),
        Some(kind) => {
            eprintln!(
                "warning: the {} engine is not compiled into this build; using {} instead. \
                 docs/hardware.md lists the cargo features.",
                kind.as_str(),
                fallback.as_str()
            );
            Ok(fallback)
        }
    }
}

fn lock_engine(engine: &Arc<Mutex<EngineManager>>) -> MutexGuard<'_, EngineManager> {
    // A poisoned lock means a worker panicked; the manager's own state is still
    // worth reading, so recover instead of aborting the command.
    engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
