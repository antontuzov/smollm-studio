//! Asking git what it thinks of the working tree.
//!
//! These two tools exist as themselves rather than as `run_command git …`
//! because the agent should not have to be told which git flags are safe: the
//! argv is written here, in full, and the model supplies only a path and two
//! switches. That is what lets both of them be [`Permission::ReadOnly`] — the
//! class a command-derived intent would otherwise raise to `execute` — and it is
//! why neither can be talked into `git reset --hard` by a persuasive file name.
//!
//! Both run with `GIT_OPTIONAL_LOCKS=0`, so a read cannot take a lock another
//! process needs, and neither reaches the network: no fetch, no push, no remote
//! is ever named.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

use agent_sandbox::Permission;

use crate::call::{ToolCall, ToolOutput, ToolResult};
use crate::context::Context;
use crate::schema;
use crate::tool::{Intent, Tool, ToolDefinition};

/// How long git gets to answer. A status call on a huge tree is slow, but a
/// status call that has not answered in half a minute is not going to.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct GitStatus;

#[async_trait]
impl Tool for GitStatus {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "git_status",
            "Show what git thinks of the working tree: the branch line, then one line per \
             changed path (`M` modified, `??` untracked, `A` added, `D` deleted, `R` renamed), \
             with the staged state in the first column. `path` narrows it to one file or \
             directory. Reads only: nothing is fetched, staged, committed or stashed.",
            schema::object(
                json!({
                    "path": schema::path("file or directory to restrict the listing to; default the whole repository"),
                }),
                &[],
            ),
            Permission::ReadOnly,
        )
    }

    fn intent(&self, call: &ToolCall, ctx: &Context) -> Result<Intent, String> {
        Ok(Intent::reading(vec![scope(call, ctx)?]))
    }

    async fn run(&self, call: &ToolCall, ctx: &Context) -> ToolResult {
        let scope = match self.intent(call, ctx) {
            Ok(intent) => intent.paths[0].clone(),
            Err(problem) => return failed(call, problem),
        };
        let mut argv = own(&[
            "git",
            "--no-pager",
            "-c",
            "core.quotepath=false",
            "status",
            "--short",
            "--branch",
        ]);
        if let Some(problem) = narrow(&mut argv, ctx, &scope) {
            return failed(call, problem);
        }
        match git(&argv, &ctx.root).await {
            Ok(text) => ToolResult::completed(
                call.clone(),
                Permission::ReadOnly,
                ToolOutput::new(status_text(&text)),
                0,
            ),
            Err(problem) => failed(call, problem),
        }
    }
}

/// The branch line, and then either the changed paths or the sentence that says
/// there are none.
///
/// `--branch` means git never prints nothing, so "clean" has to be read from
/// the absence of rows rather than from an empty string — and the two porcelain
/// columns are the staged-versus-worktree state, so a row is kept exactly as
/// git wrote it.
fn status_text(text: &str) -> String {
    let branch = text
        .lines()
        .find(|line| line.starts_with("## "))
        .unwrap_or("");
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("## ") && !line.trim().is_empty())
        .collect();
    if rows.is_empty() {
        return format!("{branch}\nclean: nothing has changed since the last commit");
    }
    if branch.is_empty() {
        rows.join("\n")
    } else {
        format!("{branch}\n{}", rows.join("\n"))
    }
}

/// The diff itself, which is what the agent proposes and what a human approves.
pub struct GitDiff;

#[async_trait]
impl Tool for GitDiff {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "git_diff",
            "Show the diff of the working tree against the index, or the staged changes when \
             `staged` is true. `path` narrows it to one file or directory; `context_lines` \
             says how much unchanged text to show around each hunk (git's default is 3). The \
             answer is the diff, not a summary of it. Reads only: the working tree is not \
             touched and nothing is fetched.",
            schema::object(
                json!({
                    "path": schema::path("file or directory to restrict the diff to; default everything"),
                    "staged": schema::boolean("diff what is staged for commit instead of the working tree"),
                    "context_lines": schema::integer("unchanged lines shown around each hunk; default 3", 0),
                }),
                &[],
            ),
            Permission::ReadOnly,
        )
    }

    fn intent(&self, call: &ToolCall, ctx: &Context) -> Result<Intent, String> {
        Ok(Intent::reading(vec![scope(call, ctx)?]))
    }

    async fn run(&self, call: &ToolCall, ctx: &Context) -> ToolResult {
        let scope = match self.intent(call, ctx) {
            Ok(intent) => intent.paths[0].clone(),
            Err(problem) => return failed(call, problem),
        };
        let mut argv = own(&[
            "git",
            "--no-pager",
            "-c",
            "core.quotepath=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
        ]);
        if let Some(lines) = call.usize_arg("context_lines") {
            argv.push(format!("-U{lines}"));
        }
        if call.bool_arg("staged").unwrap_or(false) {
            argv.push("--staged".to_owned());
        }
        if let Some(problem) = narrow(&mut argv, ctx, &scope) {
            return failed(call, problem);
        }
        let empty = if scope == ctx.root {
            "no changes in the working tree".to_owned()
        } else {
            format!("no changes under {}", ctx.relative(&scope))
        };
        let answer = git(&argv, &ctx.root).await;
        report(call, answer, &empty)
    }
}

/// The directory or file the model named, defaulting to the whole repository.
fn scope(call: &ToolCall, ctx: &Context) -> Result<PathBuf, String> {
    let raw = call.str_arg("path").unwrap_or_else(|| ".".to_owned());
    ctx.resolve(&raw)
}

/// Append `-- <relative path>` unless the scope is the whole repository.
///
/// The path goes after `--` and as its own argument, so a file named
/// `--help` or `; rm -rf` is a file name rather than a flag or a command line.
fn narrow(argv: &mut Vec<String>, ctx: &Context, scope: &PathBuf) -> Option<String> {
    if scope == &ctx.root {
        return None;
    }
    match scope.strip_prefix(&ctx.root) {
        Ok(relative) if !relative.as_os_str().is_empty() => {
            argv.push("--".to_owned());
            argv.push(relative.to_string_lossy().into_owned());
            None
        }
        _ => Some(format!(
            "{} is not inside the repository at {}",
            scope.display(),
            ctx.root.display()
        )),
    }
}

/// An empty answer means "nothing changed", which is a result rather than a
/// failure — the difference between a clean tree and a tool that did not work.
fn report(call: &ToolCall, answer: Result<String, String>, when_empty: &str) -> ToolResult {
    match answer {
        Ok(text) if text.trim().is_empty() => ToolResult::completed(
            call.clone(),
            Permission::ReadOnly,
            ToolOutput::new(when_empty),
            0,
        ),
        Ok(text) => {
            ToolResult::completed(call.clone(), Permission::ReadOnly, ToolOutput::new(text), 0)
        }
        Err(problem) => failed(call, problem),
    }
}

/// Run one git command, with no shell, no stdin and no network, from the
/// workspace root. A non-zero exit is not a crash: it becomes the message the
/// model reads, in git's own words.
async fn git(argv: &[String], cwd: &PathBuf) -> Result<String, String> {
    let program = argv.first().ok_or_else(|| "no program to run".to_owned())?;
    let spawned = Command::new(program)
        .args(&argv[1..])
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        // One language for the messages, whatever the terminal's is: a model
        // that reads "не найден git репозиторий" cannot act on it, and this
        // crate's own tests assert on those sentences. `core.quotepath=false`
        // keeps a non-ASCII file name readable instead of octal-escaped.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let output = match spawned {
        Ok(child) => tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                format!(
                    "{} did not answer within {}s",
                    program,
                    GIT_TIMEOUT.as_secs()
                )
            })?
            .map_err(|source| format!("cannot run {}: {source}", argv.join(" ")))?,
        Err(source) => {
            return Err(format!(
                "cannot run {}: {source}. Is git installed and on PATH?",
                argv.join(" ")
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let complaint = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if complaint.contains("not a git repository") {
        return Err(format!(
            "{} is not a git repository, so git has nothing to report",
            cwd.display()
        ));
    }
    let code = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "on a signal".to_owned());
    Err(format!(
        "{} exited {code}{}",
        argv.join(" "),
        if complaint.is_empty() {
            String::new()
        } else {
            format!(": {complaint}")
        }
    ))
}

fn own(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn failed(call: &ToolCall, message: String) -> ToolResult {
    ToolResult::failed(
        call.clone(),
        Permission::ReadOnly,
        message,
        ToolOutput::empty(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// A real repository, because a git tool tested against a mock git proves
    /// nothing about the flags it passes. `None` means this machine has no git,
    /// which is a reason to skip rather than to fail.
    fn repo(files: &[(&str, &str)]) -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().expect("a temp dir");
        if !git_in(dir.path(), &["--version"]) {
            return None;
        }
        assert!(git_in(dir.path(), &["init", "-q", "."]));
        assert!(git_in(
            dir.path(),
            &["config", "user.email", "agent@example.invalid"]
        ));
        assert!(git_in(dir.path(), &["config", "user.name", "Agent"]));
        for (name, contents) in files {
            std::fs::write(dir.path().join(name), contents).expect("writable");
            // After a `--`, so a file called `--help` is a file name.
            assert!(git_in(dir.path(), &["add", "--", name]));
        }
        assert!(git_in(dir.path(), &["commit", "-q", "-m", "start"]));
        Some(dir)
    }

    fn context(dir: &tempfile::TempDir) -> Context {
        Context::new(dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn a_modified_and_an_untracked_file_are_both_listed() {
        let Some(dir) = repo(&[("tracked.txt", "one\n"), ("second.txt", "same\n")]) else {
            return;
        };
        std::fs::write(dir.path().join("tracked.txt"), "two\n").expect("writable");
        std::fs::write(dir.path().join("untracked.txt"), "new\n").expect("writable");
        let ctx = context(&dir);
        let result = GitStatus
            .run(&ToolCall::new("git_status", json!({})), &ctx)
            .await;
        assert!(result.is_success(), "{}", result.summary());
        let text = result.output.text;
        assert!(
            text.contains("M  tracked.txt") || text.contains(" M tracked.txt"),
            "{text}"
        );
        assert!(text.contains("?? untracked.txt"), "{text}");
        assert!(text.starts_with("## "), "{text}");
    }

    #[tokio::test]
    async fn a_clean_tree_is_an_answer_not_a_failure() {
        let Some(dir) = repo(&[("tracked.txt", "one\n")]) else {
            return;
        };
        let ctx = context(&dir);
        let result = GitStatus
            .run(&ToolCall::new("git_status", json!({})), &ctx)
            .await;
        assert!(result.is_success(), "{}", result.summary());
        assert!(
            result.output.text.contains("clean"),
            "{}",
            result.output.text
        );
    }

    #[tokio::test]
    async fn a_scope_limits_the_listing_to_one_path() {
        let Some(dir) = repo(&[("tracked.txt", "one\n")]) else {
            return;
        };
        std::fs::write(dir.path().join("tracked.txt"), "two\n").expect("writable");
        std::fs::write(dir.path().join("other.txt"), "new\n").expect("writable");
        let ctx = context(&dir);
        let result = GitStatus
            .run(
                &ToolCall::new("git_status", json!({"path": "tracked.txt"})),
                &ctx,
            )
            .await;
        let text = result.output.text;
        assert!(text.contains("tracked.txt"), "{text}");
        assert!(!text.contains("other.txt"), "{text}");
    }

    #[tokio::test]
    async fn a_diff_shows_the_change_that_was_made() {
        let Some(dir) = repo(&[("tracked.txt", "one\n")]) else {
            return;
        };
        std::fs::write(dir.path().join("tracked.txt"), "two\n").expect("writable");
        let ctx = context(&dir);
        let result = GitDiff
            .run(
                &ToolCall::new("git_diff", json!({"path": "tracked.txt"})),
                &ctx,
            )
            .await;
        assert!(result.is_success(), "{}", result.summary());
        let text = result.output.text;
        assert!(text.contains("-one"), "{text}");
        assert!(text.contains("+two"), "{text}");
        assert!(text.contains("b/tracked.txt"), "{text}");
    }

    #[tokio::test]
    async fn staged_and_unstaged_are_separate_questions() {
        let Some(dir) = repo(&[("tracked.txt", "one\n")]) else {
            return;
        };
        std::fs::write(dir.path().join("tracked.txt"), "two\n").expect("writable");
        assert!(git_in(dir.path(), &["add", "tracked.txt"]));
        let ctx = context(&dir);

        let staged = GitDiff
            .run(&ToolCall::new("git_diff", json!({"staged": true})), &ctx)
            .await;
        assert!(
            staged.output.text.contains("+two"),
            "{}",
            staged.output.text
        );

        let unstaged = GitDiff
            .run(&ToolCall::new("git_diff", json!({})), &ctx)
            .await;
        assert!(
            unstaged.output.text.contains("no changes"),
            "{}",
            unstaged.output.text
        );
    }

    #[tokio::test]
    async fn context_lines_are_the_models_to_ask_for() {
        let lines = "a\nb\nc\nd\nE\nf\ng\nh\ni\n";
        let Some(dir) = repo(&[("many.txt", lines)]) else {
            return;
        };
        std::fs::write(dir.path().join("many.txt"), lines.replace('E', "e")).expect("writable");
        let ctx = context(&dir);

        let tight = GitDiff
            .run(
                &ToolCall::new("git_diff", json!({"path": "many.txt", "context_lines": 1})),
                &ctx,
            )
            .await;
        let text = tight.output.text;
        assert!(text.contains("-E"), "{text}");
        assert!(text.contains("+e"), "{text}");
        // Only the body rows carry context; a `@@ … @@ c` suffix is git's own
        // hint about where the hunk lives, not a line of the file.
        let shown: Vec<&str> = text.lines().filter(|line| line.starts_with(' ')).collect();
        assert!(shown.contains(&" d"), "one context line is d: {text}");
        assert!(shown.contains(&" f"), "{text}");
        assert!(
            !shown.contains(&" c"),
            "one context line does not reach c: {text}"
        );

        let wide = GitDiff
            .run(
                &ToolCall::new("git_diff", json!({"path": "many.txt", "context_lines": 3})),
                &ctx,
            )
            .await;
        let shown: Vec<&str> = wide
            .output
            .text
            .lines()
            .filter(|line| line.starts_with(' '))
            .collect();
        assert!(shown.contains(&" c"), "{}", wide.output.text);
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repository_says_so() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("loose.txt"), "x").expect("writable");
        let ctx = context(&dir);
        let result = GitStatus
            .run(&ToolCall::new("git_status", json!({})), &ctx)
            .await;
        let summary = result.summary();
        if git_in(dir.path(), &["--version"]) {
            assert!(summary.contains("not a git repository"), "{summary}");
        } else {
            assert!(summary.contains("cannot run git"), "{summary}");
        }
    }

    #[tokio::test]
    async fn a_file_name_cannot_become_a_flag() {
        let Some(dir) = repo(&[("--help", "one\n")]) else {
            return;
        };
        std::fs::write(dir.path().join("--help"), "two\n").expect("writable");
        let ctx = context(&dir);
        let result = GitDiff
            .run(&ToolCall::new("git_diff", json!({"path": "--help"})), &ctx)
            .await;
        assert!(result.is_success(), "{}", result.summary());
        assert!(
            result.output.text.contains("+two"),
            "{}",
            result.output.text
        );
    }

    #[test]
    fn both_tools_are_reads_and_never_carry_a_command_in_their_intent() {
        let ctx = Context::new(PathBuf::from("/repo"));
        for tool in [Box::new(GitStatus) as Box<dyn Tool>, Box::new(GitDiff)] {
            let definition = tool.definition();
            assert_eq!(definition.permission, Permission::ReadOnly);
            assert!(
                definition.description.contains("Reads only"),
                "{} says what it does not do",
                definition.name
            );
            let intent = tool
                .intent(&ToolCall::new(definition.name.clone(), json!({})), &ctx)
                .expect("no arguments needed");
            assert!(
                intent.command.is_none(),
                "the argv is not the model's to write"
            );
            assert_eq!(intent.paths, vec![PathBuf::from("/repo")]);
            assert_eq!(
                intent.permission(Permission::ReadOnly),
                Permission::ReadOnly,
                "a git read stays a read"
            );
        }
    }

    #[test]
    fn a_path_outside_the_repository_is_refused_before_git_runs() {
        let ctx = Context::new(PathBuf::from("/repo"));
        let call = ToolCall::new("git_diff", json!({"path": "../outside"}));
        let problem = GitDiff.intent(&call, &ctx).expect_err("climbs out");
        assert!(problem.contains("climbs out"), "{problem}");
    }

    #[test]
    fn a_scope_becomes_an_argument_after_the_double_dash() {
        let ctx = Context::new(PathBuf::from("/repo"));
        let mut argv = own(&["git", "diff"]);
        let scope = PathBuf::from("/repo/src/lib.rs");
        assert_eq!(narrow(&mut argv, &ctx, &scope), None);
        assert_eq!(argv, vec!["git", "diff", "--", "src/lib.rs"]);

        let mut argv = own(&["git", "diff"]);
        assert_eq!(narrow(&mut argv, &ctx, &PathBuf::from("/repo")), None);
        assert_eq!(
            argv,
            vec!["git", "diff"],
            "the whole repository needs no scope"
        );
    }
}
