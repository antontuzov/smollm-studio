//! The tool registry: one description of every capability the agent may reach for, with the
//! permission class that gates it and the JSON schema the model is handed.
//!
//! What lives here is the contract every executor obeys — a tool cannot exist unless it has an id, a
//! schema, and a declared permission class, because both the approval engine and the sandbox make
//! their decisions from those two facts alone. Only [`EXECUTABLE`] of them have an executor yet, in
//! [`crate::exec`]; the rest wait on the patch and git halves of the loop.
//!
//! One detail from `KNOWN_ISSUES.md` is baked into [`ToolCall::arguments_wire`]: this model emits tool
//! calls whose `arguments` are empty or non-JSON, and the fork's server answers those with HTTP 500
//! (issue #112), so callers must normalize an empty argument object to `"{}"` before replaying a call.

use freecode_config::agent::{shell_governed, PermissionClass};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub id: &'static str,
    /// Human label for the transcript row ("Read file", "Terminal command ran").
    pub title: &'static str,
    pub description: &'static str,
    pub permission: PermissionClass,
    /// Only reachable when the model may touch the network — which requires an explicitly
    /// configured remote provider, never a silent default.
    pub requires_network: bool,
    pub input_schema: Value,
}

impl ToolDescriptor {
    pub fn is_read_only(&self) -> bool {
        self.permission == PermissionClass::ReadOnly
    }
}

/// A request to run a tool, as parsed out of the model's stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub tool_id: String,
    pub arguments: Value,
}

impl ToolCall {
    /// The wire form this engine requires: `arguments` must always be a JSON object string, and
    /// `""` or `null` makes the fork answer HTTP 500 (llama.cpp#112).
    pub fn arguments_wire(&self) -> String {
        if self.arguments.is_null() {
            "{}".to_string()
        } else if self.arguments.is_object() {
            self.arguments.to_string()
        } else {
            // A scalar sneaking through is wrapped rather than sent, so the call cannot 500.
            json!({ "value": self.arguments }).to_string()
        }
    }
}

/// Everything this build can do when a run is allowed to act, in the order the model sees it.
///
/// The registry is the offer, not a catalog of intentions: every entry here reaches a runner, either
/// an executor in [`crate::exec`] or the loop's own delegation. A surface that lists capabilities can
/// print this and be accurate.
pub fn registry() -> Vec<ToolDescriptor> {
    use PermissionClass::{Network, ReadOnly, SideEffect, WorkspaceWrite};

    let s = |props: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        })
    };

    vec![
        ToolDescriptor {
            id: "read_file",
            title: "Read file",
            description: "Read a text file inside the workspace, optionally a line range.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({
                    "path": { "type": "string", "description": "Workspace-relative path" },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 }
                }),
                &["path"],
            ),
        },
        ToolDescriptor {
            id: "list_dir",
            title: "List directory",
            description: "List the entries of a directory inside the workspace.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({
                    "path": { "type": "string" },
                    "include_hidden": { "type": "boolean" }
                }),
                &["path"],
            ),
        },
        ToolDescriptor {
            id: "glob",
            title: "Find files",
            description: "Match file paths against a glob pattern inside the workspace.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({ "pattern": { "type": "string" }, "path": { "type": "string" } }),
                &["pattern"],
            ),
        },
        ToolDescriptor {
            id: "grep_search",
            title: "Search content",
            description: "Search file contents by regular expression, with line numbers.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "ignore_case": { "type": "boolean" },
                    "context_lines": { "type": "integer", "minimum": 0, "maximum": 20 }
                }),
                &["pattern"],
            ),
        },
        ToolDescriptor {
            id: "todo_write",
            title: "Update plan",
            description: "Record the plan for this task as a short list of steps and their state. \
                Call it before starting work that needs more than one call, and again whenever a \
                step changes: pending while it waits, active while it is the one being worked on, \
                done once its result is verified. It changes nothing on disk — the list is what the \
                user reads to see where the run is.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({
                    "items": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "active", "done"]
                                }
                            },
                            "required": ["text", "status"],
                            "additionalProperties": false
                        }
                    }
                }),
                &["items"],
            ),
        },
        ToolDescriptor {
            id: "write_file",
            title: "Create file",
            description: "Create a file inside the workspace with the given contents, making the \
                directories its path names. It only creates: a path that already holds a file is \
                refused, so an existing file is changed with edit_file and never overwritten whole.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({
                    "path": { "type": "string" },
                    "contents": { "type": "string" }
                }),
                &["path", "contents"],
            ),
        },
        ToolDescriptor {
            id: "edit_file",
            title: "Edit file",
            description:
                "Replace an exact string in a workspace file; fails if the match is ambiguous.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                }),
                &["path", "old_string", "new_string"],
            ),
        },
        ToolDescriptor {
            id: "apply_patch",
            title: "Apply patch",
            description: "Apply a unified diff to files that already exist in the workspace. Each \
                file's --- / +++ header opens its section, and one call may patch several files. \
                Every hunk's original lines must match its file exactly, and every file is checked \
                before anything is written: the whole patch applies or none of it does. Creating, \
                deleting and renaming a file are not this call.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(json!({ "patch": { "type": "string" } }), &["patch"]),
        },
        ToolDescriptor {
            id: "terminal",
            title: "Terminal command ran",
            description: "Run one shell command with the workspace as its starting directory. Only \
                offered while Terminal access is switched on in Settings. The command runs with the \
                user's own permissions, so it is not confined to the workspace; it is stopped at the \
                timeout, and only the tail of its output comes back. Use it to build, run or test what \
                this workspace contains.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({
                    "command": { "type": "string" },
                    "cwd": { "type": "string", "description": "Workspace-relative directory" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600 }
                }),
                &["command"],
            ),
        },
        ToolDescriptor {
            id: "run_tests",
            title: "Run tests",
            description: "Run the workspace's test command and report failures.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({
                    "filter": { "type": "string" },
                    "target": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } }
                }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "run_build",
            title: "Build project",
            description: "Run the workspace's build and surface compiler errors.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({ "target": { "type": "string" }, "release": { "type": "boolean" } }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "run_linter",
            title: "Run linter",
            description: "Run lint and type checks, returning machine-readable diagnostics.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({ "paths": { "type": "array", "items": { "type": "string" } } }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "git_status",
            title: "Git status",
            description: "Show working-tree status for the workspace repository.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(json!({}), &[]),
        },
        ToolDescriptor {
            id: "git_diff",
            title: "Git diff",
            description: "Show the staged or unstaged diff, with per-file +N −M counts.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({ "staged": { "type": "boolean" }, "path": { "type": "string" } }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "git_log",
            title: "Git log",
            description: "List recent commits, optionally scoped to a path.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({ "limit": { "type": "integer", "maximum": 200 }, "path": { "type": "string" } }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "git_stage",
            title: "Stage changes",
            description: "Stage the named paths for the next commit. The paths have to be inside the \
                workspace; nothing else is staged, and a path that is not there is refused rather \
                than matched loosely.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({ "paths": { "type": "array", "items": { "type": "string" } } }),
                &["paths"],
            ),
        },
        ToolDescriptor {
            id: "git_commit",
            title: "Commit changes",
            description: "Commit the staged changes with the given message. Always requires approval \
                outside Auto approve, and refuses a message that is empty. An amend rewrites the \
                last commit rather than adding one.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({ "message": { "type": "string" }, "amend": { "type": "boolean" } }),
                &["message"],
            ),
        },
        ToolDescriptor {
            id: "git_branch",
            title: "Switch branch",
            description: "Switch to a branch, creating it from the current one when it does not \
                exist yet. The uncommitted changes travel with the switch.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({ "name": { "type": "string" }, "start_point": { "type": "string" } }),
                &["name"],
            ),
        },
        ToolDescriptor {
            id: "git_push",
            title: "Push branch",
            description:
                "Push the current branch to its remote, with whatever credential this machine \
                 already holds for that remote. It leaves the machine, so it needs the Privacy \
                 switch on and an approval of its own.",
            permission: SideEffect,
            requires_network: true,
            input_schema: s(
                json!({ "remote": { "type": "string" }, "branch": { "type": "string" } }),
                &[],
            ),
        },
        ToolDescriptor {
            id: "github_pr",
            title: "Open a pull request",
            description:
                "Open a pull request on GitHub. The repository and the branches are read from the \
                 local checkout and the remote, so they only have to be named when they differ. \
                 Needs a GitHub token saved in the Keychain; without one the call says so rather \
                 than failing quietly.",
            permission: SideEffect,
            requires_network: true,
            input_schema: s(
                json!({
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "head": { "type": "string" },
                    "base": { "type": "string" },
                    "repo": { "type": "string" },
                    "draft": { "type": "boolean" },
                }),
                &["title"],
            ),
        },
        ToolDescriptor {
            id: "fetch_url",
            title: "Read web page",
            description: "Read one web page by its full http(s) address and return its text, with the \
                markup taken out. Public pages only: an address on this machine, a file path, or a \
                non-text file is refused. The result is cut to max_chars, which says so when it is.",
            permission: Network,
            requires_network: true,
            input_schema: s(
                json!({ "url": { "type": "string", "format": "uri" }, "max_chars": { "type": "integer" } }),
                &["url"],
            ),
        },
        ToolDescriptor {
            id: "web_search",
            title: "Search the web",
            description: "Search Wikipedia and return each hit's title, one line about it, and the \
                address of its page. It answers what a thing is or who someone was; it does not answer \
                what happened today, a price, or anything in a niche field. Use fewer, plainer words \
                rather than a whole question, and read a hit in full with fetch_url.",
            permission: Network,
            requires_network: true,
            input_schema: s(
                json!({ "query": { "type": "string" }, "limit": { "type": "integer", "minimum": 1, "maximum": 8 } }),
                &["query"],
            ),
        },
        // The seven calls that drive the page the window's browser panel is showing. They read and
        // act inside the run's own document rather than on disk or the network, which is why the
        // first is confined to an HTML file in the workspace and the rest take nothing but a
        // selector, text or a key name.
        ToolDescriptor {
            id: "browser_open",
            title: "Open in browser",
            description: "Show an HTML file from this workspace in the window's browser panel, where \
                the browser calls can drive it. The other browser calls act on whatever page the \
                panel is showing, so this is how a page is put there.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({ "path": { "type": "string", "description": "Workspace-relative path" } }),
                &["path"],
            ),
        },
        ToolDescriptor {
            id: "browser_read",
            title: "Read the page",
            description: "Read the text the page in the browser panel is showing, with its title and \
                address. Pass selector for one element instead of the whole page. This is the rendered \
                page, so it shows what the page's own script wrote, not what its file says.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(json!({ "selector": { "type": "string" } }), &[]),
        },
        ToolDescriptor {
            id: "browser_click",
            title: "Click on the page",
            description: "Click the first element matching a CSS selector in the browser panel. x and y, \
                when given, are pixel offsets inside the element's box, which is how a canvas or a game \
                field is clicked at a particular spot; otherwise the click lands at its centre.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({
                    "selector": { "type": "string" },
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                }),
                &["selector"],
            ),
        },
        ToolDescriptor {
            id: "browser_type",
            title: "Type into the page",
            description: "Put text into the first element matching a CSS selector, the way typing does \
                it: the field is focused, its value replaced, and input and change events sent. Without \
                a selector the focused element takes it. submit presses Enter afterwards.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({
                    "selector": { "type": "string" },
                    "text": { "type": "string" },
                    "submit": { "type": "boolean" }
                }),
                &["text"],
            ),
        },
        ToolDescriptor {
            id: "browser_press",
            title: "Press a key",
            description: "Send one key press to the page, as a player's keyboard does: keydown, keypress \
                where it applies, then keyup. Names are KeyboardEvent.key names — ArrowLeft, Space, \
                Enter, Escape, a, 1.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(json!({ "key": { "type": "string" } }), &["key"]),
        },
        ToolDescriptor {
            id: "browser_wait",
            title: "Wait for the page",
            description: "Wait until an element matching a CSS selector appears in the browser panel, up \
                to timeout_ms (3000 by default, 15000 at most), and say how long it took — or that it \
                never appeared, with the text the page was showing.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(
                json!({
                    "selector": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 15000 }
                }),
                &["selector"],
            ),
        },
        ToolDescriptor {
            id: "browser_console",
            title: "Read the console",
            description: "Read the errors and warnings the page in the browser panel has logged since it \
                loaded, uncaught exceptions included. This is where a page that draws nothing says why.",
            permission: ReadOnly,
            requires_network: false,
            input_schema: s(json!({}), &[]),
        },
        ToolDescriptor {
            id: "browser_capture",
            title: "Capture the page",
            description: "Save what the page in the browser panel looks like as a PNG and answer where \
                the file is kept. The text calls read what the page says; this is what it draws, which \
                is the only way to see that a game rendered its field or that a layout put things where \
                they belong. One capture is the page as the panel shows it right now.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(json!({}), &[]),
        },
        ToolDescriptor {
            id: "browser_check",
            title: "Check the page's picture",
            description: "Capture the page and assert on its pixels without a model looking at them: \
                that the page drew anything at all (expect \"not-blank\"), or that one point of it is \
                the colour it should be (expect \"colour\" with x, y and rgb). Answers whether the \
                check passed and keeps the PNG, so a run can prove a game rendered or that a layout \
                put something where it belongs, and check again after a fix. x and y are fractions of \
                the picture: 0 is the left or top edge, 1 the right or bottom, so 0.5, 0.5 is the \
                middle.",
            permission: WorkspaceWrite,
            requires_network: false,
            input_schema: s(
                json!({
                    "expect": { "type": "string", "enum": ["not-blank", "colour"] },
                    "x": { "type": "number", "minimum": 0, "maximum": 1 },
                    "y": { "type": "number", "minimum": 0, "maximum": 1 },
                    "rgb": { "type": "string", "description": "#rrggbb" },
                    "tolerance": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 255,
                        "description": "How far each channel may be from rgb; 24 when omitted."
                    }
                }),
                &["expect"],
            ),
        },
        ToolDescriptor {
            id: "spawn_subagent",
            title: "Run subagent",
            description: "Delegate one self-contained question to a subagent: a second model run that \
                          can read this workspace with read_file, list_dir, glob and grep_search but \
                          cannot change it. Its answer returns as one tool result. One delegation per run.",
            permission: SideEffect,
            requires_network: false,
            input_schema: s(
                json!({
                    "task": {
                        "type": "string",
                        "description": "The self-contained question the subagent should answer"
                    }
                }),
                &["task"],
            ),
        },
    ]
}

/// The tools this build can actually run: the four workspace reads, the plan the run keeps, the
/// three writes, the nine browser calls, the shell and the three project runners, the three
/// read-only git calls, the five calls that change the repository or take it to GitHub, and the two
/// calls that reach the internet, whose executors live in [`crate::exec`] and [`crate::web`].
///
/// This list plus [`DELEGATED`] is the whole registry: every declaration has an executor behind it,
/// and a capability this build does not have is simply absent rather than declared. A schema with no
/// verb under it is still an offer — the model reads the description, asks for the call, and spends a
/// round being told the delete it asked for cannot happen — so nothing is declared that cannot run,
/// and the registry's own test holds the two lists as one. The writes are on it because the sandbox
/// bounds them to the workspace root — one
/// makes a file that is not there, one changes text it has to find first, and one applies a diff
/// whose every old line it has to find first. The shell and the runners are on it because the user's
/// Terminal access switch bounds them: the switch is off by default, and [`executable_specs`] does
/// not hand any of them over until the switch says so. The git calls are reads of what the
/// repository already holds, with a fixed argv and no switch of their own.
///
/// The five repository calls are the route to GitHub. Staging, switching a branch and committing
/// stay inside the checkout, so the sandbox and the path rules still mean something there; the push
/// and the pull request leave the machine, which is why they carry `requires_network` and sit
/// behind the Privacy switch as well as behind an approval of their own.
pub const EXECUTABLE: &[&str] = &[
    "read_file",
    "list_dir",
    "glob",
    "grep_search",
    "todo_write",
    "write_file",
    "edit_file",
    "apply_patch",
    "terminal",
    "run_tests",
    "run_build",
    "run_linter",
    "git_status",
    "git_diff",
    "git_log",
    "git_stage",
    "git_commit",
    "git_branch",
    "git_push",
    "github_pr",
    "web_search",
    "fetch_url",
    "browser_open",
    "browser_read",
    "browser_click",
    "browser_type",
    "browser_press",
    "browser_wait",
    "browser_console",
    "browser_capture",
    "browser_check",
];

pub fn is_executable(tool_id: &str) -> bool {
    EXECUTABLE.contains(&tool_id)
}

/// The one call the host's own loop runs rather than an executor in [`crate::exec`]: a subagent is a
/// second model request, so it lives in the chat path and not in the tool crate.
///
/// It is kept out of [`EXECUTABLE`] on purpose: `execute` has no runtime and no engine, and a match
/// arm returning "not here" would be a verb this crate cannot honour. [`is_runnable`] is the answer
/// for a surface that only needs to know whether a call does something.
pub const DELEGATED: &[&str] = &["spawn_subagent"];

pub fn is_delegated(tool_id: &str) -> bool {
    DELEGATED.contains(&tool_id)
}

/// Whether a call reaches a runner in this build — an executor, or the loop's own delegation path.
/// What the Extensions page counts, what the round header counts, and what the refusal text means by
/// "available".
pub fn is_runnable(tool_id: &str) -> bool {
    is_executable(tool_id) || is_delegated(tool_id)
}

/// The declaration side for [`EXECUTABLE`], minus the two web calls while `network` is off, the
/// shell and its project runners while `shell` is off, and delegation while `subagents` is off.
///
/// The gate itself is [`crate::authorize`], which refuses each one anyway — but a tool that is
/// offered and then refused is a promise the interface made and broke, and it costs the model a round
/// to learn so. When a switch is off, the offer itself has to say what this run cannot reach.
pub fn executable_specs(network: bool, shell: bool, subagents: bool) -> Vec<Value> {
    registry()
        .into_iter()
        .filter(|t| {
            is_runnable(t.id)
                && (network || !t.requires_network)
                && (shell || !shell_governed(t.id))
                && (subagents || t.id != "spawn_subagent")
        })
        .map(spec)
        .collect()
}

/// The read-only calls: the four workspace reads, the three git reads, and nothing that writes,
/// runs or reaches the internet.
///
/// Two kinds of run are offered exactly this list. A plan-mode run may explore and must propose the
/// change rather than make it, so a write schema in its offer would be a promise the approval engine
/// then refuses. A subagent could not write either: it would change files the parent's review never
/// saw. Web reads stay out of both — one switch already governs what leaves the machine, and a plan
/// or a nested run is not the place to spend it a second time. The git calls are on the list because
/// they read the repository with a fixed argv, the same bargain `read_file` makes with the sandbox.
///
/// The page reads are the third exclusion: they are read-only against the disk, but the document they
/// read lives in the window's panel, which a subagent's own run cannot reach. A schema it cannot
/// honour would cost it a round to learn that.
pub fn read_only_specs() -> Vec<Value> {
    registry()
        .into_iter()
        .filter(|t| is_executable(t.id) && t.is_read_only() && !crate::page::is_page_tool(t.id))
        .map(spec)
        .collect()
}

/// One declaration in the `tools` array's OpenAI function form, which is what llama.cpp's server and
/// the free gateway both parse. `arguments` normalization happens in [`ToolCall::arguments_wire`];
/// this shapes the declaration side only.
fn spec(t: ToolDescriptor) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.id,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

pub fn find(tool_id: &str) -> Option<ToolDescriptor> {
    registry().into_iter().find(|t| t.id == tool_id)
}

/// The approval question for a call, derived from its class and arguments.
pub fn summary(tool_id: &str, arguments: &Value) -> String {
    match tool_id {
        "terminal" => format!(
            "Run `{}`",
            arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("<no command>")
        ),
        // A runner derives its command from the manifest when it executes, so the headline names the
        // job rather than a command this function cannot see.
        "run_tests" => "Run this project's tests".to_string(),
        "run_build" => "Build this project".to_string(),
        "run_linter" => "Run this project's linter".to_string(),
        "git_status" => "Read the working-tree status".to_string(),
        "git_diff" => "Read the working-tree diff".to_string(),
        "git_log" => "Read the recent commits".to_string(),
        "git_commit" => format!(
            "Commit: {}",
            arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
        ),
        // The repository route names the thing it would do, the way the shell names its command: a
        // commit question is about the message, a push is about the branch that leaves, and a pull
        // request is about the title the reviewer will read first.
        "git_stage" => {
            let paths = arguments
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("Stage {paths}")
        }
        "git_branch" => format!(
            "Switch to branch {}",
            arguments
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<no branch>")
        ),
        "git_push" => format!(
            "Push {}",
            arguments
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("the current branch")
        ),
        "github_pr" => format!(
            "Open a pull request: {}",
            arguments
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("<no title>")
        ),
        "fetch_url" => format!(
            "Read {}",
            arguments.get("url").and_then(Value::as_str).unwrap_or("")
        ),
        "web_search" => format!(
            "Search for {}",
            arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("<no words>")
        ),
        // A page call is named by the thing it acts on: the file it opens, the element it clicks or
        // types into, the key it presses, the element it waits for. The two reads that act on the
        // whole page say so rather than showing an empty chip.
        "browser_open" => format!(
            "Open {} in the browser",
            arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("<no path>")
        ),
        "browser_click" => format!(
            "Click {} on the page",
            arguments
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("<no selector>")
        ),
        "browser_type" => format!(
            "Type into {}",
            arguments
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("the focused field")
        ),
        "browser_press" => format!(
            "Press {} on the page",
            arguments
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("<no key>")
        ),
        "browser_wait" => format!(
            "Wait for {} on the page",
            arguments
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or("<no selector>")
        ),
        "browser_read" => "Read the page".to_string(),
        "browser_console" => "Read the page's console".to_string(),
        "browser_check" => "Check the page's picture".to_string(),
        // A write is named by its path alone: the whole contents of a new file, both sides of an
        // edit, or every line of a patch would be quoted back at the person being asked to approve
        // it.
        "write_file" | "edit_file" => format!(
            "Write {}",
            arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("<no path>")
        ),
        "apply_patch" => match arguments
            .get("patch")
            .and_then(Value::as_str)
            .and_then(patch_target)
        {
            Some(path) => format!("Write {path}"),
            None => "Apply a patch".to_string(),
        },
        // The list is the sentence: an approval question about a plan has to show the steps it would
        // record, since the plan is the whole of what the call does.
        "todo_write" => match arguments.get("items").and_then(Value::as_array) {
            Some(items) => {
                let steps: Vec<&str> = items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect();
                format!("Plan {} steps: {}", steps.len(), steps.join("; "))
            }
            None => "Update the plan".to_string(),
        },
        other => format!("{other} {arguments}"),
    }
}

/// The file a unified diff names as its target, for the row and the approval sentence that have to
/// name *what* is about to be written without quoting every line of the patch. `None` when the text
/// names no target or names `/dev/null` — a create or a delete, which this call does not perform — and
/// when it names more than one file, because a chip or a headline can hold only one name and the
/// first header would be a fraction of the truth.
///
/// The executor parses and validates the patch in full; this only reads the headers, so a patch this
/// returns a name for can still be refused for every other reason.
pub fn patch_target(patch: &str) -> Option<String> {
    let mut headers = patch.lines().filter_map(|line| line.strip_prefix("+++ "));
    let rest = headers.next()?;
    if headers.next().is_some() {
        return None;
    }
    let path = rest.split('\t').next().unwrap_or(rest).trim();
    let path = path
        .strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix('"'))
        .unwrap_or(path);
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    (!path.is_empty() && path != "/dev/null").then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_at_least_twenty_distinct_permitted_tools() {
        let r = registry();
        assert!(r.len() >= 20, "only {} tools", r.len());
        let mut ids: Vec<&str> = r.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), r.len(), "tool ids must be unique");
        assert!(r.iter().all(|t| !t.title.is_empty()));
        assert!(
            r.iter().all(|t| t.input_schema["type"] == json!("object")),
            "every tool needs an object parameter schema"
        );
    }

    /// The registry is the offer rather than a wish list: every entry reaches a runner, either an
    /// executor in [`crate::exec`] or the loop's own delegation. A schema with no verb behind it is
    /// still a promise — the model reads "Remove a file or directory inside the workspace", asks for
    /// the call, and spends a round being refused — so this is the line a new declaration has to
    /// cross before this build can list one.
    #[test]
    fn the_registry_declares_only_what_a_run_can_actually_run() {
        let r = registry();
        for t in &r {
            assert!(is_runnable(t.id), "{} is declared with no executor", t.id);
        }
        assert_eq!(
            r.len(),
            EXECUTABLE.len() + DELEGATED.len(),
            "the registry and the runnable lists have drifted apart"
        );
        // The same set the model is handed once every switch is on, so a tool cannot be declared and
        // then gated out of every offer by accident.
        let all = executable_specs(true, true, true);
        let offered: Vec<&str> = all
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        for t in &r {
            assert!(offered.contains(&t.id), "{} is never offered", t.id);
        }
    }

    #[test]
    fn every_class_is_represented_so_the_approval_engine_has_something_to_discriminate() {
        let r = registry();
        for class in [
            PermissionClass::ReadOnly,
            PermissionClass::WorkspaceWrite,
            PermissionClass::SideEffect,
            PermissionClass::Network,
        ] {
            assert!(
                r.iter().any(|t| t.permission == class),
                "no tool in class {class:?}"
            );
        }
    }

    #[test]
    fn writing_and_running_always_require_more_than_reading() {
        let read = find("read_file").unwrap();
        let write = find("write_file").unwrap();
        let sh = find("terminal").unwrap();
        assert!(read.is_read_only());
        assert_eq!(write.permission, PermissionClass::WorkspaceWrite);
        assert_eq!(sh.permission, PermissionClass::SideEffect);
    }

    #[test]
    fn the_system_prompt_names_no_tool_a_run_could_not_be_offered() {
        // The contract tells a small model which call to reach for, and a name it cannot answer is a
        // round spent on a refusal — worse, a habit the model keeps trying. So the two lists are held
        // against each other here rather than in someone's memory of what the prompt says.
        for t in registry() {
            if is_runnable(t.id) {
                continue;
            }
            assert!(
                !freecode_config::DEFAULT_SYSTEM_PROMPT.contains(t.id),
                "the shipped contract tells a run to reach for {}, which nothing can execute",
                t.id
            );
        }
        // The reading end of the same rule: the calls the prompt leans on have to still be there.
        for id in [
            "read_file",
            "edit_file",
            "apply_patch",
            "write_file",
            "todo_write",
        ] {
            assert!(
                freecode_config::DEFAULT_SYSTEM_PROMPT.contains(id),
                "the contract no longer tells the model about {id}"
            );
            assert!(is_runnable(id), "{id} is gone from the offer");
        }
    }

    #[test]
    fn network_tools_are_hidden_until_a_remote_provider_is_configured() {
        let offline = executable_specs(false, true, true);
        let online = executable_specs(true, true, true);
        assert!(offline.len() < online.len());
        let names: Vec<&str> = offline
            .iter()
            .map(|v| v["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"web_search"),
            "web_search leaked offline: {names:?}"
        );
        assert!(names.contains(&"read_file"));
    }

    #[test]
    fn empty_tool_arguments_are_sent_as_an_object_never_a_bare_string() {
        let null_args = ToolCall {
            id: "c1".into(),
            tool_id: "git_status".into(),
            arguments: Value::Null,
        };
        assert_eq!(null_args.arguments_wire(), "{}");
        let obj = ToolCall {
            id: "c2".into(),
            tool_id: "read_file".into(),
            arguments: json!({ "path": "src/main.rs" }),
        };
        assert!(obj.arguments_wire().starts_with('{'));
        // llama.cpp#112: a non-object argument payload 500s the server.
        let scalar = ToolCall {
            id: "c3".into(),
            tool_id: "run_tests".into(),
            arguments: json!("all"),
        };
        assert!(serde_json::from_str::<Value>(&scalar.arguments_wire()).is_ok());
        assert!(scalar.arguments_wire().starts_with('{'));
    }

    #[test]
    fn approval_summaries_quote_the_actual_command() {
        let s = summary("terminal", &json!({ "command": "cargo test" }));
        assert_eq!(s, "Run `cargo test`");
        assert!(
            summary("git_commit", &json!({ "message": "Add guard\n\nbody"}))
                .starts_with("Commit: Add guard")
        );
        // And a write is named by where it lands, never by the text it carries.
        assert_eq!(
            summary(
                "write_file",
                &json!({ "path": "my-app/index.html", "contents": "x".repeat(9_000) })
            ),
            "Write my-app/index.html"
        );
        // A patch is many lines of text, and the person approving it is owed the file it lands on
        // rather than the diff quoted back at them.
        assert_eq!(
            summary(
                "apply_patch",
                &json!({
                    "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n"
                })
            ),
            "Write src/lib.rs"
        );
        assert_eq!(
            summary("apply_patch", &json!({ "patch": "not a diff" })),
            "Apply a patch"
        );
        // A header naming a create or a delete names no file this call can write.
        assert_eq!(
            patch_target("--- /dev/null\n+++ b/new.rs\n"),
            Some("new.rs".to_string())
        );
        assert_eq!(patch_target("--- a/gone.rs\n+++ /dev/null\n"), None);
        // Nor does a patch that names several files: one name in the chip would be whichever file
        // the model happened to write first, and the approval would read as if that were all.
        let several = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n\
                       --- a/page.html\n+++ b/page.html\n@@ -1 +1 @@\n-c\n+d\n";
        assert_eq!(patch_target(several), None);
        assert_eq!(
            summary("apply_patch", &json!({ "patch": several })),
            "Apply a patch"
        );
    }

    /// A tool with no executor must not be offered: the model would ask for a command and every round
    /// of that is a refusal the run has to explain.
    ///
    /// The other half of the rule is what the loop promises a run: everything offered either is a
    /// workspace read the sandbox bounds, or is one of the three writes it bounds, or is a call that
    /// leaves the machine and therefore only stands on the list while the Privacy switch is on, or is
    /// the command and its runners — which stand on the list only while Terminal access is on, and are
    /// the offers the path sandbox does not bound. A delete or a rename appearing here would mean
    /// nothing at all stands between a run and the machine.
    #[test]
    fn only_bounded_reads_the_writes_the_switchable_web_calls_and_the_shell_are_offered() {
        for id in EXECUTABLE {
            let tool = find(id).unwrap_or_else(|| panic!("{id} is offered but not declared"));
            let allowed = match tool.permission {
                PermissionClass::ReadOnly | PermissionClass::WorkspaceWrite => true,
                PermissionClass::Network => matches!(*id, "web_search" | "fetch_url"),
                // A side effect either stands behind the Terminal switch, or stands on an approval of
                // its own: the commit is local but rewrites history, and the push and the pull
                // request leave the machine.
                PermissionClass::SideEffect => {
                    shell_governed(id) || matches!(*id, "git_commit" | "git_push" | "github_pr")
                }
            };
            assert!(allowed, "{id} is offered with {:?}", tool.permission);
        }
        let writes: Vec<&str> = EXECUTABLE
            .iter()
            .filter(|id| find(id).is_some_and(|t| t.permission == PermissionClass::WorkspaceWrite))
            .copied()
            .collect();
        assert_eq!(
            writes,
            vec![
                "write_file",
                "edit_file",
                "apply_patch",
                // Two writes to the repository's own index and refs: they change what the next commit
                // holds and which branch the checkout is on, and both stay inside the checkout.
                "git_stage",
                "git_branch",
                // These three are writes to the panel's document rather than to disk: they change what
                // the run's own preview shows, and the plan-mode refusal still holds them out of a run
                // that must only propose.
                "browser_click",
                "browser_type",
                "browser_press",
                // A capture writes one file the run keeps, but it writes it where the app keeps its
                // own data rather than into the project — the row is a write because a row that wrote
                // a file and said "read" would be the one place the list lied about what a call does.
                // A check takes the same picture, so it writes the same kind of file.
                "browser_capture",
                "browser_check"
            ],
            "the offered writes are a create that cannot overwrite, an edit that has to match, a patch \
             whose every old line has to match, the two repository writes that stay inside the \
             checkout, the three page actions that change only the panel's document, and the two \
             pictures of it kept beside the app's data"
        );
        // A run cannot take a file away, search a workspace there is no index for, install an add-on
        // or ask the user a question. Those are not capabilities this build declines — they are not
        // capabilities it claims, so no schema, no row and no description promises them.
        for id in [
            "delete_path",
            "move_path",
            "format_code",
            "codebase_search",
            "http_request",
            "install_extension",
            "mcp_call",
            "create_artifact",
            "ask_user",
            "wait_for_port",
        ] {
            assert!(
                find(id).is_none(),
                "{id} is declared with nothing behind it"
            );
        }
        assert!(
            is_executable("terminal"),
            "the shell has an executor; the switch is what keeps it out of a run"
        );
        // Delegation is runnable without an executor: the chat loop runs it, so it is offered with the
        // rest while its switch is on — and counted by the same roll-call the page prints.
        assert!(is_delegated("spawn_subagent"));
        assert!(!is_executable("spawn_subagent"));
        assert!(is_runnable("spawn_subagent"));
        assert_eq!(
            executable_specs(true, true, true).len(),
            EXECUTABLE.len() + DELEGATED.len()
        );
        assert!(executable_specs(true, true, true)
            .iter()
            .all(|s| s["type"] == json!("function")));
        // With the Privacy switch off, the two web calls and the two calls that reach GitHub are not
        // offered at all — refused-and-explained would spend a round of the run to arrive at the same
        // answer.
        let offline = executable_specs(false, true, true);
        let names: Vec<&str> = offline
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names.len(),
            EXECUTABLE.len() + DELEGATED.len() - 4,
            "{names:?}"
        );
        for id in ["web_search", "fetch_url", "git_push", "github_pr"] {
            assert!(!names.contains(&id), "{id}: {names:?}");
        }
        // And with Terminal access off, the command and its runners are not offered either, whatever
        // the other switch says. This is the offer the default settings produce.
        for network in [true, false] {
            let specs = executable_specs(network, false, true);
            let names: Vec<&str> = specs
                .iter()
                .map(|s| s["function"]["name"].as_str().unwrap())
                .collect();
            for id in ["terminal", "run_tests", "run_build", "run_linter"] {
                assert!(!names.contains(&id), "{names:?}");
            }
            // Four runners out with the shell off, and the four network calls out when the network is.
            assert_eq!(
                names.len(),
                EXECUTABLE.len() + DELEGATED.len() - 4 - if network { 0 } else { 4 },
                "{names:?}"
            );
            // Subagents stay out until their own switch is on, on top of every other gate.
            let no_delegation = executable_specs(network, false, false);
            assert!(!no_delegation
                .iter()
                .any(|s| s["function"]["name"] == json!("spawn_subagent")));
        }
        // The git reads are not behind the shell switch: they report what the repository holds.
        let closed = executable_specs(false, false, false);
        let names: Vec<&str> = closed
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        for id in ["git_status", "git_diff", "git_log"] {
            assert!(names.contains(&id), "{id} is not a shell call: {names:?}");
        }
    }

    #[test]
    fn a_read_only_offer_holds_the_reads_and_nothing_that_writes() {
        let names: Vec<String> = read_only_specs()
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "read_file",
                "list_dir",
                "glob",
                "grep_search",
                "todo_write",
                "git_status",
                "git_diff",
                "git_log"
            ],
            "the workspace reads, the plan and the git reads, in registry order, and nothing else"
        );
        // The schema asks for the one thing the loop reads, and no field it would ignore.
        let spawn = find("spawn_subagent").expect("declared");
        assert_eq!(spawn.input_schema["required"], json!(["task"]));
        assert_eq!(
            spawn.input_schema["properties"].as_object().unwrap().len(),
            1,
            "{:?}",
            spawn.input_schema
        );
    }
}
