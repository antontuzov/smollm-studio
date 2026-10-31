//! The OpenCode app on this Mac, the only route the free-model tier has.
//!
//! OpenCode's free models are served only to OpenCode's own runtime: a request from anywhere else is
//! refused by their gateway. The desktop app this module talks to is that runtime — it keeps a
//! daemon on loopback, writes its address and password to a state file under the user's home, and
//! relays the turn with the handshake the gateway expects. Using it is using the copy the user
//! already installed, not a way around it: the page says the free-model service has to be running, and
//! a turn without it is refused.
//!
//! The password in that file is a local secret. It is read when a request needs it, lives in
//! [`Service`] for the life of that turn, and is never logged, filed, returned over IPC, or written
//! back. The state file is untrusted input: only a loopback address is accepted, because sending that
//! password anywhere else would hand a local credential to whatever the file names.
//!
//! The daemon is started by OpenCode's own CLI when the app opens and wants to send something.
//! Waiting for the user to launch a second app first was the whole failure of a first download: the
//! free rows were there, disabled, with no control that could enable them. [`ensure`] runs the CLI
//! that owns the service when the state file is missing or stale, which is the same thing the
//! desktop app does at its own launch.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use freecode_core::transcript::Hunk;
use freecode_llama::{Reply, ReplyDelta, StreamControl};
use serde::Serialize;
use serde_json::{json, Value};

/// The provider id the local service's own model list uses.
pub const PROVIDER: &str = "opencode";

/// How long a probe may wait. The daemon answers `/api/info` in a millisecond or two on a live
/// machine; this bound only decides how long a status poll waits when it is not there at all.
const PROBE_TIMEOUT: Duration = Duration::from_millis(900);

/// How long the CLI may spend starting the daemon before it is called failed.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to keep looking for the state file the CLI writes once it has been run. The CLI starts
/// a daemon and exits; the file appears within a second of that exit on this machine.
const START_WAIT: Duration = Duration::from_secs(8);

/// Where a user (or a test) points this module at a CLI that is not in any of the usual places.
const CLI_ENV: &str = "FREECODE_OPENCODE_CLI";

/// How long the service may take to acknowledge a prompt before the turn is called failed.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the event stream may stay silent mid-answer. Long enough for a slow first token, short
/// enough that a daemon that died holding the socket is reported rather than waited on forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the wait loop looks at the Stop flag while no event is arriving.
const STOP_POLL: Duration = Duration::from_millis(250);

/// How many times the service may retry a provider call that keeps failing before the turn is called
/// failed. The daemon retries a refused call on its own schedule and announces each attempt on the
/// stream, which keeps the stream alive: without a cap the window waits on a turn that cannot finish,
/// because a quota refusal is the same answer however many times it is asked.
const RETRY_LIMIT: u64 = 3;

/// The title a session opened from here carries in OpenCode's own session list.
const TITLE: &str = "freecode";

/// Whether the last probe found the service. Read synchronously by the status document, which is
/// built on the blocking pool and cannot wait for a probe of its own.
static REACHABLE: AtomicBool = AtomicBool::new(false);

/// What the window learns about the service: that it is there, and which version — never its address
/// beyond loopback, and never its password.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub available: bool,
    pub version: Option<String>,
}

/// A live local service: where it listens and the password its own state file holds.
///
/// Deliberately not `Debug`: a derived one would print the password into any log line that formats a
/// turn, and there is no debugging value in that which is worth the secret.
#[derive(Clone)]
pub struct Service {
    base_url: String,
    password: String,
}

impl Service {
    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }

    fn get(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        client
            .get(format!("{}{}", self.base_url, path))
            .basic_auth("opencode", Some(&self.password))
    }

    fn post(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        client
            .post(format!("{}{}", self.base_url, path))
            .basic_auth("opencode", Some(&self.password))
    }
}

/// Where OpenCode writes the state of its local service.
fn state_file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("opencode").join("service.json"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("opencode")
            .join("service.json"),
    )
}

/// Read the state file, refusing anything that is not a loopback service.
fn parse_state(raw: &str) -> Result<Service, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| format!("the free-model service state is unreadable: {e}"))?;
    let base_url = value
        .get("url")
        .and_then(Value::as_str)
        .ok_or("the free-model service state names no address")?
        .trim_end_matches('/')
        .to_string();
    let password = value
        .get("password")
        .and_then(Value::as_str)
        .ok_or("the free-model service state names no password")?
        .to_string();
    if password.is_empty() {
        return Err("the free-model service state holds an empty password".into());
    }
    // A state file that named a public host would have this app send a local credential to it.
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| base_url.starts_with(prefix));
    if !loopback {
        return Err("the free-model service state does not point at a loopback address".into());
    }
    Ok(Service { base_url, password })
}

/// The service the state file describes, without proving it answers.
pub fn service() -> Result<Service, String> {
    let path = state_file()
        .ok_or("this machine has no home directory to find the free-model service in")?;
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        "the free-model service on this Mac is not running yet, so the free models are not \
         reachable. FreeCode starts it for you; if the rows stay greyed out, install OpenCode from \
         opencode.ai and try again."
            .to_string()
    })?;
    parse_state(&raw)
}

/// The service, proven to answer. `None` is the ordinary "OpenCode is not running" case.
pub async fn ready() -> Option<Service> {
    let service = service().ok()?;
    let answered = match service
        .get(&service.client(), "/api/info")
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    };
    REACHABLE.store(answered, Ordering::Relaxed);
    answered.then_some(service)
}

/// The service, starting it when it is not up yet.
///
/// This is what a free turn and the picker's row state go through. Probing first keeps the ordinary
/// "it is already running" case at one round trip; only a missing daemon pays for the CLI. One
/// caller starts it at a time — the status poll and a send can arrive together — and the others wait
/// on the same state file rather than racing a second daemon.
pub async fn ensure() -> Option<Service> {
    if let Some(service) = ready().await {
        return Some(service);
    }
    if STARTING.swap(true, Ordering::SeqCst) {
        return wait_for_service().await;
    }
    let outcome = match command_line() {
        Some(cli) => start(&cli).await,
        None => Err(
            "OpenCode is not installed on this Mac, and it is the runtime the free models are \
             served to. Install it from opencode.ai, then pick a free model again."
                .to_string(),
        ),
    };
    if let Err(why) = outcome {
        eprintln!("the free-model service did not start: {why}");
    }
    let service = wait_for_service().await;
    STARTING.store(false, Ordering::SeqCst);
    service
}

/// Whether a start is already running, so two callers do not launch two daemons.
static STARTING: AtomicBool = AtomicBool::new(false);

/// Where OpenCode's CLI is normally found, in the order the app looks.
///
/// The desktop application ships the CLI inside its bundle and does not put it on `PATH`, so a user
/// who installed OpenCode the normal way has a working free tier without a terminal.
fn cli_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(CLI_ENV).filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from(
        "/Applications/OpenCode.app/Contents/Resources/opencode-cli",
    ));
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        let home = PathBuf::from(home);
        candidates.push(home.join("Applications/OpenCode.app/Contents/Resources/opencode-cli"));
        candidates.push(home.join(".opencode").join("bin").join("opencode"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("opencode")));
    }
    candidates
}

fn command_line() -> Option<PathBuf> {
    first_file(cli_candidates())
}

fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_file())
}

/// Run the CLI's own `service start`, the command the desktop app runs at launch.
///
/// Its output goes to a file rather than a pipe: the daemon it spawns may inherit the handle, and a
/// piped stdout would then be held open after the CLI has exited, hanging the wait that follows.
async fn start(cli: &Path) -> Result<(), String> {
    let log = std::env::temp_dir().join("freecode-opencode-service.log");
    let sink = std::fs::File::create(&log)
        .map_err(|why| format!("could not open {}: {why}", log.display()))?;
    let status = tokio::time::timeout(
        START_TIMEOUT,
        tokio::process::Command::new(cli)
            .args(["service", "start"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(sink))
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| {
        format!(
            "{} did not finish starting the free-model service within {}s",
            cli.display(),
            START_TIMEOUT.as_secs()
        )
    })?
    .map_err(|why| format!("could not run {}: {why}", cli.display()))?;
    if status.success() {
        return Ok(());
    }
    let detail = std::fs::read_to_string(&log).unwrap_or_default();
    let detail: String = detail
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    Err(if detail.is_empty() {
        format!("{} exited with {status}", cli.display())
    } else {
        format!("{} exited with {status}: {detail}", cli.display())
    })
}

/// Poll until the CLI's state file answers, or the start window closes.
async fn wait_for_service() -> Option<Service> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        if let Some(service) = ready().await {
            return Some(service);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Ask whether the service answers, and answer with its version when it does.
pub async fn status() -> Status {
    let Ok(service) = service() else {
        REACHABLE.store(false, Ordering::Relaxed);
        return Status::default();
    };
    let client = service.client();
    let response = match service
        .get(&client, "/api/info")
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => {
            REACHABLE.store(false, Ordering::Relaxed);
            return Status::default();
        }
    };
    REACHABLE.store(true, Ordering::Relaxed);
    let version = response.json::<Value>().await.ok().and_then(|body| {
        body.get("version")
            .or_else(|| body.get("data").and_then(|d| d.get("version")))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Status {
        available: true,
        version,
    }
}

/// The tools a turn from here may not use.
///
/// The shell, the network and the browser are refused however this session was opened: this app
/// prices those through their own switches, and a free-model turn has no say in what they are set to.
fn denials() -> Value {
    json!(["shell", "bash", "webfetch", "browser"]
        .iter()
        .map(|action| json!({"action": action, "resource": "*", "effect": "deny"}))
        .collect::<Vec<_>>())
}

/// The ruleset for a session bound to a folder the window can show.
///
/// Writes are put to `ask` rather than to `allow`, because `allow` is not confinement: measured
/// against the live service, `{action: "write", resource: "*", effect: "allow"}` wrote a path outside
/// the session's directory, and `{action: "write", resource: "<directory>", …}` — with or without a
/// `/**` suffix — took the write tool away from the model entirely. `ask` keeps the tool and hands
/// the decision to the caller of [`stream_turn`], which answers with the same sandbox rule the app's
/// own tool loop applies. The service then refuses the call itself, and says why, in the words the
/// app sent with the refusal.
fn project_permissions() -> Value {
    let mut ruleset = ["edit", "write", "patch"]
        .iter()
        .map(|action| json!({"action": action, "resource": "*", "effect": "ask"}))
        .collect::<Vec<_>>();
    ruleset.extend(denials().as_array().into_iter().flatten().cloned());
    json!(ruleset)
}

/// The ruleset for a session with no folder to work in: nothing may touch disk, because a change
/// nobody can show is a change nobody can take back.
///
/// The list is the one measured to keep the free tier's handshake intact — denying *every* tool, or
/// the wildcard action, makes OpenCode's gateway refuse the turn, so this names writes and the shell
/// rather than inventing a rule the service does not honour.
fn read_only_permissions() -> Value {
    let mut ruleset = ["edit", "write", "patch"]
        .iter()
        .map(|action| json!({"action": action, "resource": "*", "effect": "deny"}))
        .collect::<Vec<_>>();
    ruleset.extend(denials().as_array().into_iter().flatten().cloned());
    json!(ruleset)
}

/// One file a service call changed, as the patch the service wrote for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// As the service names it: relative to the session's directory for a path inside it.
    pub path: String,
    /// `added`, `modified` or `deleted`, from the service's own field.
    pub status: String,
    pub added: u32,
    pub removed: u32,
    pub hunks: Vec<Hunk>,
}

impl Change {
    /// Whether the file is one this run made rather than one it changed.
    ///
    /// Read off the status and not off the request's action, because measured against the live service
    /// the action is `edit` for both: the same tool writes a new file and rewrites an old one, and its
    /// answer says which happened. A row that promised "created" from the action alone would promise it
    /// of every edit.
    pub fn is_created(&self) -> bool {
        self.status == "added"
    }
}

/// What the service asks the app before one of its calls touches disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: String,
    /// The id of the tool call the request belongs to, which is what closes the row it opens: the
    /// service reports the call's own outcome under this id.
    pub call: Option<String>,
    pub action: String,
    /// The paths the call named. The app decides on these, so they are kept even though a ruleset
    /// that asks over every file in the request answers with the first one.
    pub resources: Vec<String>,
    pub changes: Vec<Change>,
}

/// The app's answer to one [`Request`].
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    /// Refused, with the sentence the service hands back to the model. The service tells the model
    /// the call was refused either way; the words here are what it is told about why.
    Refuse(String),
}

impl Verdict {
    fn decision(&self) -> &'static str {
        match self {
            Self::Allow => "once",
            Self::Refuse(_) => "reject",
        }
    }

    fn message(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::Refuse(why) => Some(why),
        }
    }
}

/// The call a `session.tool.success` or `session.tool.failed` event closed. Its id is the one the
/// permission request named, which is what ties a write the app answered to the outcome it waited for.
fn event_call(event: &Value) -> Option<String> {
    event
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The files a request or a diff entry names, with their patches.
fn parse_changes(value: &Value) -> Vec<Change> {
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| Change {
            path: entry
                .get("file")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            added: entry.get("additions").and_then(Value::as_u64).unwrap_or(0) as u32,
            removed: entry.get("deletions").and_then(Value::as_u64).unwrap_or(0) as u32,
            hunks: hunks_from(
                entry
                    .get("patch")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

/// One hunk of a patch: the numbers its header gave, and the body lines under it.
struct Block {
    /// The hunk's first old-file line, 1-based, as `@@ -a,b +c,d @@` wrote `a`.
    old_start: u32,
    /// How many old lines it spans; the body's ` ` and `-` lines are exactly these.
    old_count: u32,
    body: Vec<String>,
}

impl Block {
    /// The line the change opens on, 1-based in the file as it stood before the write — the number
    /// [`Hunk::at`] carries, and the same rule the app's own patch tool works to. An empty old range
    /// is a pure insertion, so its block opens on the line the insert follows.
    fn opening_line(&self) -> u32 {
        if self.old_count == 0 {
            return self.old_start + 1;
        }
        self.old_start
            + self
                .body
                .iter()
                .take_while(|line| line.starts_with(' '))
                .count() as u32
    }
}

/// Read a patch into the blocks the review draws.
///
/// Both dialects the service writes parse here: `git`'s `diff --git …` headers, and the `Index:`
/// form the permission request carries. Neither header is kept — the file the change belongs to comes
/// from the entry that carried the patch, and quoting header text into a hunk would print a line that
/// is not in the file.
fn hunks_from(patch: &str) -> Vec<Hunk> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            // `@@ -0,0 +1,234 @@` and the `@@ -1 +1 @@` form a one-line range writes without the counts.
            let range = header
                .split_whitespace()
                .next()
                .and_then(|side| side.strip_prefix('-'))
                .unwrap_or_default();
            let (start, count) = range.split_once(',').unwrap_or((range, "1"));
            blocks.push(Block {
                old_start: start.parse().unwrap_or(0),
                old_count: count.parse().unwrap_or(0),
                body: Vec::new(),
            });
            continue;
        }
        let Some(block) = blocks.last_mut() else {
            continue;
        };
        if line.starts_with(['+', '-', ' ']) {
            block.body.push(line.to_string());
        } else if line.is_empty() {
            // A blank context line, stored as the single space a diff should have written it with, so
            // the review's first character is always a prefix it can read a gutter from.
            block.body.push(" ".to_string());
        }
    }
    blocks
        .into_iter()
        .map(|block| Hunk {
            at: block.opening_line(),
            lines: block.body.join("\n"),
        })
        .collect()
}

/// The request a `permission.asked` event carries.
fn permission_request(event: &Value) -> Option<Request> {
    let data = event.get("data")?;
    Some(Request {
        id: data.get("id").and_then(Value::as_str)?.to_string(),
        call: data
            .get("source")
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        action: data
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        resources: data
            .get("resources")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        changes: parse_changes(
            data.get("metadata")
                .and_then(|m| m.get("files"))
                .unwrap_or(&Value::Null),
        ),
    })
}

async fn body_message(response: reqwest::Response) -> String {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&text).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| {
            v.get("message")
                .and_then(Value::as_str)
                .or_else(|| {
                    v.get("data")
                        .and_then(|d| d.get("message"))
                        .and_then(Value::as_str)
                })
                .or_else(|| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                })
                .or_else(|| v.get("error").and_then(Value::as_str))
        })
        .map(str::to_string)
        .unwrap_or_else(|| text.trim().to_string());
    if message.is_empty() {
        format!("the free-model service answered {status}")
    } else {
        message
    }
}

/// Open a session bound to `model`, working in `directory` when one is named.
///
/// The directory is the whole difference between a run this window can follow and one it cannot: the
/// service writes where the session says it works, so binding it to the project folder is what puts
/// the change somewhere the review, the artifact list and the desktop's own opener can reach.
/// The id is what later prompts on this conversation continue.
pub async fn create_session(
    service: &Service,
    model: &str,
    directory: Option<&Path>,
) -> Result<String, String> {
    let mut body = json!({
        "title": TITLE,
        "model": {"id": model, "providerID": PROVIDER},
        "permissions": if directory.is_some() { project_permissions() } else { read_only_permissions() },
    });
    if let Some(dir) = directory {
        body["location"] = json!({ "directory": dir });
    }
    let response = service
        .post(&service.client(), "/api/session")
        .json(&body)
        .timeout(ACCEPT_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the free-model service would not open a session: {e}"))?;
    if !response.status().is_success() {
        return Err(body_message(response).await);
    }
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("the free-model service's session answer was unreadable: {e}"))?;
    value
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the free-model service opened a session without naming it".to_string())
}

/// Answer one permission request.
///
/// A request nobody answers holds the run: the service waits for the decision before the call goes
/// through, and the answer here is the only one it will get from this app. A refusal to deliver it is
/// therefore an error rather than a detail — the turn would otherwise sit until the idle timeout
/// called it a dead service.
pub async fn reply(
    service: &Service,
    session: &str,
    request: &str,
    verdict: &Verdict,
) -> Result<(), String> {
    let body = json!({ "decision": verdict.decision(), "message": verdict.message() });
    let response = service
        .post(
            &service.client(),
            &format!("/api/session/{session}/permission/{request}/reply"),
        )
        .json(&body)
        .timeout(ACCEPT_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the free-model service's request could not be answered: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(body_message(response).await)
    }
}

/// Whether a session the conversation remembers is still one the service holds.
pub async fn session_lives(service: &Service, session: &str) -> Result<bool, String> {
    let response = service
        .get(&service.client(), &format!("/api/session/{session}"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the free-model service could not be asked about the session: {e}"))?;
    Ok(response.status().is_success())
}

/// Point an existing session at the model the picker now names.
pub async fn switch_model(service: &Service, session: &str, model: &str) -> Result<(), String> {
    let body = json!({"model": {"id": model, "providerID": PROVIDER}});
    let response = service
        .post(&service.client(), &format!("/api/session/{session}/model"))
        .json(&body)
        .timeout(ACCEPT_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the free-model service would not switch the session's model: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(body_message(response).await)
    }
}

/// Take back a turn that is still running. Best effort: the answer already streamed is kept either
/// way, and a service that has already finished has nothing to interrupt.
pub async fn interrupt(service: &Service, session: &str) {
    let _ = service
        .post(
            &service.client(),
            &format!("/api/session/{session}/interrupt"),
        )
        .json(&json!({}))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

/// Read one event out of the byte buffer, or `None` when no complete block has arrived.
///
/// A block with no `data:` line is the service's keep-alive comment, and one that will not parse is
/// an event this build does not understand. Both are skipped rather than returned as nothing: a
/// caller that treated them as "no event yet" would leave the event queued behind them until the
/// next chunk happened to arrive.
fn take_event(buffer: &mut Vec<u8>) -> Option<Value> {
    loop {
        let position = buffer.windows(2).position(|pair| pair == b"\n\n")?;
        let block: Vec<u8> = buffer.drain(..position + 2).collect();
        let text = String::from_utf8_lossy(&block);
        let mut data = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.trim_start());
                data.push('\n');
            }
        }
        if data.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(data.trim_end()) {
            return Some(event);
        }
    }
}

/// The session id an event is about, when it names one.
fn event_session(event: &Value) -> Option<&str> {
    event
        .get("data")
        .and_then(|data| data.get("sessionID"))
        .and_then(Value::as_str)
}

/// What one `session.step.ended` counted.
fn step_tokens(event: &Value) -> (Option<u64>, Option<u64>) {
    let tokens = match event.get("data").and_then(|d| d.get("tokens")) {
        Some(tokens) => tokens,
        None => return (None, None),
    };
    let count = |field: &str| tokens.get(field).and_then(Value::as_u64).unwrap_or(0);
    let input = count("input");
    let total = input + count("output") + count("reasoning");
    (Some(input), Some(total))
}

/// The message a failed execution carries, if it names one.
fn event_failure(event: &Value) -> Option<String> {
    let error = event.get("data").and_then(|d| d.get("error"))?;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("the free-model service could not answer this turn");
    Some(message.to_string())
}

/// How many attempts the service says it has made, and the reason it is retrying.
///
/// A retry event is not a failure: the service is asking the provider again. But it is the only place
/// a refusal is named — a quota error never reaches `session.step.failed` — so the turn reads it to
/// decide when asking again is pointless.
fn event_retry(event: &Value) -> Option<(u64, String)> {
    let data = event.get("data")?;
    let attempt = data.get("attempt").and_then(Value::as_u64)?;
    Some((
        attempt,
        event_failure(event)
            .unwrap_or_else(|| "the free-model service could not answer this turn".into()),
    ))
}

/// The service's reason, in words a person can act on.
///
/// A provider that hangs up reaches the app as the transport's own sentence — a socket error with a
/// `fetch()` hint — which names nothing the user can change. That case is said in the app's words;
/// every other reason goes out as the service gave it, because it usually names what was refused.
fn failure_words(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    let hung_up = lower.contains("econnreset")
        || lower.contains("socket connection was closed")
        || lower.contains("socket hang up");
    if hung_up {
        // No model is named here on purpose: measured across three of the registry's free models,
        // the same reset arrived from each, so the pick is not what the user should change.
        return "the free-model service's provider closed the connection before answering; its free \
                capacity may be used up, so try again in a little while"
            .into();
    }
    reason.to_string()
}

/// What became of one write the service asked about.
#[derive(Debug, Clone, PartialEq)]
pub enum Fate {
    /// This app refused it, with the sentence the service then told the model.
    Refused(String),
    /// The call ran, and the service said so under the same id it asked about.
    Applied,
    /// The call this app approved did not land. The change the request promised is not on disk.
    Failed,
}

/// One write, reported to the window when its fate is known: what was asked, and what became of it.
pub struct Write<'a> {
    pub request: &'a Request,
    pub fate: Fate,
}

/// Stream one turn through the local service.
///
/// The event stream is opened before the prompt is posted — the service writes `server.connected`
/// only once it has subscribed — so no delta can be missed between the prompt and the subscription.
/// Only events naming this session are considered; the stream carries every session the app has open.
///
/// `gate` answers every write the service asks about, and `on_write` reports each one's fate once it
/// is known: at the refusal, or at the call's own success or failure. The pair is what lets a run on
/// this route have rows, a pill that moves, and a review that says what changed — see
/// [`project_permissions`] for why the asking, rather than an allow rule, is the confinement.
///
/// The answer is a future rather than a value because it can be a prompt the user answers: a call the
/// dial leaves to the person running it waits for a press, and this stream is read on the same event
/// loop that has to deliver it. A synchronous gate here would park the loop on its own answer and
/// deadlock the run at the first prompt. The request is handed over by value for the same reason —
/// the future outlives the call that built it.
pub async fn stream_turn<Fut>(
    service: &Service,
    session: &str,
    text: &str,
    stop: &AtomicBool,
    mut on_delta: impl FnMut(&[ReplyDelta]) -> StreamControl,
    mut gate: impl FnMut(Request) -> Fut,
    mut on_write: impl FnMut(&Write),
) -> Result<Reply, String>
where
    Fut: Future<Output = Verdict> + Send,
{
    let client = service.client();
    let mut events = service
        .get(&client, "/api/event")
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| format!("the free-model service would not open its event stream: {e}"))?;

    // Wait until the subscription is acknowledged, not merely dialled.
    let mut buffer: Vec<u8> = Vec::new();
    let mut connected = false;
    while !connected {
        let chunk = events
            .chunk()
            .await
            .map_err(|e| format!("the free-model service's event stream stopped: {e}"))?;
        let Some(chunk) = chunk else {
            return Err(
                "the free-model service's event stream closed before the turn began".into(),
            );
        };
        buffer.extend_from_slice(&chunk);
        while let Some(event) = take_event(&mut buffer) {
            if event.get("type").and_then(Value::as_str) == Some("server.connected") {
                connected = true;
            }
        }
    }

    let response = service
        .post(&client, &format!("/api/session/{session}/prompt"))
        .json(&json!({ "text": text }))
        .timeout(ACCEPT_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the free-model service would not take the prompt: {e}"))?;
    if !response.status().is_success() {
        return Err(body_message(response).await);
    }

    let mut answer = String::new();
    let mut reasoning = String::new();
    let mut prompt_tokens = None;
    let mut total_tokens = None;
    let mut finish = None;
    let mut last_event = Instant::now();
    let mut stopped = false;
    // Writes this app answered, waiting on the call's own outcome. Keyed by nothing but that order
    // would pair a write with some other call's report, so the calls are matched by their ids.
    let mut asked: Vec<Request> = Vec::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            interrupt(service, session).await;
            stopped = true;
            break;
        }
        let chunk = tokio::select! {
            chunk = events.chunk() => chunk.map_err(|e| format!("the free-model service's event stream stopped: {e}"))?,
            _ = tokio::time::sleep(STOP_POLL) => {
                if last_event.elapsed() > IDLE_TIMEOUT {
                    return Err(
                        "the free-model service stopped sending events while this turn was running. \
                         It may have quit; start it again and try again."
                            .into(),
                    );
                }
                continue;
            }
        };
        let Some(chunk) = chunk else {
            return Err(
                "the free-model service's event stream closed before the turn finished".into(),
            );
        };
        last_event = Instant::now();
        buffer.extend_from_slice(&chunk);
        let mut done = false;
        while let Some(event) = take_event(&mut buffer) {
            if event_session(&event) != Some(session) {
                continue;
            }
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "session.reasoning.delta" => {
                    let delta = event
                        .get("data")
                        .and_then(|d| d.get("delta"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if !delta.is_empty() {
                        reasoning.push_str(&delta);
                        if on_delta(&[ReplyDelta::Thought(delta)]) == StreamControl::Stop {
                            interrupt(service, session).await;
                            stopped = true;
                            done = true;
                        }
                    }
                }
                "session.text.delta" => {
                    let delta = event
                        .get("data")
                        .and_then(|d| d.get("delta"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if !delta.is_empty() {
                        answer.push_str(&delta);
                        if on_delta(&[ReplyDelta::Answer(delta)]) == StreamControl::Stop {
                            interrupt(service, session).await;
                            stopped = true;
                            done = true;
                        }
                    }
                }
                "session.text.ended" => {
                    // A service that reported the whole block without deltas would otherwise look like
                    // a turn that said nothing. One copy of the text is the rule: only when no delta
                    // landed does the block itself go out.
                    if answer.is_empty() {
                        let whole = event
                            .get("data")
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !whole.is_empty() {
                            answer.push_str(&whole);
                            if on_delta(&[ReplyDelta::Answer(whole)]) == StreamControl::Stop {
                                interrupt(service, session).await;
                                stopped = true;
                                done = true;
                            }
                        }
                    }
                }
                "session.step.ended" => {
                    let (prompt, total) = step_tokens(&event);
                    prompt_tokens = prompt.or(prompt_tokens);
                    total_tokens = total.or(total_tokens);
                    finish = event
                        .get("data")
                        .and_then(|d| d.get("finish"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or(finish);
                }
                "session.step.failed" | "session.execution.failed" => {
                    let why = event_failure(&event).unwrap_or_else(|| {
                        "the free-model service could not answer this turn".into()
                    });
                    return Err(failure_words(&why));
                }
                // The provider refused and the service is asking again. The first attempts may be a
                // blip; past the cap the same answer is coming, so the turn ends with the reason the
                // service gave rather than spinning on a retry schedule the window cannot see.
                "session.retry.scheduled" => {
                    if let Some((attempt, why)) = event_retry(&event) {
                        if attempt >= RETRY_LIMIT {
                            return Err(failure_words(&why));
                        }
                    }
                }
                // The service's own question before a write lands. `ask` is the ruleset that keeps the
                // tool and still confines it, so this arm is where the app applies its sandbox to a
                // route that does not run through its tool loop.
                "permission.asked" => {
                    let Some(request) = permission_request(&event) else {
                        continue;
                    };
                    let verdict = gate(request.clone()).await;
                    reply(service, session, &request.id, &verdict).await?;
                    match verdict {
                        Verdict::Allow => {
                            if request.call.is_none() {
                                // A request that named no call has no outcome event to wait for, so
                                // the approval is the last word this app gets on it. It is reported
                                // rather than dropped: the alternative is a write the window never
                                // mentions.
                                on_write(&Write {
                                    request: &request,
                                    fate: Fate::Applied,
                                });
                            } else {
                                asked.push(request);
                            }
                        }
                        Verdict::Refuse(why) => on_write(&Write {
                            request: &request,
                            fate: Fate::Refused(why),
                        }),
                    }
                }
                "session.tool.success" | "session.tool.failed" => {
                    let Some(call) = event_call(&event) else {
                        continue;
                    };
                    // Only the calls this app answered have rows to close; every other tool the
                    // service runs — its reads, its searches — arrives here unasked and is skipped.
                    let Some(index) = asked.iter().position(|r| r.call.as_deref() == Some(&call))
                    else {
                        continue;
                    };
                    let request = asked.remove(index);
                    let fate = if kind == "session.tool.success" {
                        Fate::Applied
                    } else {
                        Fate::Failed
                    };
                    on_write(&Write {
                        request: &request,
                        fate,
                    });
                }
                "session.execution.succeeded" => done = true,
                _ => {}
            }
            if done {
                break;
            }
        }
        if done {
            break;
        }
    }

    Ok(Reply {
        text: answer,
        reasoning,
        tool_calls: Vec::new(),
        finish_reason: if stopped {
            Some("stopped".to_string())
        } else {
            finish.or_else(|| Some("stop".to_string()))
        },
        total_tokens,
        prompt_tokens,
        timings: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_is_the_first_candidate_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("not-installed");
        let present = dir.path().join("opencode");
        std::fs::write(&present, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            first_file(vec![absent, present.clone()]),
            Some(present.clone())
        );
        assert_eq!(first_file(Vec::new()), None);
    }

    #[test]
    fn the_desktop_app_bundle_is_one_of_the_places_looked_in() {
        let bundle = PathBuf::from("/Applications/OpenCode.app/Contents/Resources/opencode-cli");
        assert!(
            cli_candidates().contains(&bundle),
            "the copy inside OpenCode.app is the one a normal install leaves"
        );
    }

    #[test]
    fn the_state_file_must_point_at_this_machine() {
        let state =
            |url: &str| format!(r#"{{"url":"{url}","password":"local-secret","version":"2.1.3"}}"#);
        assert!(parse_state(&state("http://127.0.0.1:49374")).is_ok());
        assert!(parse_state(&state("http://localhost:49374/")).is_ok());
        assert!(parse_state(&state("https://opencode.ai")).is_err());
        assert!(parse_state(&state("http://192.168.1.20:49374")).is_err());
        assert!(parse_state(r#"{"url":"http://127.0.0.1:1"}"#).is_err());
        assert!(parse_state("not json").is_err());
    }

    #[test]
    fn a_trailing_slash_does_not_double_the_path() {
        let service =
            parse_state(r#"{"url":"http://127.0.0.1:49374/","password":"local-secret"}"#).unwrap();
        assert_eq!(service.base_url, "http://127.0.0.1:49374");
    }

    #[test]
    fn one_event_block_is_one_event() {
        let mut buffer =
            b"data: {\"type\":\"server.connected\"}\n\n: heartbeat\n\ndata: {\"type\":\"second\"}\n\n"
                .to_vec();
        let first = take_event(&mut buffer).unwrap();
        assert_eq!(first["type"], "server.connected");
        let second = take_event(&mut buffer).unwrap();
        assert_eq!(second["type"], "second");
        assert!(take_event(&mut buffer).is_none());
    }

    #[test]
    fn an_incomplete_block_waits_for_its_terminator() {
        let mut buffer = b"data: {\"type\":\"half\"}".to_vec();
        assert!(take_event(&mut buffer).is_none());
        buffer.extend_from_slice(b"\n\n");
        assert_eq!(take_event(&mut buffer).unwrap()["type"], "half");
    }

    #[test]
    fn a_failed_execution_names_the_reason() {
        let event = json!({
            "type": "session.execution.failed",
            "data": {"error": {"type": "provider.auth", "message": "the gateway refused it"}}
        });
        assert_eq!(
            event_failure(&event).as_deref(),
            Some("the gateway refused it")
        );
    }

    #[test]
    fn a_retry_names_its_attempt_and_reason() {
        let event = json!({
            "type": "session.retry.scheduled",
            "data": {
                "attempt": 5,
                "error": {"type": "provider.transport", "message": "ECONNRESET: the gateway hung up"}
            }
        });
        assert_eq!(
            event_retry(&event),
            Some((5, "ECONNRESET: the gateway hung up".to_string()))
        );
    }

    #[test]
    fn a_retry_without_a_reason_still_names_the_attempt() {
        let event = json!({"type": "session.retry.scheduled", "data": {"attempt": 1}});
        let (attempt, why) = event_retry(&event).expect("the event names an attempt");
        assert_eq!(attempt, 1);
        assert!(!why.is_empty());
    }

    #[test]
    fn a_hung_up_provider_is_said_in_words_a_person_can_act_on() {
        let raw = "ECONNRESET: The socket connection was closed unexpectedly. For more \
                   information, pass `verbose: true` in the second argument to fetch()";
        let said = failure_words(raw);
        assert!(!said.contains("fetch()"));
        assert!(said.contains("free capacity"));
        assert_eq!(failure_words("rate limit reached"), "rate limit reached");
    }

    #[test]
    fn tokens_are_the_whole_fill() {
        let event = json!({
            "type": "session.step.ended",
            "data": {"finish": "stop", "tokens": {"input": 10, "output": 4, "reasoning": 6}}
        });
        assert_eq!(step_tokens(&event), (Some(10), Some(20)));
    }

    /// A `permission.asked` as the live service wrote it (OpenCode 2.1.3, measured 2026-10-24): a new
    /// file, whose action is still `edit`, with its patch in the `Index:` dialect.
    const CREATED: &str = r#"{
        "id": "per_16e1f9676001Sf6DFM9Esen6nh",
        "sessionID": "ses_e91e07c0cffezlrISpD50iiT10",
        "action": "edit",
        "resources": ["page.html"],
        "save": ["*"],
        "metadata": {"files": [{
            "file": "page.html",
            "status": "added",
            "additions": 10,
            "deletions": 0,
            "patch": "Index: page.html\n===================================================================\n--- page.html\n+++ page.html\n@@ -0,0 +1,10 @@\n+<!DOCTYPE html>\n+<html lang=\"en\">\n"
        }]},
        "source": {"type": "tool", "messageID": "msg_1", "id": "chatcmpl-tool-8959ad70926794a9"}
    }"#;

    /// The same event for a line changed inside an existing file.
    const EDITED: &str = r#"{
        "id": "per_16e1f9677001g51hroYSehcZvS",
        "sessionID": "ses_e91e07c0cffezlrISpD50iiT10",
        "action": "edit",
        "resources": ["notes.md"],
        "metadata": {"files": [{
            "file": "notes.md",
            "status": "modified",
            "additions": 1,
            "deletions": 1,
            "patch": "Index: notes.md\n===================================================================\n--- notes.md\n+++ notes.md\n@@ -1,3 +1,3 @@\n line one\n-line two\n+LINE TWO EDITED\n line three\n"
        }]},
        "source": {"type": "tool", "id": "chatcmpl-tool-8d0d1099ca06efd1"}
    }"#;

    fn request(raw: &str) -> Request {
        permission_request(&json!({"type": "permission.asked", "data": serde_json::from_str::<Value>(raw).unwrap()}))
            .expect("the event names a request")
    }

    #[test]
    fn a_request_names_its_call_and_its_files() {
        let created = request(CREATED);
        assert_eq!(created.id, "per_16e1f9676001Sf6DFM9Esen6nh");
        assert_eq!(created.action, "edit");
        assert_eq!(created.resources, ["page.html"]);
        // The id the call's own success or failure is reported under, which is what closes the row.
        assert_eq!(
            created.call.as_deref(),
            Some("chatcmpl-tool-8959ad70926794a9")
        );
        assert_eq!(created.changes.len(), 1);
        assert_eq!(created.changes[0].path, "page.html");
        assert_eq!(created.changes[0].added, 10);
        assert_eq!(created.changes[0].removed, 0);
        assert!(created.changes[0].is_created());
        assert!(!request(EDITED).changes[0].is_created());
    }

    #[test]
    fn a_patches_header_becomes_the_line_the_review_numbers_from() {
        // A pure insertion has no old lines, so its block opens on the first line it writes; an edit
        // one line into a file opens past the one line of context the patch carries above it.
        assert_eq!(request(CREATED).changes[0].hunks[0].at, 1);
        assert_eq!(request(EDITED).changes[0].hunks[0].at, 2);
        assert_eq!(
            request(EDITED).changes[0].hunks[0].lines,
            " line one\n-line two\n+LINE TWO EDITED\n line three"
        );
    }

    #[test]
    fn a_git_patch_reads_the_same_way() {
        // The session's diff endpoint writes git's dialect rather than the `Index:` one; both reach
        // the same parser, so a change is reported the same however it was asked about.
        let hunks = hunks_from(
            "diff --git a/extra.txt b/extra.txt\nnew file mode 100644\nindex 0000000..814f4a4\n\
             --- /dev/null\n+++ b/extra.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n",
        );
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].at, 1);
        assert_eq!(hunks[0].lines, "+one\n+two");
    }

    #[test]
    fn asking_keeps_the_write_tool_and_denies_the_rest() {
        fn effects<'a>(ruleset: &'a Value, action: &str) -> Vec<&'a str> {
            ruleset
                .as_array()
                .unwrap()
                .iter()
                .filter(|rule| rule["action"] == action)
                .map(|rule| rule["effect"].as_str().unwrap())
                .collect()
        }
        let project = project_permissions();
        // `ask` is the confinement: `allow` was measured writing outside the session's directory, and
        // a scoped `allow` took the tool away from the model altogether.
        assert_eq!(effects(&project, "edit"), ["ask"]);
        assert_eq!(effects(&project, "write"), ["ask"]);
        assert_eq!(effects(&project, "patch"), ["ask"]);
        for action in ["shell", "bash", "webfetch", "browser"] {
            assert_eq!(effects(&project, action), ["deny"]);
        }
        // A session with no folder to show has no change to report, so it takes nothing.
        assert_eq!(effects(&read_only_permissions(), "edit"), ["deny"]);
    }
}
