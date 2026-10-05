//! Hugging Face URL resolution and lightweight metadata probing.

use reqwest::header::{HeaderMap, ETAG};
use serde::{Deserialize, Serialize};
use smollm_core::error::{AppError, AppResult};
use smollm_core::model::ModelDescriptor;

pub const USER_AGENT: &str = concat!("SmolLLM-Studio/", env!("CARGO_PKG_VERSION"));

/// What a HEAD/metadata round trip tells us about a remote GGUF.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    /// Where the file is downloaded from (never the metadata endpoint).
    pub url: String,
    pub size_bytes: Option<u64>,
    pub etag: Option<String>,
    pub sha256: Option<String>,
    pub accepts_ranges: bool,
    /// Hugging Face commit the blob resolves to (provenance only).
    pub commit: Option<String>,
    /// HTTP status we saw (200/206 = present, 401 = gated, 404 = missing).
    pub status: u16,
}

impl RemoteFile {
    pub fn is_available(&self) -> bool {
        matches!(self.status, 200 | 206 | 302)
    }

    pub fn requires_consent(&self) -> bool {
        matches!(self.status, 401 | 403)
    }
}

/// `https://huggingface.co/{repo}/resolve/{revision}/{filename}`
pub fn resolve_url(repo: &str, revision: &str, filename: &str) -> String {
    format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        repo.trim_matches('/'),
        revision.trim_matches('/'),
        filename.trim_start_matches('/')
    )
}

/// `https://huggingface.co/api/models/{repo}/resolve/{revision}/{file}` -
/// returns JSON with size and sha for the blob.
pub fn metadata_url(repo: &str, revision: &str, filename: &str) -> String {
    format!(
        "https://huggingface.co/api/models/{}/resolve/{}/{}",
        repo.trim_matches('/'),
        revision.trim_matches('/'),
        filename.trim_start_matches('/')
    )
}

fn parse_u64_header(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

pub fn parse_content_length(headers: &HeaderMap) -> Option<u64> {
    parse_u64_header(headers, reqwest::header::CONTENT_LENGTH)
}

pub fn parse_etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .map(|value| value.trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

pub fn parse_accept_ranges(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("bytes"))
}

/// HF returns `{"size": 123, "etag": "\"abc\"", "commitHash": ...}` from the
/// resolve API endpoint; unknown fields are ignored on purpose.
#[derive(Debug, Deserialize)]
struct HfFileMetadata {
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    etag: Option<String>,
    /// The resolve endpoint spells this `commitHash`, so the field is renamed.
    #[serde(default, rename = "commitHash")]
    commit_hash: Option<String>,
}

/// Blocking-free Hugging Face client.
///
/// `endpoint` is normally `https://huggingface.co`; it is a field rather than a
/// constant so tests can drive the whole download path against a localhost
/// server instead of the internet.
#[derive(Debug, Clone)]
pub struct HfClient {
    http: reqwest::Client,
    endpoint: String,
}

impl Default for HfClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HfClient {
    pub fn new() -> Self {
        Self::with_client(
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::limited(10))
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        )
    }

    pub fn with_client(http: reqwest::Client) -> Self {
        Self {
            http,
            endpoint: "https://huggingface.co".to_string(),
        }
    }

    /// Point the client at another host, e.g. `http://127.0.0.1:8025`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.trim_end_matches('/').to_string();
        self
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The download URL this client would fetch a model from.
    pub fn resolve_url(&self, model: &ModelDescriptor) -> String {
        format!(
            "{}/resolve/{}/{}/{}",
            self.endpoint,
            model.hf_repo.trim_matches('/'),
            model.revision.trim_matches('/'),
            model.filename.trim_start_matches('/')
        )
    }

    fn probe_url(&self, model: &ModelDescriptor) -> String {
        format!(
            "{}/api/models/{}/resolve/{}/{}",
            self.endpoint,
            model.hf_repo.trim_matches('/'),
            model.revision.trim_matches('/'),
            model.filename.trim_start_matches('/')
        )
    }

    /// Ask the HF API for the authoritative size/sha of a model file.
    ///
    /// The `url` it reports is always the *download* URL, not the metadata
    /// endpoint it was probed from.
    pub async fn probe(&self, model: &ModelDescriptor) -> AppResult<RemoteFile> {
        let api_url = self.probe_url(model);
        let url = self.resolve_url(model);
        let response = self.http.get(&api_url).send().await.map_err(|source| {
            AppError::DownloadFailed(format!("cannot reach Hugging Face: {source}"))
        })?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return Ok(RemoteFile {
                url,
                status,
                ..Default::default()
            });
        }
        if !response.status().is_success() {
            return Ok(RemoteFile {
                url,
                status,
                ..Default::default()
            });
        }

        let text = response.text().await.map_err(|source| {
            AppError::DownloadFailed(format!("cannot read Hugging Face metadata: {source}"))
        })?;
        let metadata: HfFileMetadata = serde_json::from_str(&text).map_err(|source| {
            AppError::DownloadFailed(format!(
                "unexpected Hugging Face metadata for {api_url}: {source}"
            ))
        })?;

        Ok(RemoteFile {
            size_bytes: metadata
                .size
                .or_else(|| (model.size_mb > 0).then(|| model.size_mb.saturating_mul(1_000_000))),
            etag: metadata.etag.map(|etag| etag.trim_matches('"').to_string()),
            // An optional `sha256` has to come from the catalog or the user:
            // the resolve API reports a commit hash instead of a blob digest.
            sha256: None,
            accepts_ranges: true,
            commit: metadata.commit_hash,
            url,
            status,
        })
    }

    /// HEAD the resolve URL; used when the API endpoint is unavailable.
    pub async fn head(&self, url: &str) -> AppResult<RemoteFile> {
        let response =
            self.http.head(url).send().await.map_err(|source| {
                AppError::DownloadFailed(format!("HEAD {url} failed: {source}"))
            })?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        Ok(RemoteFile {
            url: url.to_string(),
            size_bytes: parse_content_length(&headers),
            etag: parse_etag(&headers),
            sha256: None,
            accepts_ranges: parse_accept_ranges(&headers),
            commit: None,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_urls_are_normalised() {
        assert_eq!(
            resolve_url("Qwen/Qwen2.5-0.5B-Instruct-GGUF", "main", "q4.gguf"),
            "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/q4.gguf"
        );
        // Leading/trailing slashes must not create empty path segments.
        assert_eq!(
            resolve_url("/bartowski/Repo/", "/main/", "/file.gguf"),
            "https://huggingface.co/bartowski/Repo/resolve/main/file.gguf"
        );
        assert_eq!(
            metadata_url("a/b", "main", "c.gguf"),
            "https://huggingface.co/api/models/a/b/resolve/main/c.gguf"
        );
    }

    #[test]
    fn header_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            "491400032".parse().expect("valid"),
        );
        headers.insert(ETAG, "\"abc123\"".parse().expect("valid"));
        headers.insert(
            reqwest::header::ACCEPT_RANGES,
            "bytes".parse().expect("valid"),
        );
        assert_eq!(parse_content_length(&headers), Some(491_400_032));
        assert_eq!(parse_etag(&headers).as_deref(), Some("abc123"));
        assert!(parse_accept_ranges(&headers));

        let empty = HeaderMap::new();
        assert_eq!(parse_content_length(&empty), None);
        assert_eq!(parse_etag(&empty), None);
        assert!(!parse_accept_ranges(&empty));

        let mut nonsense = HeaderMap::new();
        nonsense.insert(
            reqwest::header::CONTENT_LENGTH,
            "not-a-number".parse().expect("valid"),
        );
        nonsense.insert(ETAG, "\"\"".parse().expect("valid"));
        nonsense.insert(
            reqwest::header::ACCEPT_RANGES,
            "none".parse().expect("valid"),
        );
        assert_eq!(parse_content_length(&nonsense), None);
        assert_eq!(parse_etag(&nonsense), None);
        assert!(!parse_accept_ranges(&nonsense));
    }

    #[test]
    fn availability_reflects_status() {
        assert!(RemoteFile {
            status: 200,
            ..Default::default()
        }
        .is_available());
        assert!(RemoteFile {
            status: 401,
            ..Default::default()
        }
        .requires_consent());
        assert!(!RemoteFile {
            status: 404,
            ..Default::default()
        }
        .is_available());
    }

    #[test]
    fn parses_hf_metadata_json() {
        let parsed: HfFileMetadata =
            serde_json::from_str(r#"{"size":491400032,"etag":"\"deadbeef\"","commitHash":"abc"}"#)
                .expect("hf shape");
        assert_eq!(parsed.size, Some(491_400_032));
        assert_eq!(parsed.etag.as_deref(), Some("\"deadbeef\""));
        assert_eq!(parsed.commit_hash.as_deref(), Some("abc"));
    }
}
