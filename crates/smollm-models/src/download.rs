//! Resumable Hugging Face downloads with progress, cancellation and retry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smollm_core::chat::CancelToken;
use smollm_core::error::{AppError, AppResult};
use smollm_core::model::ModelDescriptor;
use smollm_core::paths::AppPaths;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::hf::HfClient;

/// Emit progress at most this often; keeps the event bus quiet.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// Speed is averaged over this window so the UI number does not jitter.
const SPEED_WINDOW: Duration = Duration::from_millis(1_000);
/// Extra headroom demanded beyond the model size itself.
const DISK_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Running,
    Verifying,
    Complete,
    Cancelled,
    Failed,
}

impl DownloadState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled | Self::Failed)
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
    pub async fn pull(
        &self,
        model: &ModelDescriptor,
        expected_sha256: Option<String>,
    ) -> AppResult<DownloadTask> {
        let file_name = sanitize_file_name(&model.filename)?;
        self.paths.ensure()?;

        let final_path = self.paths.models_dir.join(&file_name);
        let partial_path = partial_path_for(&final_path);
        let on_disk = std::fs::metadata(&final_path).ok().map(|meta| meta.len());

        if let Some(size) = on_disk {
            if size_matches(size, model.size_mb) {
                let mut task = self.build_task(model, &file_name, &final_path, size);
                task.state = DownloadState::Complete;
                task.total_bytes = Some(size);
                task.percent = 100.0;
                task.finished_ms = Some(now_ms());
                task.sha256 = expected_sha256;
                self.remember(task.clone());
                self.send(DownloadEvent::Completed(DownloadCompleted {
                    download_id: task.id.clone(),
                    model_id: task.model_id.clone(),
                    path: task.path.clone(),
                    size_bytes: size,
                    sha256: task.sha256.clone(),
                    elapsed_ms: 0,
                }));
                return Ok(task);
            }
        }

        let mut resume_from = std::fs::metadata(&partial_path).ok().map(|meta| meta.len());
        if let (Some(offset), Some(total)) = (resume_from, expected_bytes(model.size_mb)) {
            if offset >= total {
                // Stale partial file from a re-uploaded export: start over.
                let _ = std::fs::remove_file(&partial_path);
                resume_from = None;
            }
        }

        let already_have = resume_from.unwrap_or(0);
        let required = expected_bytes(model.size_mb)
            .unwrap_or(0)
            .saturating_sub(already_have);
        let free = free_space(&self.paths.models_dir)?;
        if required > 0 && free < required.saturating_add(DISK_HEADROOM_BYTES) {
            return Err(AppError::InsufficientDiskSpace {
                required_gb: gb(required),
                available_gb: gb(free),
            });
        }

        let mut task = self.build_task(model, &file_name, &final_path, already_have);
        task.total_bytes = expected_bytes(model.size_mb);
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
        };
        tokio::spawn(async move { runner.run().await });

        Ok(task)
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

/// Runs one transfer to completion on a Tokio worker.
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
}

impl Runner {
    async fn run(mut self) {
        self.set_state(DownloadState::Running, None);
        let started = Instant::now();
        let offset = std::fs::metadata(&self.partial_path)
            .ok()
            .map(|meta| meta.len());

        let request = self
            .client
            .http()
            .get(&self.task.url)
            .header(reqwest::header::ACCEPT, "application/octet-stream");
        let request = match offset {
            Some(offset) => request.header(reqwest::header::RANGE, format!("bytes={offset}-")),
            None => request,
        };

        let outcome = self.transfer(request, offset, started).await;
        if let Ok(mut guard) = self.cancels.lock() {
            guard.remove(&self.task.id);
        }

        match outcome {
            Ok(size) => self.finish(size, started),
            Err(AppError::DownloadCancelled(_)) => self.fail_cancelled(),
            Err(error) => self.fail(error),
        }
    }

    async fn transfer(
        &mut self,
        request: reqwest::RequestBuilder,
        offset: Option<u64>,
        started: Instant,
    ) -> Result<u64, AppError> {
        let response = request
            .send()
            .await
            .map_err(|source| AppError::DownloadFailed(format!("request failed: {source}")))?;

        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(AppError::DownloadFailed(format!(
                "{} requires Hugging Face consent (HTTP {}). Accept the model licence on huggingface.co or choose another catalog entry.",
                self.task.display_name,
                status.as_u16()
            )));
        }
        if status.as_u16() == 416 {
            let _ = std::fs::remove_file(&self.partial_path);
            return Err(AppError::DownloadFailed(
                "the partial file no longer matches the remote export; retry to download from the start"
                    .to_string(),
            ));
        }
        if !status.is_success() {
            return Err(AppError::DownloadFailed(format!(
                "HTTP {} while downloading {}",
                status.as_u16(),
                self.task.url
            )));
        }

        let remaining = response.content_length();
        let total = remaining.map(|value| value + offset.unwrap_or(0));
        if let Some(total) = total {
            self.task.total_bytes = Some(total);
        }

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.partial_path)
            .await?;

        let mut hasher = Sha256::new();
        let mut downloaded = offset.unwrap_or(0);
        let mut window_start = Instant::now();
        let mut window_bytes: u64 = 0;
        let mut last_emit = Duration::ZERO;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            if self.cancel.is_cancelled() {
                file.flush().await?;
                self.task.downloaded_bytes = downloaded;
                return Err(AppError::DownloadCancelled(self.task.id.clone()));
            }
            let chunk = chunk.map_err(|source| AppError::DownloadFailed(source.to_string()))?;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            window_bytes += chunk.len() as u64;
            self.task.downloaded_bytes = downloaded;
            if let Some(total) = self.task.total_bytes {
                self.task.percent = percent_of(downloaded, total);
            }

            let elapsed = started.elapsed();
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

        file.flush().await?;
        drop(file);

        if let Some(expected) = self.task.total_bytes {
            if downloaded != expected {
                return Err(AppError::DownloadFailed(format!(
                    "size mismatch: received {downloaded} bytes, expected {expected}"
                )));
            }
        }

        if let Some(expected_sha) = self.expected_sha256.as_deref().map(str::trim) {
            if !expected_sha.is_empty() {
                let actual = hex::encode(hasher.finalize());
                if !actual.eq_ignore_ascii_case(expected_sha) {
                    let _ = std::fs::remove_file(&self.partial_path);
                    return Err(AppError::DownloadFailed(format!(
                        "sha256 mismatch: expected {expected_sha}, got {actual}"
                    )));
                }
            }
        }

        Ok(downloaded)
    }

    fn finish(&self, size_bytes: u64, started: Instant) {
        self.set_state(DownloadState::Verifying, None);
        match std::fs::rename(&self.partial_path, &self.final_path) {
            Ok(()) => {
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
        }
    }
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

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

fn partial_path_for(final_path: &Path) -> PathBuf {
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
}
