//! The `smoll` binary: one CLI in front of the agent crates.
//!
//! Every command takes `-C/--cwd` and `--json`, so the same answer a human
//! reads can be consumed by a script or a CI job. The exit codes mean something:
//! 0 for success, 1 for a task that ran and did not reach its goal, 2 for a
//! usage or configuration error (clap itself uses 2, which is why a bad
//! configuration exits 2 too rather than 1).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "smoll",
    version,
    about = "A local-first coding agent for small language models",
    long_about = "smoll reads your repository, plans a change, proposes a diff, asks before it \
                  writes, and runs the project's own tests to check itself. It answers from a \
                  model you run locally; nothing about your code leaves the machine unless you \
                  configure a remote provider.\n\n\
                  Build status: the loop is still being wired. Today `init` and `config` work, \
                  `task`, `tools` and `doctor` say they are not wired yet, and the only \
                  providers that answer are the scripted mock and the echo inspector."
)]
struct Cli {
    /// Run as if started in DIR, which is how a script points the agent at a
    /// repository without changing the process's working directory.
    #[arg(short = 'C', long, value_name = "DIR", global = true)]
    cwd: Option<PathBuf>,

    /// Machine-readable output where the command has it
    #[arg(long, global = true)]
    json: bool,

    /// Log to stderr, filtered by RUST_LOG when that is set
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Write a starter smoll.toml and show what it contains
    Init {
        /// Replace an existing smoll.toml
        #[arg(long)]
        force: bool,
    },
    /// What this machine, this repository and this config add up to
    Doctor,
    /// Show the resolved configuration: files, environment and defaults
    Config,
    /// List the tools this build exposes, with their permission class
    Tools,
    /// Run one task end to end
    Task {
        /// The instruction, in plain language
        prompt: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let cwd = match resolve_cwd(cli.cwd.as_deref()) {
        Ok(path) => path,
        Err(message) => return fail(&message),
    };
    let outcome = match cli.command {
        None => return print_usage_hint(),
        Some(Commands::Init { force }) => cmd_init(&cwd, cli.json, force),
        Some(Commands::Config) => cmd_config(&cwd, cli.json),
        Some(Commands::Doctor) => not_wired("doctor"),
        Some(Commands::Tools) => not_wired("tools"),
        Some(Commands::Task { .. }) => not_wired("task"),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(&message),
    }
}

fn resolve_cwd(explicit: Option<&Path>) -> Result<PathBuf, String> {
    match explicit {
        Some(path) => {
            if !path.is_dir() {
                return Err(format!("{} is not a directory", path.display()));
            }
            Ok(path.to_path_buf())
        }
        None => {
            std::env::current_dir().map_err(|source| format!("cannot find the directory: {source}"))
        }
    }
}

/// `smoll init` is the first thing a reader does, so it has to leave them with
/// a configuration that runs and tell them plainly what it will do.
fn cmd_init(cwd: &Path, json: bool, force: bool) -> Result<(), String> {
    let path = agent_config::write_starter(cwd, force).map_err(|source| source.to_string())?;
    let loaded = agent_config::load(cwd).map_err(|source| source.to_string())?;
    if json {
        let report = serde_json::json!({
            "written": path,
            "approvalMode": loaded.config.agent.approval_mode.to_string(),
            "provider": loaded.config.resolve_provider().map_err(render)?.0,
            "sources": loaded.sources,
            "warnings": loaded.warnings,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("a value built here")
        );
        return Ok(());
    }
    println!("wrote {}", path.display());
    println!();
    println!("  provider       {}", describe_provider(&loaded.config));
    println!("  approval mode  {}", loaded.config.agent.approval_mode);
    println!("  workspace only {}", loaded.config.privacy.workspace_only);
    println!();
    println!(
        "The starter answers from the mock provider, so this configuration resolves with no \
         model on disk. `smoll task` is not wired to the loop yet; point it at a real model by \
         editing [providers] in {}.",
        path.display()
    );
    print_warnings(&loaded.warnings);
    Ok(())
}

fn cmd_config(cwd: &Path, json: bool) -> Result<(), String> {
    let loaded = agent_config::load(cwd).map_err(|source| source.to_string())?;
    if json {
        let report = serde_json::json!({
            "config": serde_json::to_value(&loaded.config).expect("a type that serialises"),
            "sources": loaded.sources,
            "warnings": loaded.warnings,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("a value built here")
        );
        return Ok(());
    }
    for source in &loaded.sources {
        eprintln!("# from {source}");
    }
    print!(
        "{}",
        loaded
            .config
            .to_toml_string()
            .map_err(|source| source.to_string())?
    );
    print_warnings(&loaded.warnings);
    Ok(())
}

fn describe_provider(config: &agent_config::Config) -> String {
    match config.resolve_provider() {
        Ok((name, provider)) => format!("{name} (type {})", provider.kind),
        Err(problems) => render(problems),
    }
}

fn render(problems: Vec<agent_config::Problem>) -> String {
    problems
        .iter()
        .map(|problem| problem.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn not_wired(command: &str) -> Result<(), String> {
    Err(format!(
        "`smoll {command}` is parsed but not wired up yet: the agent loop lands in Phase 5"
    ))
}

fn print_usage_hint() -> ExitCode {
    eprintln!("smoll does something per subcommand; run `smoll --help` for the list.");
    ExitCode::from(2)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(2)
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(if verbose { "info" } else { "warn" })
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()
        .ok();
}
