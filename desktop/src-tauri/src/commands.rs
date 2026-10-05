//! Tauri commands: the complete bridge between the UI and the Rust engine.
//!
//! Every command is `async`, so nothing runs on the main thread; filesystem and
//! sysinfo work is moved onto the blocking pool. Errors are [`AppError`], which
//! serialises to `{ code, message, detail }` so the frontend can show a friendly
//! sentence while the Logs page keeps the technical text.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use smollm_core::chat::{
    approx_token_count, ChatRequest, EngineMetrics, LoadModelOptions, LoadModelRequest,
    LoadModelResponse, SamplingParams,
};
use smollm_core::config::Settings;
use smollm_core::model::{
    estimate_ram_gb, CatalogStatus, LocalModel, ModelDescriptor, ModelMetadata,
};
use smollm_core::system::{
    AppInfo, Backend, DoctorReport, HardwareReport, LogEntry, LogFilter, LogStream, ServerConfig,
    ServerStatus,
};
use smollm_core::{AppError, AppResult};
use smollm_engine::benchmark::{self, BenchmarkConfig, BenchmarkProgress, BenchmarkResult};
use smollm_engine::{EngineManager, TokenStream};
use smollm_models::catalog::{CatalogFacets, CatalogFilters, SortKey};
use smollm_models::download::{DownloadState, DownloadTask};
use smollm_server::{self as server, ServerState, SharedState};
use tauri::{AppHandle, Emitter, State};

use crate::engine_kind::{fallback_warning, kind_for_backend, recommended_backend};
use crate::events::{emit_chat_error, ChatErrorPayload, BENCHMARK_PROGRESS};
use crate::state::AppState;
use crate::tasks;

/// One catalog entry, enriched with what the machine and the library know.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub model: ModelDescriptor,
    pub downloaded: bool,
    pub estimated_ram_gb: f64,
    pub fits_memory: bool,
    pub downloading: bool,
    pub download_percent: f64,
    /// The state of this model's most recent transfer, so a card can say
    /// *retrying* rather than *running* and offer Retry after a failure.
    pub download_state: Option<DownloadState>,
    pub download_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingPreset {
    pub name: String,
    pub label: String,
    pub params: SamplingParams,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerExamples {
    pub curl: String,
    pub python: String,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetOutcome {
    pub info: AppInfo,
    pub cleared_settings: bool,
    pub removed_part_files: usize,
    pub kept_models: usize,
    pub note: String,
}

type Shared<'a> = State<'a, Arc<AppState>>;

/// Run blocking work (filesystem, sysinfo) off the async worker threads.
async fn run_blocking<T, F>(work: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| AppError::Internal("a background task panicked"))
}

// ---------------------------------------------------------------------------
// hardware, app info, doctor
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn detect_hardware(state: Shared<'_>) -> AppResult<HardwareReport> {
    let report = run_blocking(smollm_hardware::detect).await?;
    state.store_hardware(report.clone());
    tracing::info!(
        target: "app",
        platform = report.platform.label(),
        ram_gb = report.total_ram_gb,
        cores = report.logical_cores,
        "hardware detected"
    );
    Ok(report)
}

#[tauri::command]
pub async fn get_app_info(app: AppHandle, state: Shared<'_>) -> AppResult<AppInfo> {
    build_app_info(&app, &state)
}

fn build_app_info(app: &AppHandle, state: &Arc<AppState>) -> AppResult<AppInfo> {
    let paths = state.paths();
    let manager = state.engine()?;
    Ok(AppInfo {
        name: app.package_info().name.clone(),
        version: app.package_info().version.to_string(),
        tagline: "Run small local LLMs beautifully on macOS and Windows.".to_string(),
        engine: manager.engine_name(),
        simulated_engine: manager.is_simulated(),
        platform: smollm_core::system::Platform::current().label().to_string(),
        arch: std::env::consts::ARCH.to_string(),
        data_dir: paths.data_dir.display().to_string(),
        models_dir: paths.models_dir.display().to_string(),
        logs_dir: paths.logs_dir.display().to_string(),
    })
}

#[tauri::command]
pub async fn get_doctor_report(state: Shared<'_>) -> AppResult<DoctorReport> {
    let hardware = match state.cached_hardware() {
        Some(report) => report,
        None => {
            let report = run_blocking(smollm_hardware::detect).await?;
            state.store_hardware(report.clone());
            report
        }
    };
    let candidates: Vec<smollm_hardware::ModelCandidate> = state
        .catalog
        .models()
        .iter()
        .map(|model| smollm_hardware::ModelCandidate {
            id: model.id.clone(),
            parameters_b: model.parameters_b,
            size_mb: model.size_mb,
            context_length: model.context_length,
            placeholder: matches!(model.status, CatalogStatus::Placeholder),
        })
        .collect();
    Ok(smollm_hardware::doctor_with(&hardware, &candidates))
}

// ---------------------------------------------------------------------------
// catalog, library, downloads
// ---------------------------------------------------------------------------

/// The Models page filter bar, as one argument so the command surface stays
/// narrow. Every field is optional; an empty string means "no constraint".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogQuery {
    pub query: Option<String>,
    pub sort: Option<String>,
    pub min_parameters_b: Option<f32>,
    pub max_parameters_b: Option<f32>,
    pub quantization: Option<String>,
    pub tag: Option<String>,
    pub license: Option<String>,
    pub architecture: Option<String>,
    pub hide_placeholders: Option<bool>,
}

impl CatalogQuery {
    fn into_filters(self) -> CatalogFilters {
        let CatalogQuery {
            query,
            sort,
            min_parameters_b,
            max_parameters_b,
            quantization,
            tag,
            license,
            architecture,
            hide_placeholders,
        } = self;
        CatalogFilters {
            query: query.unwrap_or_default(),
            min_parameters_b,
            max_parameters_b,
            quantization: optional(quantization),
            tag: optional(tag),
            license: optional(license),
            architecture: optional(architecture),
            hide_placeholders: hide_placeholders.unwrap_or(true),
            sort: parse_sort(sort.as_deref()),
        }
    }
}

/// An empty or whitespace-only filter value means "any", not "match nothing".
fn optional(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[tauri::command]
pub async fn list_catalog_models(
    state: Shared<'_>,
    filters: Option<CatalogQuery>,
    // `Some(true)` = only what is on disk, `Some(false)` = only what is not.
    downloaded: Option<bool>,
) -> AppResult<Vec<CatalogEntry>> {
    // No filter bar at all still means "hide unverified ids".
    let filters = filters.unwrap_or_default().into_filters();
    let state = Arc::clone(&state);
    let hardware = state.cached_hardware();
    run_blocking(move || {
        let models = state.catalog.filter(&filters);
        let library = state.library();
        let on_disk: Vec<String> = library
            .scan(&state.catalog)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|model| model.catalog_id)
            .collect();
        let tasks = state
            .downloads()
            .map(|manager| manager.snapshot())
            .unwrap_or_default();
        let usable_ram = hardware
            .as_ref()
            .map_or(f64::INFINITY, |report| report.available_ram_gb * 0.8);

        let mut entries: Vec<CatalogEntry> = models
            .into_iter()
            .map(|model| {
                let task = tasks
                    .iter()
                    .filter(|task| task.model_id == model.id)
                    .max_by_key(|task| task.started_ms);
                let estimated = model.estimated_ram_gb(model.context_length);
                CatalogEntry {
                    downloaded: on_disk.contains(&model.id),
                    estimated_ram_gb: estimated,
                    fits_memory: estimated <= usable_ram,
                    downloading: task.is_some_and(|task| task.state.is_active()),
                    download_percent: task.map_or(0.0, |task| task.percent),
                    download_state: task.map(|task| task.state),
                    download_error: task.and_then(|task| task.error.clone()),
                    model,
                }
            })
            .collect();
        // Whether an entry is on disk is only knowable here, next to the
        // library scan, so this filter stays out of the catalog crate.
        if let Some(wanted) = downloaded {
            entries.retain(|entry| entry.downloaded == wanted);
        }
        entries
    })
    .await
}

/// Values behind the Models page filter controls, derived from the loaded
/// catalog so a local overlay contributes its own tags and licenses.
#[tauri::command]
pub async fn catalog_facets(state: Shared<'_>) -> AppResult<CatalogFacets> {
    let state = Arc::clone(&state);
    run_blocking(move || state.catalog.facets()).await
}

fn parse_sort(value: Option<&str>) -> SortKey {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "smallest" => SortKey::Smallest,
        "largest" => SortKey::Largest,
        "fastest" => SortKey::Fastest,
        "name" => SortKey::Name,
        _ => SortKey::Recommended,
    }
}

#[tauri::command]
pub async fn list_local_models(state: Shared<'_>) -> AppResult<Vec<LocalModel>> {
    let state = Arc::clone(&state);
    run_blocking(move || state.library().scan(&state.catalog)).await?
}

#[tauri::command]
pub async fn pull_model(state: Shared<'_>, model_id: String) -> AppResult<DownloadTask> {
    let descriptor = descriptor_of(&state, &model_id)?;
    let manager = state.downloads()?;
    tracing::info!(target: "app", model = %descriptor.id, "download requested");
    manager.pull(&descriptor, None).await
}

#[tauri::command]
pub async fn cancel_download(
    state: Shared<'_>,
    download_id: String,
) -> AppResult<Option<DownloadTask>> {
    let manager = state.downloads()?;
    manager.cancel(&download_id)?;
    tracing::info!(target: "app", download_id = %download_id, "download cancelled");
    Ok(manager.task(&download_id))
}

#[tauri::command]
pub async fn retry_download(state: Shared<'_>, download_id: String) -> AppResult<DownloadTask> {
    let manager = state.downloads()?;
    let task = manager
        .task(&download_id)
        .ok_or_else(|| AppError::InvalidRequest(format!("unknown download {download_id}")))?;
    let descriptor = descriptor_of(&state, &task.model_id)?;
    manager.retry(&download_id, &descriptor).await
}

#[tauri::command]
pub async fn get_download_snapshot(state: Shared<'_>) -> AppResult<Vec<DownloadTask>> {
    Ok(state.downloads()?.snapshot())
}

#[tauri::command]
pub async fn delete_local_model(
    app: AppHandle,
    state: Shared<'_>,
    file_name: String,
) -> AppResult<String> {
    let owned = Arc::clone(&state);
    let removed = {
        let name = file_name.clone();
        let path = run_blocking(move || owned.library().delete(&name)).await??;
        path.display().to_string()
    };

    // Forget the model from memory if that file was the loaded one.
    let mut manager = state.engine()?;
    let was_loaded = manager
        .loaded_handle()
        .is_some_and(|handle| handle.path == removed);
    if was_loaded {
        manager.unload()?;
    }
    drop(manager);

    refresh_advertised(&app, &state);
    tracing::info!(target: "app", file = %file_name, unloaded = was_loaded, "local model deleted");
    Ok(removed)
}

fn descriptor_of(state: &Arc<AppState>, model_id: &str) -> AppResult<ModelDescriptor> {
    let id = model_id.trim();
    if id.is_empty() {
        return Err(AppError::InvalidRequest(
            "choose a model to download".to_string(),
        ));
    }
    state
        .catalog
        .find(id)
        .or_else(|| state.catalog.find_by_filename(id))
        .cloned()
        .ok_or_else(|| {
            AppError::ModelNotFound(format!(
                "{id} is not in the curated catalog; add it by dropping the .gguf file into the models folder"
            ))
        })
}

// ---------------------------------------------------------------------------
// engine: load, unload, chat, metrics
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn load_model(
    app: AppHandle,
    state: Shared<'_>,
    model_id: String,
    options: Option<LoadModelOptions>,
) -> AppResult<LoadModelResponse> {
    let owned = Arc::clone(&state);
    let response = run_blocking(move || load_into_engine(&owned, &model_id, options)).await??;
    refresh_advertised(&app, &state);
    Ok(response)
}

#[tauri::command]
pub async fn unload_model(app: AppHandle, state: Shared<'_>) -> AppResult<EngineMetrics> {
    let metrics = {
        let mut manager = state.engine()?;
        manager.unload()?;
        manager.metrics()
    };
    refresh_advertised(&app, &state);
    tracing::info!(target: "app", "model unloaded");
    Ok(metrics)
}

#[tauri::command]
pub async fn get_engine_metrics(state: Shared<'_>) -> AppResult<EngineMetrics> {
    Ok(state.engine()?.metrics())
}

/// Start one generation; output arrives as `chat-token` events and ends with
/// `chat-done` or `chat-error`.
#[tauri::command]
pub async fn start_chat_stream(
    app: AppHandle,
    state: Shared<'_>,
    mut request: ChatRequest,
) -> AppResult<String> {
    if request.request_id.trim().is_empty() {
        request.request_id = format!("chat-{}", uuid::Uuid::new_v4().simple());
    }
    request.validate()?;
    if request
        .messages
        .iter()
        .all(|message| message.content.trim().is_empty())
    {
        return Err(AppError::InvalidRequest(
            "write a message before sending".to_string(),
        ));
    }

    let requested_model = request.model_id.trim().to_string();
    let already_loaded = {
        let manager = state.engine()?;
        manager
            .loaded_handle()
            .is_some_and(|handle| handle.model_id == requested_model)
    };
    if !requested_model.is_empty() && !already_loaded {
        // Chat should just work: load what the user asked for, then generate.
        load_into_engine(&state, &requested_model, None)?;
    }

    let request_id = request.request_id.clone();
    let (stream, simulated, prompt_tokens) = {
        let mut manager = state.engine()?;
        let prompt_tokens = approx_token_count(&manager.render_prompt(&request));
        let stream: TokenStream = manager.start_chat(request)?;
        (stream, manager.is_simulated(), prompt_tokens)
    };
    tracing::info!(
        target: "app",
        request_id = %request_id,
        prompt_tokens,
        "generation started"
    );
    tasks::pump_tokens(app, request_id.clone(), stream, simulated);
    Ok(request_id)
}

#[tauri::command]
pub async fn stop_generation(state: Shared<'_>, request_id: String) -> AppResult<bool> {
    let stopped = state.engine()?.stop(&request_id);
    tracing::info!(target: "app", request_id = %request_id, stopped, "generation stop requested");
    if !stopped {
        return Err(AppError::InvalidRequest(format!(
            "{request_id} is not generating right now"
        )));
    }
    Ok(true)
}

/// Load (or switch to) a model, using settings defaults for anything unset.
fn load_into_engine(
    state: &Arc<AppState>,
    model_id: &str,
    options: Option<LoadModelOptions>,
) -> AppResult<LoadModelResponse> {
    let settings = state.settings()?;
    let id = model_id.trim();
    if id.is_empty() {
        return Err(AppError::InvalidRequest(
            "no model selected; pick one on the Models page".to_string(),
        ));
    }

    let mut manager = state.engine()?;
    if let Some(handle) = manager.loaded_handle() {
        if handle.model_id == id {
            return Ok(LoadModelResponse {
                handle,
                engine: manager.engine_name(),
                simulated: manager.is_simulated(),
                warnings: Vec::new(),
            });
        }
    }

    let descriptor = state.catalog.find(id).cloned();
    let path = state
        .library()
        .resolve(&state.catalog, id)
        .unwrap_or_else(|_| PathBuf::new());
    let on_disk = path.exists();
    let options = merge_options(options, &settings);
    let size_mb = model_size_mb(descriptor.as_ref(), &path, on_disk);

    refuse_when_ram_is_clearly_short(state, size_mb, options.context_length)?;

    let metadata = build_metadata(state, descriptor.as_ref(), &path, on_disk, &settings);
    let request = LoadModelRequest {
        model_id: id.to_string(),
        display_name: descriptor
            .as_ref()
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| id.to_string()),
        path,
        options,
        metadata,
    };

    let mut response = if manager.loaded_handle().is_some() {
        manager.switch(request)?
    } else {
        manager.load(request)?
    };
    if let Some(warning) = fallback_warning(
        settings.default_backend,
        kind_for_backend(settings.default_backend),
    ) {
        response.warnings.push(warning);
    }
    tracing::info!(
        target: "app",
        model = %response.handle.model_id,
        engine = %response.engine,
        simulated = response.simulated,
        "model loaded"
    );
    Ok(response)
}

fn model_size_mb(descriptor: Option<&ModelDescriptor>, path: &Path, on_disk: bool) -> u64 {
    if let Some(model) = descriptor {
        return model.size_mb;
    }
    if on_disk {
        return std::fs::metadata(path)
            .map(|meta| meta.len() / 1_000_000)
            .unwrap_or(0);
    }
    0
}

/// The spec asks the app to refuse a load the machine clearly cannot serve.
fn refuse_when_ram_is_clearly_short(
    state: &Arc<AppState>,
    size_mb: u64,
    context_length: u32,
) -> AppResult<()> {
    if size_mb == 0 {
        return Ok(());
    }
    let hardware = match state.cached_hardware() {
        Some(report) => report,
        // Nothing measured yet: never block a load on a guess.
        None => return Ok(()),
    };
    let needed = estimate_ram_gb(size_mb, context_length);
    let available = hardware.available_ram_gb;
    if needed > available * 0.95 {
        return Err(AppError::InsufficientMemory {
            required_gb: needed,
            available_gb: available,
        });
    }
    Ok(())
}

fn merge_options(options: Option<LoadModelOptions>, settings: &Settings) -> LoadModelOptions {
    let fallback = LoadModelOptions {
        context_length: settings.default_context_length.max(512),
        gpu_layers: settings.default_gpu_layers,
        backend: settings.default_backend,
        threads: None,
    };
    match options {
        // An explicit request wins, except for values the UI leaves at zero.
        Some(given) => LoadModelOptions {
            context_length: if given.context_length == 0 {
                fallback.context_length
            } else {
                given.context_length
            },
            gpu_layers: given.gpu_layers,
            backend: if given.backend == Backend::Cpu && settings.default_backend != Backend::Cpu {
                settings.default_backend
            } else {
                given.backend
            },
            threads: given.threads,
        },
        None => fallback,
    }
}

fn build_metadata(
    state: &Arc<AppState>,
    descriptor: Option<&ModelDescriptor>,
    path: &Path,
    on_disk: bool,
    settings: &Settings,
) -> Option<ModelMetadata> {
    if on_disk {
        if let Ok(local) = state.library().inspect(path, &state.catalog) {
            return Some(local.metadata);
        }
    }
    descriptor.map(|model| ModelMetadata {
        name: Some(model.display_name.clone()),
        architecture: Some(model.family.label().to_string()),
        quantization: Some(model.quantization.clone()),
        parameters_b: Some(model.parameters_b),
        context_length: Some(model.context_length.min(settings.default_context_length)),
        train_type: Some("instruct".to_string()),
        license: Some(model.license.clone()),
        ..ModelMetadata::default()
    })
}

// ---------------------------------------------------------------------------
// server
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    state: Shared<'_>,
    config: Option<ServerConfig>,
) -> AppResult<ServerStatus> {
    if state.server_is_running() {
        let base_url = state
            .server_state()
            .map(|shared| shared.config.base_url())
            .unwrap_or_default();
        return Err(AppError::ServerAlreadyRunning(base_url));
    }

    let config = state.server_config_for(config)?;
    config.validate()?;
    let shared: SharedState = Arc::new(
        ServerState::new(
            Arc::clone(&state.engine),
            config.clone(),
            app.package_info().version.to_string(),
        )
        .with_logs(state.logs.clone()),
    );
    advertise_all(&shared, &state);

    let handle = server::serve(shared.clone()).await?;
    let url = handle.base_url();
    let running: Arc<AtomicBool> = state.store_server(handle, shared.clone())?;
    tasks::watch_server(app.clone(), shared.clone(), running);

    tracing::info!(target: "app", url = %url, "local server started");
    push_server_line(&state, "info", format!("listening on {url}"));
    let _ = app.emit("server-state", "started");
    Ok(server::status(&shared, true))
}

#[tauri::command]
pub async fn stop_server(app: AppHandle, state: Shared<'_>) -> AppResult<ServerStatus> {
    let shared = state.server_state();
    let Some(handle) = state.take_server()? else {
        match shared {
            Some(shared) => return Ok(server::status(&shared, false)),
            None => return Err(AppError::ServerNotRunning),
        }
    };

    let base_url = handle.base_url();
    // Never hold the state lock while awaiting the graceful shutdown.
    handle.stop().await?;
    let status = shared
        .as_ref()
        .map(|state| server::status(state, false))
        .unwrap_or_default();
    state.clear_server_state();

    tracing::info!(target: "app", url = %base_url, "local server stopped");
    push_server_line(&state, "info", format!("stopped on {base_url}"));
    let _ = app.emit("server-state", "stopped");
    Ok(status)
}

#[tauri::command]
pub async fn get_server_status(state: Shared<'_>) -> AppResult<ServerStatus> {
    if let Some(shared) = state.server_state() {
        return Ok(server::status(&shared, state.server_is_running()));
    }
    let config = state.server_config()?;
    let manager = state.engine()?;
    Ok(ServerStatus {
        running: false,
        host: config.host.clone(),
        port: config.port,
        base_url: String::new(),
        engine: manager.engine_name(),
        loaded_model: manager.loaded_handle().map(|handle| handle.model_id),
        requests: 0,
        uptime_seconds: 0,
        simulated: manager.is_simulated(),
    })
}

#[tauri::command]
pub async fn get_server_examples(
    state: Shared<'_>,
    model_id: Option<String>,
) -> AppResult<ServerExamples> {
    let config = state.server_config()?;
    let model = model_id
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            state
                .engine()
                .ok()
                .and_then(|manager| manager.loaded_handle().map(|handle| handle.model_id))
        })
        .unwrap_or_default();
    Ok(ServerExamples {
        curl: server::curl_example(&config, &model),
        python: server::python_example(&config, &model),
        base_url: format!("{}/v1", config.base_url()),
        model: if model.trim().is_empty() {
            "default".to_string()
        } else {
            model
        },
    })
}

/// Advertise every model the app could serve, so `/v1/models` is useful.
fn advertise_all(shared: &SharedState, state: &Arc<AppState>) {
    let library = state.library();
    let mut ids: Vec<String> = state
        .catalog
        .models()
        .iter()
        .filter(|model| library.exists(model))
        .map(|model| model.id.clone())
        .collect();
    if let Ok(manager) = state.engine() {
        if let Some(handle) = manager.loaded_handle() {
            if !ids.contains(&handle.model_id) {
                ids.insert(0, handle.model_id);
            }
        }
    }
    shared.advertise(&ids);
}

pub(crate) fn refresh_advertised(app: &AppHandle, state: &Arc<AppState>) {
    if let Some(shared) = state.server_state() {
        advertise_all(&shared, state);
        let _ = app.emit("server-models-updated", ());
    }
}

fn push_server_line(state: &Arc<AppState>, level: &str, message: String) {
    state.logs.push(LogEntry {
        timestamp_ms: crate::now_millis(),
        level: level.to_string(),
        target: "smollm_server".to_string(),
        stream: LogStream::Server,
        message,
    });
}

// ---------------------------------------------------------------------------
// benchmark
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn run_benchmark(
    app: AppHandle,
    state: Shared<'_>,
    config: BenchmarkConfig,
) -> AppResult<BenchmarkResult> {
    let config = config.normalised();
    config.validate().map_err(AppError::InvalidRequest)?;

    let kind = kind_for_backend(config.backend);
    let load_request = load_request_for(&state, &config.model_id, &config)?;

    // A dedicated manager keeps the benchmark honest: it measures a cold load
    // and never competes with the chat or the server for the shared engine.
    let mut manager = EngineManager::with_kind(kind);
    let emitter = {
        let app = app.clone();
        move |progress: BenchmarkProgress| {
            let _ = app.emit(BENCHMARK_PROGRESS, &progress);
        }
    };
    let outcome = benchmark::run(
        &mut manager,
        &config,
        load_request,
        smollm_hardware::detect::current_memory_mb,
        emitter,
    )
    .await;

    match outcome {
        Ok(result) => {
            tracing::info!(
                target: "app",
                model = %result.model_id,
                generation_tps = result.generation_tokens_per_second,
                simulated = result.simulated,
                "benchmark finished"
            );
            Ok(result)
        }
        Err(error) => {
            emit_chat_error(
                &app,
                ChatErrorPayload {
                    request_id: "benchmark".to_string(),
                    error: error.to_payload(),
                },
            );
            Err(error)
        }
    }
}

fn load_request_for(
    state: &Arc<AppState>,
    model_id: &str,
    config: &BenchmarkConfig,
) -> AppResult<LoadModelRequest> {
    let settings = state.settings()?;
    let descriptor = state.catalog.find(model_id).cloned();
    let path = state
        .library()
        .resolve(&state.catalog, model_id)
        .unwrap_or_else(|_| PathBuf::new());
    let metadata = build_metadata(state, descriptor.as_ref(), &path, path.exists(), &settings);
    Ok(LoadModelRequest {
        model_id: model_id.to_string(),
        display_name: descriptor
            .map(|model| model.display_name)
            .unwrap_or_else(|| model_id.to_string()),
        path,
        options: LoadModelOptions {
            context_length: config.context_length,
            gpu_layers: config.gpu_layers,
            backend: config.backend,
            threads: None,
        },
        metadata,
    })
}

// ---------------------------------------------------------------------------
// logs, settings, folders
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_logs(state: Shared<'_>, filter: Option<LogFilter>) -> AppResult<Vec<LogEntry>> {
    Ok(state.logs.query(&filter.unwrap_or_default()))
}

#[tauri::command]
pub async fn clear_logs(state: Shared<'_>) -> AppResult<usize> {
    let before = state.logs.len();
    state.logs.clear();
    tracing::info!(target: "app", cleared = before, "log buffer cleared");
    Ok(before)
}

#[tauri::command]
pub async fn get_settings(state: Shared<'_>) -> AppResult<Settings> {
    state.settings()
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: Shared<'_>,
    settings: Settings,
) -> AppResult<Settings> {
    settings.sampling.validate()?;
    if !(512..=32_768).contains(&settings.default_context_length) {
        return Err(AppError::InvalidRequest(format!(
            "context length must be between 512 and 32768, got {}",
            settings.default_context_length
        )));
    }
    let config = ServerConfig {
        host: settings.server_host.clone(),
        port: settings.server_port,
        default_model_id: settings.default_model_id.clone(),
    };
    config.validate()?;

    let kind = kind_for_backend(settings.default_backend);
    state.use_engine_kind(kind)?;

    let previous = state.settings()?;
    if settings.model_dir != previous.model_dir {
        state.apply_model_dir(settings.model_dir.clone())?;
    }

    let paths = state.paths();
    settings.save(&paths)?;
    {
        let mut guard = AppState::lock(&state.settings, "settings")?;
        *guard = settings.clone();
    }
    refresh_advertised(&app, &state);
    tracing::info!(target: "app", theme = ?settings.theme, "settings saved");
    Ok(settings)
}

#[tauri::command]
pub async fn get_presets() -> AppResult<Vec<SamplingPreset>> {
    Ok(SamplingParams::preset_names()
        .iter()
        .map(|name| SamplingPreset {
            name: (*name).to_string(),
            label: capitalise(name),
            params: SamplingParams::preset(name),
        })
        .collect())
}

fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[tauri::command]
pub async fn open_model_folder(state: Shared<'_>) -> AppResult<String> {
    let paths = state.paths();
    paths.ensure()?;
    let target = paths.models_dir.display().to_string();
    run_blocking({
        let target = target.clone();
        move || reveal(&target)
    })
    .await??;
    tracing::info!(target: "app", path = %target, "opened model folder");
    Ok(target)
}

#[tauri::command]
pub async fn open_log_folder(state: Shared<'_>) -> AppResult<String> {
    let paths = state.paths();
    paths.ensure()?;
    let target = paths.logs_dir.display().to_string();
    run_blocking({
        let target = target.clone();
        move || reveal(&target)
    })
    .await??;
    Ok(target)
}

/// Reveal a folder with the platform's own file manager. Nothing is uploaded and
/// no shell string is built, so a path can never inject extra arguments.
fn reveal(path: &str) -> AppResult<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|source| {
            AppError::Config(format!("could not open {path} with {opener}: {source}"))
        })
}

#[tauri::command]
pub async fn reset_app_data(app: AppHandle, state: Shared<'_>) -> AppResult<ResetOutcome> {
    let paths = state.paths();
    let defaults = Settings::default();
    defaults.save(&paths)?;
    {
        let mut guard = AppState::lock(&state.settings, "settings")?;
        *guard = defaults;
    }
    state.logs.clear();

    // Partial transfers are the only thing safe to delete: models belong to the
    // user and are removed one by one from the Library page.
    let models_dir = paths.models_dir.clone();
    let removed = run_blocking(move || remove_part_files(&models_dir)).await?;
    let kept = count_gguf_files(&paths.models_dir);
    let info = build_app_info(&app, &state)?;
    tracing::warn!(target: "app", removed, kept, "app data reset");
    Ok(ResetOutcome {
        info,
        cleared_settings: true,
        removed_part_files: removed,
        kept_models: kept,
        note: "Settings and the log buffer were reset. Downloaded models were kept; delete them from the Library page.".to_string(),
    })
}

fn remove_part_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "part")
        })
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

fn count_gguf_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("gguf"))
        .count()
}

/// Write a JSON snapshot the user can attach to a bug report.
#[tauri::command]
pub async fn export_diagnostics(app: AppHandle, state: Shared<'_>) -> AppResult<String> {
    let paths = state.paths();
    let settings = state.settings()?;
    let metrics = state.engine()?.metrics();
    let logs = state.logs.snapshot();
    let hardware = match state.cached_hardware() {
        Some(report) => report,
        None => run_blocking(smollm_hardware::detect).await?,
    };
    let server_status = if state.server_is_running() {
        state
            .server_state()
            .map(|shared| server::status(&shared, true))
    } else {
        None
    };
    let payload = serde_json::json!({
        "app": build_app_info(&app, &state)?,
        "settings": settings,
        "hardware": hardware,
        "recommendedBackend": recommended_backend(&hardware),
        "engine": metrics,
        "server": server_status,
        "logs": logs,
    });
    let text = serde_json::to_string_pretty(&payload)?;
    let file = paths
        .logs_dir
        .join(format!("diagnostics-{}.json", crate::now_millis()));
    run_blocking(move || {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, text)?;
        Ok(file.display().to_string())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the serialization test constructs these directly.
    use smollm_models::catalog::FacetValue;

    #[test]
    fn sort_labels_map_onto_catalog_keys() {
        assert_eq!(parse_sort(None), SortKey::Recommended);
        assert_eq!(parse_sort(Some("SMALLEST")), SortKey::Smallest);
        assert_eq!(parse_sort(Some("fastest")), SortKey::Fastest);
        assert_eq!(parse_sort(Some("nonsense")), SortKey::Recommended);
    }

    /// Pins the `filters` object shape that `desktop/src/lib/api.ts` sends, so
    /// the two sides cannot drift without a test failing.
    #[test]
    fn catalog_query_accepts_the_frontend_shape() {
        let value = serde_json::json!({
            "query": "qwen",
            "sort": "fastest",
            "minParametersB": 1.0,
            "maxParametersB": 3.0,
            "quantization": "",
            "tag": "chat",
            "license": "apache-2.0",
            "architecture": null,
            "hidePlaceholders": true,
        });
        let filters: CatalogQuery = serde_json::from_value(value).expect("camelCase keys");
        let filters = filters.into_filters();
        assert_eq!(filters.query, "qwen");
        assert_eq!(filters.sort, SortKey::Fastest);
        assert_eq!(filters.min_parameters_b, Some(1.0));
        assert_eq!(filters.max_parameters_b, Some(3.0));
        assert_eq!(
            filters.quantization, None,
            "an untouched select means 'any', not 'match nothing'"
        );
        assert_eq!(filters.tag, Some("chat".to_string()));
        assert_eq!(filters.architecture, None);
        assert!(filters.hide_placeholders);

        // No filter bar at all still means unverified ids stay hidden.
        let bare: CatalogQuery = serde_json::from_str("{}").expect("every field optional");
        assert!(bare.into_filters().hide_placeholders);
    }

    #[test]
    fn facets_serialize_camel_case_for_the_ui() {
        let facets = CatalogFacets {
            quantizations: vec![FacetValue {
                value: "Q4_K_M".to_string(),
                count: 11,
            }],
            tags: Vec::new(),
            licenses: Vec::new(),
            architectures: vec![FacetValue {
                value: "granite".to_string(),
                count: 1,
            }],
            max_parameters_b: 4.0,
        };
        let json = serde_json::to_value(facets).expect("serialisable");
        assert_eq!(json["quantizations"][0]["value"], "Q4_K_M");
        assert_eq!(json["quantizations"][0]["count"], 11);
        assert_eq!(json["maxParametersB"], 4.0);
        assert_eq!(json["architectures"][0]["value"], "granite");
    }

    #[test]
    fn preset_labels_are_human_readable() {
        assert_eq!(capitalise("coding"), "Coding");
        assert_eq!(capitalise(""), "");
    }

    #[test]
    fn merge_options_falls_back_to_settings() {
        let settings = Settings {
            default_context_length: 2048,
            default_gpu_layers: 12,
            default_backend: Backend::Metal,
            ..Settings::default()
        };
        let calm = merge_options(None, &settings);
        assert_eq!(calm.context_length, 2048);
        assert_eq!(calm.gpu_layers, 12);
        assert_eq!(calm.backend, Backend::Metal);

        // An explicit backend of cpu is only overridden by a non-default setting
        // when the caller left the field at its serde default.
        let given = LoadModelOptions {
            context_length: 0,
            gpu_layers: 99,
            backend: Backend::Cpu,
            threads: Some(4),
        };
        let merged = merge_options(Some(given), &settings);
        assert_eq!(merged.context_length, 2048);
        assert_eq!(merged.gpu_layers, 99);
        assert_eq!(merged.backend, Backend::Metal);
        assert_eq!(merged.threads, Some(4));
    }

    #[test]
    fn model_size_prefers_the_catalog_and_falls_back_to_the_file() {
        let descriptor = ModelDescriptor {
            size_mb: 400,
            ..ModelDescriptor::default()
        };
        assert_eq!(
            model_size_mb(Some(&descriptor), Path::new("/nope"), false),
            400
        );
        assert_eq!(model_size_mb(None, Path::new("/nope"), false), 0);
    }

    #[tokio::test]
    async fn blocking_helper_propagates_values() {
        assert_eq!(run_blocking(|| 40 + 2).await.expect("runs"), 42);
    }

    #[test]
    fn part_file_sweep_only_touches_resume_files() {
        let dir =
            std::env::temp_dir().join(format!("smollm-parts-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("keep.gguf"), b"model").expect("written");
        std::fs::write(dir.join("resume.gguf.part"), b"partial").expect("written");
        assert_eq!(remove_part_files(&dir), 1);
        assert_eq!(count_gguf_files(&dir), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reset_note_explains_that_models_are_kept() {
        let outcome = ResetOutcome {
            info: AppInfo {
                name: "SmolLLM Studio".to_string(),
                version: "0.1.0".to_string(),
                tagline: String::new(),
                engine: "mock".to_string(),
                simulated_engine: true,
                platform: "macOS".to_string(),
                arch: "aarch64".to_string(),
                data_dir: String::new(),
                models_dir: String::new(),
                logs_dir: String::new(),
            },
            cleared_settings: true,
            removed_part_files: 0,
            kept_models: 3,
            note: "Settings and the log buffer were reset. Downloaded models were kept; delete them from the Library page.".to_string(),
        };
        let json = serde_json::to_value(&outcome).expect("serialises");
        assert_eq!(json["keptModels"], 3);
        assert!(json["note"]
            .as_str()
            .is_some_and(|note| note.contains("models were kept")));
    }
}
