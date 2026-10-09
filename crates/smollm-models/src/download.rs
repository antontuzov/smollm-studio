//! Resumable Hugging Face downloads with progress, cancellation and retry.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smollm_core::chat::CancelToken;
use smollm_core::error::{AppError, AppResult};
use smollm_core::gguf::GgufHeader;
use smollm_core::model::ModelDescriptor;
use smollm_core::paths::AppPaths;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::hf::{HfClient, RemoteFile};

/// Emit progress at most this often; keeps the event bus quiet.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// Speed is averaged over this window so the UI number does not jitter.
const SPEED_WINDOW: Duration = Duration::from_millis(1_000);
/// Extra headroom demanded beyond the model size itself.
const DISK_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
/// Transfers a connection can be re-attempted this many times before the task
/// is reported as failed. Each attempt resumes from the partial file, so a
/// dropped connection costs the bytes between the two checks, not the model.
const MAX_ATTEMPTS: u32 = 4;
/// First backoff, doubling per attempt.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// A `Retry-After` longer than this is treated as "come back later" and fails.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Read buffer used when hashing a finished file.
const HASH_CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Running,
    /// A transient network error; the next attempt is already scheduled.
    Retrying,
    Verifying,
    Complete,
    Cancelled,
    Failed,
}

impl DownloadState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled | Self::Failed)
    }

    /// True while bytes may still be moving, which is what the UI asks before
    /// offering a Cancel button.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Retrying | Self::Verifying
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    pub model_id: String,
    pub display_name: String,
    pub file_name: String,
    pub url: String,
    pub path: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub bytes_per_second: f64,
    pub percent: f64,
    pub state: DownloadState,
    pub error: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// True when the transfer continued an existing `.part` file.
    pub resumed: bool,
    pub sha256: Option<String>,
    /// 1 for the first attempt; the retry notice in `error` names the limit.
    pub attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub download_id: String,
    pub model_id: String,
    pub file_name: String,
    pub state: DownloadState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: f64,
    pub bytes_per_second: f64,
    pub error: Option<String>,
    pub attempt: u32,
    pub max_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadCompleted {
    pub download_id: String,
    pub model_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFailedEvent {
    pub download_id: String,
    pub model_id: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadCancelled {
    pub download_id: String,
    pub model_id: String,
    pub partial_bytes: u64,
}

/// Events the desktop layer forwards as `download-*` Tauri events.
#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event",
    content = "payload"
)]
pub enum DownloadEvent {
    #[serde(rename = "download-progress")]
    Progress(DownloadProgress),
    #[serde(rename = "download-complete")]
    Completed(DownloadCompleted),
    #[serde(rename = "download-error")]
    Failed(DownloadFailedEvent),
    #[serde(rename = "download-cancelled")]
    Cancelled(DownloadCancelled),
}

/// Owns in-flight transfers and their cancellation flags.
///
/// The `Arc`s are cloned into each spawned task so the manager stays cheap to
/// hold inside Tauri managed state.
#[derive(Clone)]
pub struct DownloadManager {
    paths: AppPaths,
    client: HfClient,
    tasks: Arc<Mutex<HashMap<String, DownloadTask>>>,
    cancels: Arc<Mutex<HashMap<String, CancelToken>>>,
    sink: mpsc::UnboundedSender<DownloadEvent>,
    /// First pause between resume attempts, doubling afterwards. Tests shrink it
    /// so a retry path does not sleep for seconds.
    backoff: Duration,
}

impl DownloadManager {
    pub fn new(
        paths: AppPaths,
        client: HfClient,
        sink: mpsc::UnboundedSender<DownloadEvent>,
    ) -> Self {
        Self {
            paths,
            client,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            sink,
            backoff: BACKOFF_BASE,
        }
    }

    pub fn model_dir(&self) -> &Path {
        &self.paths.models_dir
    }

    pub fn snapshot(&self) -> Vec<DownloadTask> {
        let mut tasks: Vec<DownloadTask> = self
            .tasks
            .lock()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.started_ms));
        tasks
    }

    pub fn task(&self, id: &str) -> Option<DownloadTask> {
        self.tasks
            .lock()
            .ok()
            .and_then(|guard| guard.get(id).cloned())
    }

    pub fn task_for_model(&self, model_id: &str) -> Option<DownloadTask> {
        self.tasks.lock().ok().and_then(|guard| {
            guard
                .values()
                .filter(|task| task.model_id == model_id)
                .max_by_key(|task| task.started_ms)
                .cloned()
        })
    }

    /// Start (or resume) a download for a catalog model.
    ///
    /// Calling this again while a transfer of the same file is still moving
    /// returns that transfer instead of opening a second writer on the same
    /// `.part` file, which is what makes a double-clicked Download button safe.
    pub async fn pull(
        &self,
        model: &ModelDescriptor,
        expected_sha256: Option<String>,
    ) -> AppResult<DownloadTask> {
        let file_name = sanitize_file_name(&model.filename)?;
        self.paths.ensure()?;

        let final_path = self.paths.models_dir.join(&file_name);
        let partial_path = partial_path_for(&final_path);

        if let Some(active) = self.active_transfer(&final_path) {
            return Ok(active);
        }

        // A file already on disk of a plausible size needs no round trip at all:
        // this keeps relaunching the app possible without a network.
        if let Some(size) = std::fs::metadata(&final_path).ok().map(|meta| meta.len()) {
            if size_matches(size, model.size_mb) {
                return Ok(self.complete_from_disk(
                    model,
                    &file_name,
                    &final_path,
                    size,
                    expected_sha256,
                ));
            }
        }

        // Ask Hugging Face what the file actually is before writing anything, so
        // a gated repo, a renamed export or a wrong catalog size surfaces as an
        // instant message instead of a half-written model.
        let remote = self.resolve_remote(model).await?;

        let resume_from = self.resume_offset(&partial_path, &remote);
        let already_have = resume_from;
        let required = remote
            .size_bytes
            .unwrap_or_else(|| expected_bytes(model.size_mb).unwrap_or(0))
            .saturating_sub(already_have);
        let free = free_space(&self.paths.models_dir)?;
        if required > 0 && free < required.saturating_add(DISK_HEADROOM_BYTES) {
            return Err(AppError::InsufficientDiskSpace {
                required_gb: gb(required),
                available_gb: gb(free),
            });
        }

        let mut task = self.build_task(model, &file_name, &final_path, already_have);
        task.url = remote.url.clone();
        task.total_bytes = remote.size_bytes;
        task.percent = task
            .total_bytes
            .map_or(0.0, |total| percent_of(already_have, total));
        task.resumed = already_have > 0;
        task.sha256 = expected_sha256.clone();
        self.remember(task.clone());

        let cancel = CancelToken::new();
        if let Ok(mut guard) = self.cancels.lock() {
            guard.insert(task.id.clone(), cancel.clone());
        }

        let runner = Runner {
            client: self.client.clone(),
            tasks: Arc::clone(&self.tasks),
            cancels: Arc::clone(&self.cancels),
            sink: self.sink.clone(),
            cancel,
            expected_sha256,
            task: task.clone(),
            final_path,
            partial_path,
            remote_etag: remote.etag.clone(),
            backoff: self.backoff,
        };
        tokio::spawn(async move { runner.run().await });

        Ok(task)
    }

    /// The authoritative size, digest-free description of the remote file.
    ///
    /// A failed probe is not fatal: the catalog's declared size is a workable
    /// fallback, and the server's own headers still validate the transfer.
    async fn resolve_remote(&self, model: &ModelDescriptor) -> AppResult<RemoteFile> {
        let fallback = || RemoteFile {
            url: self.client.resolve_url(model),
            size_bytes: expected_bytes(model.size_mb),
            status: 0,
            ..Default::default()
        };
        let remote = match self.client.probe(model).await {
            Ok(remote) => remote,
            Err(error) => {
                tracing::warn!(model = %model.id, "cannot read Hugging Face file metadata: {error}");
                return Ok(fallback());
            }
        };
        if remote.requires_consent() {
            return Err(AppError::DownloadFailed(format!(
                "{} requires Hugging Face consent (HTTP {}). Accept the model licence on huggingface.co or choose another catalog entry.",
                model.display_name, remote.status
            )));
        }
        if !remote.is_available() {
            return Err(AppError::DownloadFailed(format!(
                "Hugging Face reports HTTP {} for {}/{} at {}. The catalog entry may be out of date.",
                remote.status, model.hf_repo, model.revision, model.filename
            )));
        }
        Ok(remote)
    }

    /// Bytes already on disk that may be continued, or 0 after discarding a
    /// partial file that cannot be trusted.
    fn resume_offset(&self, partial_path: &Path, remote: &RemoteFile) -> u64 {
        let Some(offset) = std::fs::metadata(partial_path).ok().map(|meta| meta.len()) else {
            return 0;
        };
        let reason = match self.provenance_mismatch(partial_path, remote, offset) {
            Some(reason) => reason,
            None => return offset,
        };
        tracing::info!(path = %partial_path.display(), "starting the download over: {reason}");
        let _ = std::fs::remove_file(partial_path);
        clear_partial_meta(partial_path);
        0
    }

    /// Why this partial file must not be continued, if any reason exists.
    fn provenance_mismatch(
        &self,
        partial_path: &Path,
        remote: &RemoteFile,
        offset: u64,
    ) -> Option<String> {
        if let Some(total) = remote.size_bytes {
            if offset >= total {
                return Some(
                    "the partial file is at least as large as the current remote export"
                        .to_string(),
                );
            }
        }
        let Some(meta) = read_partial_meta(partial_path) else {
            // No sidecar: either an older build wrote this file or the process
            // died before the header response arrived. Continuing is still safe
            // against gross damage (total size, GGUF header and checksum are all
            // verified at the end), so the file is kept.
            return None;
        };
        if meta.url != remote.url {
            return Some(format!(
                "the download source changed from {} to {}",
                meta.url, remote.url
            ));
        }
        if let (Some(known), Some(now)) = (meta.etag.as_deref(), remote.etag.as_deref()) {
            if known != now {
                return Some(
                    "the remote file was re-uploaded since the partial was written".to_string(),
                );
            }
        }
        if let (Some(known), Some(now)) = (meta.total_bytes, remote.size_bytes) {
            if known != now {
                return Some(
                    "the remote file size differs from the one the partial was started against"
                        .to_string(),
                );
            }
        }
        None
    }

    /// A transfer of this exact file that has not finished yet.
    fn active_transfer(&self, final_path: &Path) -> Option<DownloadTask> {
        let wanted = final_path.display().to_string();
        let guard = self.tasks.lock().ok()?;
        guard
            .values()
            .find(|task| task.path == wanted && !task.state.is_terminal())
            .cloned()
    }

    /// Register an on-disk model as complete without touching the network.
    fn complete_from_disk(
        &self,
        model: &ModelDescriptor,
        file_name: &str,
        final_path: &Path,
        size: u64,
        sha256: Option<String>,
    ) -> DownloadTask {
        let mut task = self.build_task(model, file_name, final_path, size);
        task.state = DownloadState::Complete;
        task.total_bytes = Some(size);
        task.percent = 100.0;
        task.finished_ms = Some(now_ms());
        task.sha256 = sha256;
        self.remember(task.clone());
        self.send(DownloadEvent::Completed(DownloadCompleted {
            download_id: task.id.clone(),
            model_id: task.model_id.clone(),
            path: task.path.clone(),
            size_bytes: size,
            sha256: task.sha256.clone(),
            elapsed_ms: 0,
        }));
        task
    }

    /// Ask a running transfer to stop at the next chunk boundary.
    pub fn cancel(&self, id: &str) -> AppResult<()> {
        let cancel = {
            let guard = self
                .cancels
                .lock()
                .map_err(|_| AppError::Internal("download registry poisoned"))?;
            guard
                .get(id)
                .cloned()
                .ok_or_else(|| AppError::InvalidRequest(format!("unknown download id {id}")))?
        };
        cancel.cancel();
        Ok(())
    }

    /// Re-run a failed or cancelled transfer, resuming from the partial file.
    pub async fn retry(&self, id: &str, model: &ModelDescriptor) -> AppResult<DownloadTask> {
        let sha = self.task(id).and_then(|task| task.sha256);
        self.forget(id);
        self.pull(model, sha).await
    }

    /// Drop a terminal record from the task table.
    pub fn forget(&self, id: &str) {
        if let Ok(mut guard) = self.tasks.lock() {
            guard.remove(id);
        }
        if let Ok(mut guard) = self.cancels.lock() {
            guard.remove(id);
        }
    }

    fn build_task(
        &self,
        model: &ModelDescriptor,
        file_name: &str,
        final_path: &Path,
        downloaded_bytes: u64,
    ) -> DownloadTask {
        DownloadTask {
            id: uuid::Uuid::new_v4().to_string(),
            model_id: model.id.clone(),
            display_name: model.display_name.clone(),
            file_name: file_name.to_string(),
            url: model.download_url(),
            path: final_path.display().to_string(),
            total_bytes: None,
            downloaded_bytes,
            bytes_per_second: 0.0,
            percent: 0.0,
            state: DownloadState::Queued,
            error: None,
            started_ms: now_ms(),
            finished_ms: None,
            resumed: false,
            sha256: None,
            attempt: 1,
        }
    }

    fn remember(&self, task: DownloadTask) {
        if let Ok(mut guard) = self.tasks.lock() {
            guard.insert(task.id.clone(), task);
        }
    }

    fn send(&self, event: DownloadEvent) {
        let _ = self.sink.send(event);
    }
}

/// Why a transfer stopped, and whether trying again could plausibly help.
struct Failure {
    error: AppError,
    retryable: bool,
    /// Set when the server asked us to wait (HTTP 429 / `Retry-After`).
    wait: Option<Duration>,
}

impl Failure {
    fn fatal(error: AppError) -> Self {
        Self {
            error,
            retryable: false,
            wait: None,
        }
    }

    fn transient(error: AppError) -> Self {
        Self {
            error,
            retryable: true,
            wait: None,
        }
    }
}

/// Runs one download through to completion on a Tokio worker.
///
/// Bytes always land in a `.part` file next to the destination and are renamed
/// only after the size, the GGUF header and (when known) the checksum have been
/// verified, so an interrupted or corrupted transfer can never enter the library.
struct Runner {
    client: HfClient,
    tasks: Arc<Mutex<HashMap<String, DownloadTask>>>,
    cancels: Arc<Mutex<HashMap<String, CancelToken>>>,
    sink: mpsc::UnboundedSender<DownloadEvent>,
    cancel: CancelToken,
    expected_sha256: Option<String>,
    task: DownloadTask,
    final_path: PathBuf,
    partial_path: PathBuf,
    /// Etag from the metadata probe, recorded on the partial file so a later
    /// resume can send it back as `If-Range`.
    remote_etag: Option<String>,
    /// Pause before the first resume; doubles with each attempt.
    backoff: Duration,
}

impl Runner {
    async fn run(mut self) {
        self.attempt_loop().await;
        if let Ok(mut guard) = self.cancels.lock() {
            guard.remove(&self.task.id);
        }
    }

    async fn attempt_loop(&mut self) {
        let started = Instant::now();
        let mut attempt = 1u32;
        loop {
            if self.cancel.is_cancelled() {
                self.fail_cancelled();
                return;
            }
            self.task.attempt = attempt;
            match self.transfer().await {
                Ok(size) => {
                    self.finish(size, started);
                    return;
                }
                Err(failure) => {
                    let backoff = failure
                        .wait
                        .unwrap_or(self.backoff.saturating_mul(1 << (attempt - 1).min(4)));
                    let resume_ok = failure.retryable
                        && attempt < MAX_ATTEMPTS
                        && backoff <= MAX_WAIT
                        && !self.cancel.is_cancelled();
                    if !resume_ok {
                        self.fail(failure.error);
                        return;
                    }
                    attempt += 1;
                    let reason = failure.error.to_string();
                    tracing::warn!(
                        download_id = %self.task.id,
                        attempt,
                        wait_ms = backoff.as_millis(),
                        "resuming after a transient failure: {reason}"
                    );
                    self.mark_retrying(format!(
                        "{reason} — retrying automatically (attempt {attempt} of {MAX_ATTEMPTS})"
                    ));
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    /// One HTTP attempt: continue the partial file if the server agrees the
    /// remote file has not changed, otherwise start it over.
    ///
    /// Resume is only safe when the answer is 206 for exactly the bytes we have;
    /// a server that ignores `Range` must restart the file, because appending a
    /// whole body to a partial one produces a model that loads as garbage.
    async fn transfer(&mut self) -> Result<u64, Failure> {
        self.set_state(DownloadState::Running, None);
        let offset = std::fs::metadata(&self.partial_path)
            .ok()
            .map(|meta| meta.len())
            .filter(|len| *len > 0);
        // Only claim a conditional resume when the bytes on disk carry an etag;
        // `If-Range` is how the server tells us the export moved underneath us.
        let if_range = offset.and_then(|_| read_partial_meta(&self.partial_path)?.etag);

        let request = self
            .client
            .http()
            .get(&self.task.url)
            .header(reqwest::header::ACCEPT, "application/octet-stream");
        let request = match (&if_range, offset) {
            (Some(etag), Some(offset)) => request
                .header(reqwest::header::RANGE, format!("bytes={offset}-"))
                .header(reqwest::header::IF_RANGE, etag.clone()),
            (None, Some(offset)) => {
                request.header(reqwest::header::RANGE, format!("bytes={offset}-"))
            }
            (_, None) => request,
        };

        let response = request.send().await.map_err(|source| {
            let error = AppError::DownloadFailed(format!("request failed: {source}"));
            if source.is_timeout() || source.is_connect() || source.is_request() {
                Failure::transient(error)
            } else {
                Failure::fatal(error)
            }
        })?;

        let status = response.status();
        let code = status.as_u16();
        if matches!(code, 401 | 403) {
            return Err(Failure::fatal(AppError::DownloadFailed(format!(
                "{} requires Hugging Face consent (HTTP {code}). Accept the model licence on huggingface.co or choose another catalog entry.",
                self.task.display_name
            ))));
        }
        if matches!(code, 404 | 410) {
            return Err(Failure::fatal(AppError::DownloadFailed(format!(
                "Hugging Face has no such file (HTTP {code}): {}. Reload the catalog in case it moved.",
                self.task.url
            ))));
        }
        if code == 416 {
            // Our offset is past the end of the remote file: the export shrank or
            // was replaced. Discard the partial and let the next attempt fetch it
            // whole.
            self.discard_partial("the server rejected the resume offset (HTTP 416)");
            return Err(Failure::transient(AppError::DownloadFailed(
                "the partial file no longer matches the remote export".to_string(),
            )));
        }
        if matches!(code, 429 | 500..=599) {
            let mut failure = Failure::transient(AppError::DownloadFailed(format!(
                "the server is busy right now (HTTP {code}); the download resumes by itself"
            )));
            failure.wait = retry_after(response.headers());
            return Err(failure);
        }
        if !status.is_success() {
            return Err(Failure::fatal(AppError::DownloadFailed(format!(
                "HTTP {code} while downloading {}",
                self.task.url
            ))));
        }

        let headers = response.headers().clone();
        let server_etag = crate::hf::parse_etag(&headers);
        let body_bytes = response.content_length();
        let (mut downloaded, append) = match (code, offset) {
            (206, Some(offset)) => {
                match parse_content_range(&headers) {
                    Some((start, _)) if start != offset => {
                        self.discard_partial("the server resumed from a different offset");
                        return Err(Failure::transient(AppError::DownloadFailed(
                            "the resumed range did not line up with the partial file".to_string(),
                        )));
                    }
                    Some((_, total)) => self.task.total_bytes = Some(total),
                    // A 206 with no Content-Range is malformed but harmless;
                    // trust the reported length instead of discarding good bytes.
                    None => {
                        self.task.total_bytes = body_bytes.map(|len| len.saturating_add(offset));
                    }
                }
                (offset, true)
            }
            (206, None) => {
                return Err(Failure::fatal(AppError::DownloadFailed(
                    "the server sent a partial file for a full request".to_string(),
                )))
            }
            (200, Some(_)) => {
                // Ignoring `Range` is legal, appending a whole file to a partial
                // one is how a resumed download becomes corrupt: restart instead.
                self.discard_partial("the server ignored the resume request (HTTP 200)");
                if body_bytes.is_some() {
                    self.task.total_bytes = body_bytes;
                }
                (0, false)
            }
            (200, None) => {
                if body_bytes.is_some() {
                    self.task.total_bytes = body_bytes;
                }
                (0, true)
            }
            (_, _) => (offset.unwrap_or(0), true),
        };

        if let Some(total) = self.task.total_bytes {
            if downloaded >= total {
                self.discard_partial("the remote file is smaller than the partial on disk");
                return Err(Failure::transient(AppError::DownloadFailed(
                    "the remote file changed size mid-download".to_string(),
                )));
            }
        }

        let file = self.open_partial(append).await?;
        let mut file = tokio::io::BufWriter::with_capacity(64 * 1024, file);
        // Record provenance now: if this attempt is interrupted, the next one can
        // check the remote file still matches before appending to these bytes.
        write_partial_meta(
            &self.partial_path,
            &PartialMeta {
                url: self.task.url.clone(),
                etag: server_etag.or_else(|| self.remote_etag.clone()),
                total_bytes: self.task.total_bytes,
                written_ms: now_ms(),
            },
        );

        let attempt_started = Instant::now();
        let mut window_start = Instant::now();
        let mut window_bytes: u64 = 0;
        let mut last_emit = Duration::ZERO;
        let mut stream = response.bytes_stream();
        let mut failure: Option<Failure> = None;

        while let Some(chunk) = stream.next().await {
            if self.cancel.is_cancelled() {
                self.task.downloaded_bytes = downloaded;
                failure = Some(Failure {
                    error: AppError::DownloadCancelled(self.task.id.clone()),
                    retryable: false,
                    wait: None,
                });
                break;
            }
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(source) => {
                    failure = Some(Failure::transient(AppError::DownloadFailed(format!(
                        "the connection dropped after {downloaded} bytes: {source}"
                    ))));
                    break;
                }
            };
            if let Err(source) = file.write_all(&chunk).await {
                failure = Some(Failure::fatal(AppError::Io(source)));
                break;
            }
            downloaded += chunk.len() as u64;
            window_bytes += chunk.len() as u64;
            self.task.downloaded_bytes = downloaded;
            if let Some(total) = self.task.total_bytes {
                self.task.percent = percent_of(downloaded, total);
            }

            let elapsed = attempt_started.elapsed();
            if elapsed - last_emit >= PROGRESS_INTERVAL {
                last_emit = elapsed;
                let window = window_start.elapsed();
                if window >= SPEED_WINDOW {
                    self.task.bytes_per_second = window_bytes as f64 / window.as_secs_f64();
                    window_start = Instant::now();
                    window_bytes = 0;
                }
                self.sync_task();
            }
        }

        // Flush before propagating anything: bytes that only reached the write
        // buffer would otherwise vanish on a failed attempt, and a resume would
        // restart lower than it needs to.
        let flushed = file
            .flush()
            .await
            .map_err(|source| Failure::fatal(AppError::Io(source)));
        drop(file);
        if let Some(failure) = failure {
            return Err(failure);
        }
        flushed?;

        // Everything below checks what actually landed on disk, so a resumed
        // transfer is validated over its whole length, not just its last leg.
        let on_disk = std::fs::metadata(&self.partial_path)
            .map(|meta| meta.len())
            .map_err(|source| Failure::fatal(AppError::Io(source)))?;

        if let Some(expected) = self.task.total_bytes {
            if on_disk != expected {
                // Keep the partial: the next attempt resumes for the missing tail.
                return Err(Failure::transient(AppError::DownloadFailed(format!(
                    "the transfer stopped at {} of {expected} bytes",
                    on_disk
                ))));
            }
        }

        self.verify_gguf()?;
        self.verify_checksum(on_disk)?;
        Ok(on_disk)
    }

    /// A `.gguf` that does not parse as GGUF never reaches the library, so a CDN
    /// error page or a truncated header cannot masquerade as a downloaded model.
    fn verify_gguf(&self) -> Result<(), Failure> {
        if let Err(source) = GgufHeader::read(&self.partial_path) {
            self.discard_partial("the downloaded file is not a readable GGUF model");
            return Err(Failure::fatal(AppError::GgufParse(format!(
                "{} did not return a GGUF model: {source}",
                self.task.url
            ))));
        }
        Ok(())
    }

    fn verify_checksum(&self, size_bytes: u64) -> Result<(), Failure> {
        let Some(expected) = self.expected_sha256.as_deref().map(str::trim) else {
            return Ok(());
        };
        if expected.is_empty() {
            return Ok(());
        }
        self.set_state(DownloadState::Verifying, None);
        let actual = sha256_of(&self.partial_path).map_err(Failure::fatal)?;
        if !actual.eq_ignore_ascii_case(expected) {
            self.discard_partial("the checksum did not match");
            return Err(Failure::fatal(AppError::DownloadFailed(format!(
                "sha256 mismatch on the {size_bytes} byte file: expected {expected}, got {actual}"
            ))));
        }
        Ok(())
    }

    /// Delete the partial file and its provenance sidecar.
    fn discard_partial(&self, why: &str) {
        tracing::info!(path = %self.partial_path.display(), "discarding partial file: {why}");
        let _ = std::fs::remove_file(&self.partial_path);
        clear_partial_meta(&self.partial_path);
    }

    async fn open_partial(&self, append: bool) -> Result<tokio::fs::File, Failure> {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true);
        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }
        options
            .open(&self.partial_path)
            .await
            .map_err(|source| Failure::fatal(AppError::Io(source)))
    }

    fn finish(&self, size_bytes: u64, started: Instant) {
        self.set_state(DownloadState::Verifying, None);
        // Get the bytes on the platter before publishing the file: a rename that
        // survives a power loss but points at an empty file is worse than an
        // interrupted download, which can be resumed.
        if let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.partial_path)
        {
            let _ = file.sync_all();
        }
        match std::fs::rename(&self.partial_path, &self.final_path) {
            Ok(()) => {
                clear_partial_meta(&self.partial_path);
                let mut completed = self.task.clone();
                completed.state = DownloadState::Complete;
                completed.percent = 100.0;
                completed.downloaded_bytes = size_bytes;
                completed.total_bytes = Some(size_bytes);
                completed.finished_ms = Some(now_ms());
                completed.error = None;
                self.store(completed.clone());
                self.send(DownloadEvent::Progress(completed.progress()));
                self.send(DownloadEvent::Completed(DownloadCompleted {
                    download_id: completed.id,
                    model_id: completed.model_id,
                    path: completed.path,
                    size_bytes,
                    sha256: completed.sha256,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }));
            }
            Err(error) => self.fail(AppError::DownloadFailed(format!(
                "cannot move the download into place: {error}"
            ))),
        }
    }

    fn fail(&self, error: AppError) {
        let message = error.to_string();
        let mut task = self.task.clone();
        task.state = DownloadState::Failed;
        task.error = Some(message.clone());
        task.finished_ms = Some(now_ms());
        self.store(task.clone());
        self.send(DownloadEvent::Progress(task.progress()));
        self.send(DownloadEvent::Failed(DownloadFailedEvent {
            download_id: task.id,
            model_id: task.model_id,
            code: error.code(),
            message,
        }));
    }

    fn fail_cancelled(&self) {
        let mut task = self.task.clone();
        task.state = DownloadState::Cancelled;
        task.finished_ms = Some(now_ms());
        let partial_bytes = task.downloaded_bytes;
        self.store(task.clone());
        self.send(DownloadEvent::Progress(task.progress()));
        self.send(DownloadEvent::Cancelled(DownloadCancelled {
            download_id: task.id,
            model_id: task.model_id,
            partial_bytes,
        }));
    }

    fn set_state(&self, state: DownloadState, error: Option<String>) {
        let mut task = self.task.clone();
        task.state = state;
        task.error = error;
        self.store(task.clone());
        self.send(DownloadEvent::Progress(task.progress()));
    }

    /// Tell the UI a transient failure happened and the resume is already queued.
    fn mark_retrying(&self, notice: String) {
        self.set_state(DownloadState::Retrying, Some(notice));
    }

    fn sync_task(&self) {
        let mut task = self.task.clone();
        task.error = None;
        self.store(task.clone());
        self.send(DownloadEvent::Progress(task.progress()));
    }

    fn store(&self, task: DownloadTask) {
        if let Ok(mut guard) = self.tasks.lock() {
            guard.insert(task.id.clone(), task);
        }
    }

    fn send(&self, event: DownloadEvent) {
        let _ = self.sink.send(event);
    }
}

impl DownloadTask {
    fn progress(&self) -> DownloadProgress {
        DownloadProgress {
            download_id: self.id.clone(),
            model_id: self.model_id.clone(),
            file_name: self.file_name.clone(),
            state: self.state,
            downloaded_bytes: self.downloaded_bytes,
            total_bytes: self.total_bytes,
            percent: self.percent,
            bytes_per_second: self.bytes_per_second,
            error: self.error.clone(),
            attempt: self.attempt,
            max_attempts: MAX_ATTEMPTS,
        }
    }
}

/// What a `.part` file claims to be, so a later resume can check the remote file
/// still matches before appending to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialMeta {
    url: String,
    etag: Option<String>,
    total_bytes: Option<u64>,
    written_ms: u64,
}

/// `model.gguf.part` -> `model.gguf.part.meta.json`, outside the library scan
/// because it does not end in `.gguf`.
fn partial_meta_path(partial_path: &Path) -> PathBuf {
    let mut path = PathBuf::from(partial_path).into_os_string();
    path.push(".meta.json");
    PathBuf::from(path)
}

fn read_partial_meta(partial_path: &Path) -> Option<PartialMeta> {
    let text = std::fs::read_to_string(partial_meta_path(partial_path)).ok()?;
    match serde_json::from_str::<PartialMeta>(&text) {
        Ok(meta) => Some(meta),
        Err(source) => {
            tracing::warn!(path = %partial_path.display(), "ignoring an unreadable download sidecar: {source}");
            None
        }
    }
}

/// Best effort: losing the sidecar costs a resume, never the download itself.
fn write_partial_meta(partial_path: &Path, meta: &PartialMeta) {
    let path = partial_meta_path(partial_path);
    if let Ok(text) = serde_json::to_string(meta) {
        if let Err(source) = std::fs::write(&path, text) {
            tracing::warn!(path = %path.display(), "cannot write the download sidecar: {source}");
        }
    }
}

fn clear_partial_meta(partial_path: &Path) {
    let _ = std::fs::remove_file(partial_meta_path(partial_path));
}

/// `bytes 1024-2047/3000` -> the start offset and the whole-file size.
fn parse_content_range(headers: &reqwest::header::HeaderMap) -> Option<(u64, u64)> {
    let value = headers
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?
        .trim()
        .strip_prefix("bytes ")?;
    let (span, total) = value.split_once('/')?;
    let (start, _) = span.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// A `Retry-After` delta in seconds; other forms fall back to plain backoff.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

/// Hash the file as it sits on disk, which is what makes checksum verification
/// correct after a resumed transfer.
fn sha256_of(path: &Path) -> AppResult<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Catalog sizes are decimal MB; allow a small drift for re-uploaded exports.
pub fn size_matches(actual_bytes: u64, size_mb: u64) -> bool {
    let Some(expected) = expected_bytes(size_mb) else {
        return true;
    };
    let tolerance = expected / 20;
    actual_bytes + tolerance >= expected && actual_bytes.saturating_sub(tolerance) <= expected
}

pub fn expected_bytes(size_mb: u64) -> Option<u64> {
    if size_mb == 0 {
        None
    } else {
        Some(size_mb.saturating_mul(1_000_000))
    }
}

fn percent_of(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
    }
}

pub(crate) fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

/// The name a file is written under while it is still incomplete.
///
/// Library scans and the start-up sweep both skip `*.part`, so an interrupted
/// import or download cannot be mistaken for a model.
pub(crate) fn partial_path_for(final_path: &Path) -> PathBuf {
    let mut path = PathBuf::from(final_path).into_os_string();
    path.push(".part");
    PathBuf::from(path)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
}

/// Free bytes on the volume backing `path`, walking up to an existing ancestor.
pub fn free_space(path: &Path) -> AppResult<u64> {
    let mut cursor = Some(path.to_path_buf());
    while let Some(candidate) = cursor {
        if candidate.is_dir() {
            if let Ok(space) = fs2::available_space(&candidate) {
                return Ok(space);
            }
        }
        cursor = candidate.parent().map(PathBuf::from);
    }
    Err(AppError::Io(std::io::Error::other(
        "cannot determine free disk space",
    )))
}

/// Reject anything that could escape the model directory.
pub fn sanitize_file_name(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidRequest("file name is empty".into()));
    }
    if !trimmed.to_lowercase().ends_with(".gguf") {
        return Err(AppError::InvalidRequest(format!(
            "only .gguf files can be downloaded, got {raw}"
        )));
    }
    if trimmed.contains("..") || trimmed.contains('/') || trimmed.contains('\\') {
        return Err(AppError::InvalidRequest(format!(
            "file name must not contain path separators: {raw}"
        )));
    }
    if trimmed.starts_with('.') {
        return Err(AppError::InvalidRequest(format!(
            "hidden file names are not allowed: {raw}"
        )));
    }
    if trimmed.len() > 200 {
        return Err(AppError::InvalidRequest("file name too long".into()));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ModelDescriptor {
        ModelDescriptor {
            id: "test-model".into(),
            display_name: "Test Model".into(),
            hf_repo: "test/repo".into(),
            filename: "test-q4_k_m.gguf".into(),
            size_mb: 100,
            ..Default::default()
        }
    }

    #[test]
    fn sanitises_file_names() {
        assert_eq!(
            sanitize_file_name(" Qwen2.5-0.5B-Q4_K_M.gguf ").expect("ok"),
            "Qwen2.5-0.5B-Q4_K_M.gguf"
        );
        for bad in [
            "",
            "model.bin",
            "../evil.gguf",
            "sub/dir.gguf",
            "..\\escape.gguf",
            ".hidden.gguf",
            &("a".repeat(210) + ".gguf"),
        ] {
            assert!(
                sanitize_file_name(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn size_matching_tolerates_drift() {
        assert!(expected_bytes(100) == Some(100_000_000));
        assert_eq!(expected_bytes(0), None);
        assert!(size_matches(100_000_000, 100));
        assert!(size_matches(99_000_000, 100));
        assert!(!size_matches(50_000_000, 100));
        // Unknown size: anything on disk counts as complete.
        assert!(size_matches(1, 0));
    }

    #[test]
    fn percent_and_partial_paths() {
        assert_eq!(percent_of(50, 100), 50.0);
        assert_eq!(percent_of(0, 0), 0.0);
        assert_eq!(percent_of(150, 100), 100.0);
        assert_eq!(
            partial_path_for(Path::new("/models/a.gguf")),
            PathBuf::from("/models/a.gguf.part")
        );
    }

    #[test]
    fn states_are_classified() {
        assert!(DownloadState::Complete.is_terminal());
        assert!(DownloadState::Failed.is_terminal());
        assert!(!DownloadState::Running.is_terminal());
    }

    #[test]
    fn events_use_spec_event_names() {
        let progress = DownloadProgress {
            download_id: "d1".into(),
            model_id: "m1".into(),
            file_name: "m.gguf".into(),
            state: DownloadState::Running,
            downloaded_bytes: 1,
            total_bytes: Some(2),
            percent: 50.0,
            bytes_per_second: 1024.0,
            error: None,
            attempt: 1,
            max_attempts: MAX_ATTEMPTS,
        };
        let json = serde_json::to_value(DownloadEvent::Progress(progress)).expect("serialises");
        assert_eq!(json["event"], "download-progress");
        assert_eq!(json["payload"]["downloadId"], "d1");
        assert_eq!(json["payload"]["bytesPerSecond"], 1024.0);

        let failed = DownloadEvent::Failed(DownloadFailedEvent {
            download_id: "d1".into(),
            model_id: "m1".into(),
            code: "download_failed",
            message: "boom".into(),
        });
        let json = serde_json::to_value(failed).expect("serialises");
        assert_eq!(json["event"], "download-error");
        assert_eq!(json["payload"]["code"], "download_failed");
    }

    #[test]
    fn task_progress_projection_matches_task() {
        let manager = test_manager();
        let task = manager.build_task(
            &descriptor(),
            "test-q4_k_m.gguf",
            Path::new("/tmp/test.gguf"),
            42,
        );
        let progress = task.progress();
        assert_eq!(progress.downloaded_bytes, 42);
        assert_eq!(progress.model_id, "test-model");
        assert_eq!(progress.state, DownloadState::Queued);
    }

    #[test]
    fn manager_tracks_tasks_and_cancellation() {
        let manager = test_manager();
        assert!(manager.snapshot().is_empty());

        let task = manager.build_task(
            &descriptor(),
            "test-q4_k_m.gguf",
            Path::new("/tmp/t.gguf"),
            0,
        );
        let id = task.id.clone();
        manager.remember(task);

        // Unknown ids are rejected rather than silently ignored.
        assert!(manager.cancel("nope").is_err());
        let cancel = CancelToken::new();
        manager
            .cancels
            .lock()
            .expect("lock")
            .insert(id.clone(), cancel.clone());
        assert!(manager.cancel(&id).is_ok());
        assert!(cancel.is_cancelled());

        assert_eq!(manager.task(&id).map(|task| task.id), Some(id.clone()));
        assert_eq!(
            manager.task_for_model("test-model").map(|task| task.id),
            Some(id.clone())
        );
        assert_eq!(manager.snapshot().len(), 1);
        manager.forget(&id);
        assert!(manager.task(&id).is_none());
        assert!(manager.snapshot().is_empty());
    }

    fn test_manager() -> DownloadManager {
        let dir = std::env::temp_dir().join(format!("smollm-download-{}", std::process::id()));
        let (sink, _rx) = mpsc::unbounded_channel();
        DownloadManager::new(AppPaths::new(Some(dir)), HfClient::default(), sink)
    }

    #[test]
    fn free_space_is_positive_on_a_real_directory() {
        let space = free_space(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("measured");
        assert!(space > 1_000_000);
    }

    #[test]
    fn free_space_walks_up_from_missing_directories() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("nope/nope-again");
        assert!(free_space(&missing).is_ok());
    }

    #[tokio::test]
    async fn pull_reports_progress_without_network_when_already_complete() {
        let dir = std::env::temp_dir().join(format!(
            "smollm-complete-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("test-q4_k_m.gguf"), vec![0u8; 100_000_000]).expect("file");

        let (sink, mut rx) = mpsc::unbounded_channel();
        let manager = DownloadManager::new(AppPaths::new(Some(dir)), HfClient::default(), sink);
        let task = manager
            .pull(&descriptor(), None)
            .await
            .expect("instant complete");

        assert_eq!(task.state, DownloadState::Complete);
        assert!(matches!(
            rx.recv().await,
            Some(DownloadEvent::Completed(completed)) if completed.model_id == "test-model"
        ));
    }

    // --- transfers over a real socket ----------------------------------------
    //
    // `HfClient::with_endpoint` aims the whole download path at a localhost
    // server, so resume, automatic retry and the integrity checks run exactly as
    // they ship instead of being simulated.

    /// How the fake endpoint should answer.
    #[derive(Clone, Copy)]
    struct Script {
        /// Reply 206 to a `Range` request, as Hugging Face does.
        honours_range: bool,
        /// Kill the first file request after this many body bytes.
        cut_first: Option<usize>,
        /// Refuse, as a gated repository does.
        gated: bool,
        /// Serve HTML of the declared length instead of a model.
        not_gguf: bool,
        etag: &'static str,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                honours_range: true,
                cut_first: None,
                gated: false,
                not_gguf: false,
                etag: "first-etag",
            }
        }
    }

    /// A valid GGUF header padded to `len`, standing in for a model file.
    fn gguf_body(len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes());
        let key = "general.name";
        out.extend_from_slice(&(key.len() as u64).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&8u32.to_le_bytes()); // STRING
        out.extend_from_slice(&4u64.to_le_bytes());
        out.extend_from_slice(b"Test");
        out.resize(len, 0x7f);
        out
    }

    fn local_descriptor() -> ModelDescriptor {
        ModelDescriptor {
            id: "local-model".into(),
            display_name: "Local Test Model".into(),
            hf_repo: "test/repo".into(),
            filename: "test-q4_k_m.gguf".into(),
            revision: "main".into(),
            quantization: "Q4_K_M".into(),
            // Unknown on purpose: the fake endpoint reports the real length.
            size_mb: 0,
            ..Default::default()
        }
    }

    fn response(status: &str, extra: &[String], payload: &[u8], declared: usize) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {status}\r\ncontent-length: {declared}\r\n");
        for line in extra {
            head.push_str(line);
            head.push_str("\r\n");
        }
        // No connection pooling, so every request gets its own accept().
        head.push_str("connection: close\r\n\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(payload);
        bytes
    }

    /// The start a `Range: bytes=N-` header asked for.
    fn requested_start(request: &str) -> Option<u64> {
        for line in request.lines() {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("range") {
                continue;
            }
            let spec = value.trim().strip_prefix("bytes=")?.split('-').next()?;
            return spec.parse().ok();
        }
        None
    }

    /// Answer one request per connection until the test binary exits.
    async fn fake_hf(body: Vec<u8>, script: Script) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds a port");
        let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let full = body.len();

        tokio::spawn(async move {
            let mut gets = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut raw = Vec::new();
                let mut byte = [0u8; 1];
                while let Ok(1) = socket.read(&mut byte).await {
                    raw.push(byte[0]);
                    if raw.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let is_probe = text.starts_with("GET /api/");
                if !is_probe {
                    gets += 1;
                }
                let first_get = !is_probe && gets == 1;
                if let Ok(mut log) = seen.lock() {
                    log.push(text.clone());
                }

                let reply = if is_probe {
                    if script.gated {
                        response("401 Unauthorized", &[], b"", 0)
                    } else {
                        let json = format!(
                            "{{\"size\":{full},\"etag\":\"\\\"{}\\\"\",\"commitHash\":\"c0ffee\"}}",
                            script.etag
                        );
                        response(
                            "200 OK",
                            &["content-type: application/json".to_string()],
                            json.as_bytes(),
                            json.len(),
                        )
                    }
                } else if script.gated {
                    response("401 Unauthorized", &[], b"", 0)
                } else {
                    let start = if script.honours_range {
                        requested_start(&text).filter(|from| *from < full as u64)
                    } else {
                        None
                    };
                    let mut extra = vec![format!("etag: \"{}\"", script.etag)];
                    let (status, mut payload) = match start {
                        Some(from) => {
                            extra.push(format!(
                                "content-range: bytes {}-{}-{full}",
                                from,
                                full - 1
                            ));
                            ("206 Partial Content", body[from as usize..].to_vec())
                        }
                        None => {
                            let payload = if script.not_gguf {
                                vec![b'<'; full]
                            } else {
                                body.clone()
                            };
                            ("200 OK", payload)
                        }
                    };
                    // The full length stays declared even when the body is cut
                    // short, which is exactly what a dropped connection looks like.
                    let declared = payload.len();
                    if first_get {
                        if let Some(cut) = script.cut_first {
                            payload.truncate(cut);
                        }
                    }
                    response(status, &extra, &payload, declared)
                };
                let _ = socket.write_all(&reply).await;
                let _ = socket.shutdown().await;
            }
        });
        (endpoint, requests)
    }

    /// A manager pointed at the fake endpoint, writing into a throwaway directory.
    struct Harness {
        models_dir: PathBuf,
        endpoint: String,
        manager: DownloadManager,
        events: mpsc::UnboundedReceiver<DownloadEvent>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Harness {
        fn descriptor(&self) -> ModelDescriptor {
            local_descriptor()
        }

        fn resolve_url(&self) -> String {
            HfClient::new()
                .with_endpoint(&self.endpoint)
                .resolve_url(&self.descriptor())
        }

        fn final_path(&self) -> PathBuf {
            self.models_dir.join("test-q4_k_m.gguf")
        }

        /// Leave a half-finished download behind, as an interrupted attempt would.
        fn seed_partial(&self, bytes: usize, meta: Option<PartialMeta>) -> PathBuf {
            let partial = partial_path_for(&self.final_path());
            std::fs::write(&partial, vec![0xA5u8; bytes]).expect("partial file");
            match meta {
                Some(meta) => write_partial_meta(&partial, &meta),
                None => clear_partial_meta(&partial),
            }
            partial
        }

        /// Every request the fake endpoint saw, lower-cased for header matching.
        fn log(&self) -> Vec<String> {
            self.requests
                .lock()
                .expect("lock")
                .iter()
                .map(|request| request.to_lowercase())
                .collect()
        }

        async fn settle(&mut self) -> Vec<DownloadEvent> {
            let mut seen = Vec::new();
            loop {
                let next = tokio::time::timeout(Duration::from_secs(20), self.events.recv())
                    .await
                    .expect("the transfer reports a result")
                    .expect("the event channel stays open");
                let terminal = matches!(
                    next,
                    DownloadEvent::Completed(_)
                        | DownloadEvent::Failed(_)
                        | DownloadEvent::Cancelled(_)
                );
                seen.push(next);
                if terminal {
                    return seen;
                }
            }
        }
    }

    async fn harness(body: Vec<u8>, script: Script) -> Harness {
        let (endpoint, requests) = fake_hf(body, script).await;
        let models_dir = std::env::temp_dir().join(format!(
            "smollm-transfer-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        // Created up front so a test can leave a partial file in it before pull.
        std::fs::create_dir_all(&models_dir).expect("model directory");
        let (sink, events) = mpsc::unbounded_channel();
        let mut manager = DownloadManager::new(
            AppPaths::new(Some(models_dir.clone())),
            HfClient::new().with_endpoint(&endpoint),
            sink,
        );
        // Keep the retry path quick without making the test wait seconds.
        manager.backoff = Duration::from_millis(10);
        Harness {
            models_dir,
            endpoint,
            manager,
            events,
            requests,
        }
    }

    fn failure_reason(events: &[DownloadEvent]) -> String {
        events
            .iter()
            .find_map(|event| match event {
                DownloadEvent::Failed(failed) => {
                    Some(format!("{}: {}", failed.code, failed.message))
                }
                _ => None,
            })
            .unwrap_or_else(|| "no failure was reported".to_string())
    }

    fn saw_retry(events: &[DownloadEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                DownloadEvent::Progress(progress) if progress.state == DownloadState::Retrying
            )
        })
    }

    #[tokio::test]
    async fn a_dropped_connection_resumes_where_it_left_off() {
        let body = gguf_body(3_000);
        let mut harness = harness(
            body.clone(),
            Script {
                cut_first: Some(1_200),
                ..Script::default()
            },
        )
        .await;

        harness
            .manager
            .pull(&harness.descriptor(), None)
            .await
            .expect("starts");
        let events = harness.settle().await;

        assert!(
            matches!(events.last(), Some(DownloadEvent::Completed(_))),
            "expected a completion, got {}",
            failure_reason(&events)
        );
        assert!(saw_retry(&events), "the UI is told a retry happened");
        let written = std::fs::read(harness.final_path()).expect("the model reaches the library");
        assert_eq!(written, body, "a resumed file is byte-identical");
        assert!(
            harness
                .log()
                .iter()
                .any(|request| request.contains("range: bytes=")),
            "the retry should ask to continue, not start over"
        );
    }

    #[tokio::test]
    async fn a_server_that_ignores_range_restarts_the_file() {
        let body = gguf_body(3_000);
        let mut harness = harness(
            body.clone(),
            Script {
                honours_range: false,
                ..Script::default()
            },
        )
        .await;
        // A previous attempt got 1 000 bytes; appending a whole file to those
        // bytes is how a resume silently corrupts a model.
        harness.seed_partial(
            1_000,
            Some(PartialMeta {
                url: harness.resolve_url(),
                etag: Some("first-etag".to_string()),
                total_bytes: Some(3_000),
                written_ms: now_ms(),
            }),
        );

        harness
            .manager
            .pull(&harness.descriptor(), None)
            .await
            .expect("starts");
        let events = harness.settle().await;

        assert!(
            matches!(events.last(), Some(DownloadEvent::Completed(_))),
            "{}",
            failure_reason(&events)
        );
        let written = std::fs::read(harness.final_path()).expect("the model reaches the library");
        assert_eq!(written.len(), 3_000, "the old bytes are not kept");
        assert_eq!(written, body);
    }

    #[tokio::test]
    async fn a_reuploaded_export_is_not_spliced_onto_the_old_partial() {
        let body = gguf_body(3_000);
        let mut harness = harness(body.clone(), Script::default()).await;
        harness.seed_partial(
            1_000,
            Some(PartialMeta {
                url: harness.resolve_url(),
                etag: Some("a-stale-etag".to_string()),
                total_bytes: Some(3_000),
                written_ms: now_ms(),
            }),
        );

        harness
            .manager
            .pull(&harness.descriptor(), None)
            .await
            .expect("starts");
        let events = harness.settle().await;
        assert!(
            matches!(events.last(), Some(DownloadEvent::Completed(_))),
            "{}",
            failure_reason(&events)
        );
        assert!(
            !harness.log().iter().any(|r| r.contains("range: bytes")),
            "a partial from another export must be discarded, not continued"
        );
        assert_eq!(std::fs::read(harness.final_path()).expect("file"), body);
    }

    #[tokio::test]
    async fn a_non_gguf_response_never_enters_the_library() {
        let body = gguf_body(3_000);
        let mut harness = harness(
            body,
            Script {
                not_gguf: true,
                ..Script::default()
            },
        )
        .await;

        harness
            .manager
            .pull(&harness.descriptor(), None)
            .await
            .expect("starts");
        let events = harness.settle().await;

        assert!(
            matches!(events.last(), Some(DownloadEvent::Failed(_))),
            "an HTML body must not be accepted as a model"
        );
        assert!(
            failure_reason(&events).contains("GGUF"),
            "{}",
            failure_reason(&events)
        );
        assert!(
            !harness.final_path().exists(),
            "no model file was published"
        );
        assert!(
            !partial_path_for(&harness.final_path()).exists(),
            "the unusable partial is cleaned up"
        );
    }

    #[tokio::test]
    async fn a_gated_repo_fails_before_any_bytes_are_written() {
        let body = gguf_body(3_000);
        let harness = harness(
            body,
            Script {
                gated: true,
                ..Script::default()
            },
        )
        .await;

        let error = harness
            .manager
            .pull(&harness.descriptor(), None)
            .await
            .expect_err("gated models cannot be downloaded");
        assert!(
            error.to_string().contains("consent"),
            "the message must say what to do: {error}"
        );
        let files: Vec<_> = std::fs::read_dir(&harness.models_dir)
            .expect("dir")
            .filter_map(Result::ok)
            .collect();
        assert!(files.is_empty(), "nothing is written for a gated model");
    }

    #[tokio::test]
    async fn a_resumed_file_still_matches_its_checksum() {
        let body = gguf_body(3_000);
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(&body);
            hex::encode(hasher.finalize())
        };
        let mut harness = harness(
            body.clone(),
            Script {
                cut_first: Some(1_200),
                ..Script::default()
            },
        )
        .await;

        harness
            .manager
            .pull(&harness.descriptor(), Some(digest.clone()))
            .await
            .expect("starts");
        let events = harness.settle().await;
        assert!(
            matches!(events.last(), Some(DownloadEvent::Completed(_))),
            "a checksum must cover every byte, not just the resumed tail: {}",
            failure_reason(&events)
        );
        assert_eq!(sha256_of(&harness.final_path()).expect("hashed"), digest);
    }

    #[tokio::test]
    async fn a_wrong_checksum_is_rejected_and_the_file_discarded() {
        let body = gguf_body(3_000);
        let mut harness = harness(body, Script::default()).await;

        harness
            .manager
            .pull(&harness.descriptor(), Some("0".repeat(64)))
            .await
            .expect("starts");
        let events = harness.settle().await;

        assert!(matches!(events.last(), Some(DownloadEvent::Failed(_))));
        assert!(!harness.final_path().exists());
        assert!(!partial_path_for(&harness.final_path()).exists());
    }

    #[test]
    fn a_running_transfer_is_joined_rather_than_duplicated() {
        let manager = test_manager();
        let path = Path::new("/tmp/joined.gguf");
        let mut task = manager.build_task(&descriptor(), "joined.gguf", path, 0);
        task.state = DownloadState::Running;
        let id = task.id.clone();
        manager.remember(task);

        assert_eq!(
            manager.active_transfer(path).map(|found| found.id),
            Some(id),
            "a second Download click must reuse the running transfer"
        );
        assert!(
            manager
                .active_transfer(Path::new("/tmp/other.gguf"))
                .is_none(),
            "another file is unaffected"
        );
    }

    #[test]
    fn retrying_counts_as_active_work() {
        assert!(DownloadState::Retrying.is_active());
        assert!(!DownloadState::Retrying.is_terminal());
        assert!(!DownloadState::Failed.is_active());
    }
}
