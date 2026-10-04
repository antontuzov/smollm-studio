//! SmolLLM Studio's local OpenAI-compatible server.
//!
//! Axum routes plus the start/stop supervision the desktop app needs. The
//! server always shares the app's engine manager, so what the Chat page shows
//! is exactly what `/v1/chat/completions` serves.
//!
//! Safety posture: it binds loopback only. [`serve`] refuses any other host
//! rather than exposing generated text to the network.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use smollm_core::system::{ServerConfig, ServerStatus};
use smollm_core::{AppError, AppResult};

pub mod openai;
pub mod routes;
pub mod state;
pub mod stream;

pub use openai::{ChatCompletionRequest, CompletionRequest, ModelCard, ModelList, OpenAiErrorBody};
pub use routes::{router, ApiError};
pub use state::{ServerState, SharedState};

/// A running server: keep this handle to stop it.
pub struct ServerHandle {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    state: SharedState,
}

impl ServerHandle {
    /// The address actually bound, which differs from the request when port 0
    /// was asked for.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    pub fn host(&self) -> String {
        self.local_addr.ip().to_string()
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host(), self.port())
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    /// Ask the server to stop and wait for it to notice.
    pub async fn stop(self) -> AppResult<()> {
        let url = self.base_url();
        let ServerHandle {
            shutdown,
            task,
            state,
            ..
        } = self;
        if let Some(sender) = shutdown {
            let _ = sender.send(());
        }
        let result = task
            .await
            .map_err(|_| AppError::Internal("server task panicked during shutdown"));
        state.log("info", format!("server stopped on {url}"));
        result
    }

    /// Fire-and-forget stop, for callers that cannot await the join.
    pub fn signal_stop(&mut self) -> bool {
        match self.shutdown.take() {
            Some(sender) => sender.send(()).is_ok(),
            None => false,
        }
    }
}

/// Resolve a validated config onto a concrete socket address.
pub fn socket_for(config: &ServerConfig) -> AppResult<SocketAddr> {
    config.validate()?;
    let host = config.host.trim();
    (host, config.port)
        .to_socket_addrs()
        .map_err(|error| {
            AppError::Io(std::io::Error::other(format!(
                "cannot resolve {host}: {error}"
            )))
        })?
        .next()
        .ok_or_else(|| AppError::InvalidRequest(format!("no address for host `{host}`")))
}

/// Start serving using the config carried by the state. Refuses non-loopback hosts.
pub async fn serve(state: SharedState) -> AppResult<ServerHandle> {
    let addr = socket_for(&state.config)?;
    serve_at(state, addr).await
}

/// Start serving on an exact address. Port 0 is allowed here, which is what the
/// tests and an "choose a free port" setting need.
pub async fn serve_at(state: SharedState, addr: SocketAddr) -> AppResult<ServerHandle> {
    if !addr.ip().is_loopback() {
        return Err(AppError::InvalidRequest(format!(
            "refusing to bind {}: the local server is loopback only",
            addr.ip()
        )));
    }

    let listener = TcpListener::bind(addr).await.map_err(|error| {
        AppError::Io(std::io::Error::other(format!(
            "cannot listen on {addr}: {error}"
        )))
    })?;
    let local_addr = listener.local_addr().map_err(AppError::Io)?;
    let app = router(Arc::clone(&state));

    let (sender, receiver) = oneshot::channel::<()>();
    state.mark_started();
    state.log("info", format!("server listening on http://{local_addr}"));

    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(receiver))
            .await
        {
            tracing::error!("server error: {error}");
        }
    });

    Ok(ServerHandle {
        local_addr,
        shutdown: Some(sender),
        task,
        state,
    })
}

async fn shutdown_signal(receiver: oneshot::Receiver<()>) {
    // A dropped sender (handle cancelled) counts as a shutdown request too.
    let _ = receiver.await;
}

/// Snapshot for the Server page and `get_server_status`.
pub fn status(state: &SharedState, running: bool) -> ServerStatus {
    let (engine, loaded_model, simulated, _) = routes::describe_engine(state);
    ServerStatus {
        running,
        host: state.config.host.clone(),
        port: state.config.port,
        base_url: state.config.base_url(),
        engine,
        loaded_model,
        requests: state.request_count(),
        uptime_seconds: state.uptime_seconds(),
        simulated,
    }
}

/// Model label used in the copyable snippets.
fn model_label(model_id: &str) -> &str {
    if model_id.trim().is_empty() {
        "default"
    } else {
        model_id.trim()
    }
}

/// curl snippet shown on the Server page.
pub fn curl_example(config: &ServerConfig, model_id: &str) -> String {
    format!(
        concat!(
            "curl {base}/v1/chat/completions \\\n",
            "  -H \"Content-Type: application/json\" \\\n",
            "  -d '{{\n",
            "    \"model\": \"{model}\",\n",
            "    \"messages\": [{{\"role\": \"user\", ",
            "\"content\": \"Explain Rust ownership briefly.\"}}],\n",
            "    \"stream\": false\n",
            "  }}'"
        ),
        base = config.base_url(),
        model = model_label(model_id),
    )
}

/// Python OpenAI SDK snippet shown on the Server page.
pub fn python_example(config: &ServerConfig, model_id: &str) -> String {
    format!(
        concat!(
            "from openai import OpenAI\n\n",
            "client = OpenAI(\n",
            "    base_url=\"{base}/v1\",\n",
            "    api_key=\"local\",\n",
            ")\n\n",
            "response = client.chat.completions.create(\n",
            "    model=\"{model}\",\n",
            "    messages=[\n",
            "        {{\"role\": \"user\", ",
            "\"content\": \"Explain Rust ownership briefly.\"}},\n",
            "    ],\n",
            ")\n\n",
            "print(response.choices[0].message.content)\n"
        ),
        base = config.base_url(),
        model = model_label(model_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_engine::EngineManager;
    use std::sync::Mutex;

    fn state(config: ServerConfig) -> SharedState {
        Arc::new(ServerState::new(
            Arc::new(Mutex::new(EngineManager::new())),
            config,
            "0.1.0",
        ))
    }

    #[test]
    fn socket_resolution_is_loopback_only() {
        let addr = socket_for(&ServerConfig::default()).expect("resolves");
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 8080);

        let error = socket_for(&ServerConfig {
            host: "0.0.0.0".to_string(),
            ..ServerConfig::default()
        })
        .expect_err("public host refused");
        assert!(matches!(error, AppError::InvalidRequest(_)));
    }

    #[test]
    fn snippets_are_ready_to_paste() {
        let config = ServerConfig::default();
        let curl = curl_example(&config, "qwen2.5-0.5b-instruct-gguf");
        assert!(
            curl.contains("http://127.0.0.1:8080/v1/chat/completions"),
            "{curl}"
        );
        assert!(curl.contains("\"stream\": false"));
        assert!(curl.contains("qwen2.5-0.5b-instruct-gguf"));
        assert!(
            curl.contains("\\\n"),
            "line continuations must survive copying"
        );

        let python = python_example(&config, "");
        assert!(python.contains("base_url=\"http://127.0.0.1:8080/v1\""));
        assert!(python.contains("api_key=\"local\""));
        assert!(python.contains("model=\"default\""), "no model chosen yet");
        assert!(python.contains("print(response.choices[0].message.content)"));
    }

    #[test]
    fn status_reports_whats_loaded() {
        let state = state(ServerConfig::default());
        let snapshot = status(&state, false);
        assert!(!snapshot.running);
        assert_eq!(snapshot.base_url, "http://127.0.0.1:8080");
        assert_eq!(snapshot.engine, "mock");
        assert!(snapshot.simulated, "mock must be labelled simulated");
        assert!(snapshot.loaded_model.is_none());
    }

    #[tokio::test]
    async fn serve_refuses_a_public_address() {
        let state = state(ServerConfig::default());
        let addr = SocketAddr::from(([192, 168, 1, 20], 8080));
        let error = serve_at(state, addr)
            .await
            .err()
            .expect("non-loopback refused");
        assert!(error.to_string().contains("loopback only"));
    }

    #[tokio::test]
    async fn handle_reports_its_bound_address() {
        let state = state(ServerConfig::default());
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        let handle = serve_at(state, addr).await.expect("binds");
        assert_ne!(handle.port(), 0, "the OS assigns a real port");
        assert!(handle.host() == "127.0.0.1");
        assert_eq!(
            handle.base_url(),
            format!("http://127.0.0.1:{}", handle.port())
        );
        assert!(handle.is_running());
        handle.stop().await.expect("stops cleanly");
    }
}
