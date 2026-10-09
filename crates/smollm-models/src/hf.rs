//! Hugging Face URL resolution and lightweight metadata probing.

use std::sync::{Arc, Mutex};

use reqwest::header::{HeaderMap, ETAG};
use serde::{Deserialize, Serialize};
use smollm_core::error::{AppError, AppResult};
use smollm_core::model::ModelDescriptor;
use smollm_core::secrets::Secret;

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

/// The lower-cased host of an absolute URL.
///
/// A URL that does not parse yields `None`, which means no token rather than a
/// guess: such a request fails on its own, and a credential is not worth a
/// heuristic about where it might be safe.
fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|host| host.to_ascii_lowercase())
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
///
/// The token lives in an `Arc` cell rather than being copied around, because the
/// desktop app keeps one client alive inside the download manager while a second
/// window may save or remove the token at any moment. A clone of this client
/// shares that cell, so no request outlives a token change.
#[derive(Debug, Clone)]
pub struct HfClient {
    http: reqwest::Client,
    endpoint: String,
    token: Arc<Mutex<Option<Secret>>>,
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
            token: Arc::new(Mutex::new(None)),
        }
    }

    /// Point the client at another host, e.g. `http://127.0.0.1:8025`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.trim_end_matches('/').to_string();
        self
    }

    /// Carry a Hugging Face token on requests to this client's host.
    #[must_use]
    pub fn with_token(self, token: Option<Secret>) -> Self {
        self.set_token(token);
        self
    }

    /// Replace the token, so a token saved while a download is queued takes effect
    /// on that download's next request instead of needing a new manager.
    pub fn set_token(&self, token: Option<Secret>) {
        match self.token.lock() {
            Ok(mut guard) => *guard = token,
            Err(_) => {
                tracing::warn!("the token cell is poisoned; requests will go unauthenticated")
            }
        }
    }

    /// The token to send with `url`, or none at all when that URL belongs to
    /// somebody else.
    ///
    /// A bearer token must not follow a request to another host, so the host is
    /// compared rather than assumed: Hugging Face hands downloads to a CDN whose
    /// signed URL needs no credential, and the token stays behind.
    fn bearer_for(&self, url: &str) -> Option<Secret> {
        let token = self.token.lock().ok()?.clone()?;
        let requested = host_of(url)?;
        let ours = host_of(&self.endpoint)?;
        (requested == ours).then_some(token)
    }

    /// A GET that carries the token when — and only when — the URL is ours.
    ///
    /// The transfer loop builds its request through here rather than reaching for
    /// the inner `reqwest::Client`, so there is exactly one place a credential can
    /// be attached.
    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.with_bearer(self.http.get(url), url)
    }

    fn head_request(&self, url: &str) -> reqwest::RequestBuilder {
        self.with_bearer(self.http.head(url), url)
    }

    fn with_bearer(&self, request: reqwest::RequestBuilder, url: &str) -> reqwest::RequestBuilder {
        match self.bearer_for(url) {
            Some(token) => request.bearer_auth(token.expose()),
            None => request,
        }
    }

    /// True when this client would send a token to `url`, whatever it holds.
    #[cfg(test)]
    fn sends_token_to(&self, url: &str) -> bool {
        self.bearer_for(url).is_some()
    }

    /// Whether a token is held at all, used to word a refusal honestly.
    pub fn has_token(&self) -> bool {
        self.token.lock().is_ok_and(|guard| guard.is_some())
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The download URL this client would fetch a model from.
    ///
    /// `{endpoint}/{repo}/resolve/{revision}/{filename}`: the repository comes
    /// before `resolve`, which is what the hub serves. Swapping the two halves
    /// answers `404` for a file that is plainly there.
    pub fn resolve_url(&self, model: &ModelDescriptor) -> String {
        format!(
            "{}/{}/resolve/{}/{}",
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
    ///
    /// That endpoint is not the file's own authority, so any answer that is not
    /// usable metadata becomes a HEAD of the download URL rather than a refusal:
    /// Hugging Face has answered `404` here while still serving the file, and
    /// reading that as "this catalog entry is out of date" blocks downloads that
    /// would otherwise work.
    pub async fn probe(&self, model: &ModelDescriptor) -> AppResult<RemoteFile> {
        let api_url = self.probe_url(model);
        let url = self.resolve_url(model);
        let response = self.get(&api_url).send().await.map_err(|source| {
            AppError::DownloadFailed(format!("cannot reach Hugging Face: {source}"))
        })?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            // A gated repository speaks for itself here: nothing else can tell a
            // licence that has not been accepted from a path the API does not know.
            return Ok(RemoteFile {
                url,
                status,
                ..Default::default()
            });
        }
        let metadata = if response.status().is_success() {
            self.parse_metadata(&api_url, response).await
        } else {
            tracing::warn!(
                "Hugging Face answers HTTP {status} for {api_url}; asking the file itself"
            );
            None
        };
        let Some(metadata) = metadata else {
            return self.head(&url).await;
        };

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

    /// Metadata JSON from a response that already says it is fine. A body that
    /// does not parse is as uninformative as an error status, so it yields
    /// `None` and the caller asks the file itself.
    async fn parse_metadata(
        &self,
        api_url: &str,
        response: reqwest::Response,
    ) -> Option<HfFileMetadata> {
        let text = match response.text().await {
            Ok(text) => text,
            Err(source) => {
                tracing::warn!("cannot read Hugging Face metadata at {api_url}: {source}");
                return None;
            }
        };
        match serde_json::from_str::<HfFileMetadata>(&text) {
            Ok(metadata) => Some(metadata),
            Err(source) => {
                tracing::warn!("Hugging Face metadata at {api_url} is not as expected: {source}");
                None
            }
        }
    }

    /// HEAD the resolve URL; used when the API endpoint is unavailable.
    pub async fn head(&self, url: &str) -> AppResult<RemoteFile> {
        let response =
            self.head_request(url).send().await.map_err(|source| {
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

    /// The client's own builder has to land where the hub serves, because
    /// `https://huggingface.co/resolve/{repo}/…` answers `404` for a file that
    /// exists, and a download starting there reads like a stale catalog entry.
    #[test]
    fn the_client_resolves_where_the_hub_serves() {
        let model = ModelDescriptor {
            hf_repo: "bartowski/SmolLM2-360M-Instruct-GGUF".into(),
            revision: "main".into(),
            filename: "SmolLM2-360M-Instruct-Q4_K_M.gguf".into(),
            ..Default::default()
        };
        assert_eq!(
            HfClient::new().resolve_url(&model),
            model.download_url(),
            "the hub serves a repository first and `resolve` after it"
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

    #[test]
    fn host_names_are_taken_from_the_url_and_lowercased() {
        assert_eq!(
            host_of("https://HuggingFace.co/Qwen/Repo/resolve/main/f.gguf").as_deref(),
            Some("huggingface.co")
        );
        assert_eq!(
            host_of("http://127.0.0.1:8025/x").as_deref(),
            Some("127.0.0.1"),
            "the port is not part of the host a token is allowed to reach"
        );
        assert_eq!(host_of("huggingface.co/x"), None, "a host needs a scheme");
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn a_token_only_travels_to_the_host_it_was_written_for() {
        let client = HfClient::new().with_token(Secret::new("hf_testtoken_1234567890"));
        assert!(client.sends_token_to("https://huggingface.co/api/models/a/b/resolve/main/c.gguf"));
        assert!(
            client.sends_token_to("https://HUGGINGFACE.CO/a/b/c.gguf"),
            "hosts are case-insensitive, so a capitalised mirror is not a reason to withhold"
        );
        assert!(
            !client.sends_token_to("https://cdn-lfs.huggingface.co/blobs/abc"),
            "a signed CDN handoff needs no credential, so it gets none"
        );
        assert!(!client.sends_token_to("https://example.com/huggingface.co/x"));
        assert!(
            !client.sends_token_to("not a url"),
            "an unparsable URL is not a host worth trusting"
        );
    }

    #[test]
    fn a_client_without_a_token_sends_it_nowhere() {
        let client = HfClient::new();
        assert!(!client.sends_token_to("https://huggingface.co/a/b/c.gguf"));
    }

    #[test]
    fn a_token_reaches_clients_that_were_cloned_before_it_existed() {
        // The download manager holds one client and clones it per task, so a
        // token saved after a queue was built still has to apply to it.
        let client = HfClient::new();
        let worker = client.clone();
        client.set_token(Secret::new("hf_shared_cell_1234"));
        assert!(worker.sends_token_to("https://huggingface.co/a/b/c.gguf"));

        client.set_token(None);
        assert!(
            !worker.sends_token_to("https://huggingface.co/a/b/c.gguf"),
            "and removing one is not undone by the older clone"
        );
    }

    #[test]
    fn a_client_does_not_print_its_token() {
        let client = HfClient::new().with_token(Secret::new("hf_testtoken_1234567890"));
        let debug = format!("{client:?}");
        assert!(
            !debug.contains("testtoken"),
            "a `{client:?}` in a tracing call must not carry the credential: {debug}"
        );
    }
}
