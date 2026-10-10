//! The `smoll` binary: one CLI in front of the agent crates.
//!
//! Every subcommand has a `--json` form so the same output a human reads can be
//! consumed by a script or a CI job, and the exit codes mean something: 0 for
//! success, 1 for a task that ran and failed, 2 for a usage or configuration
//! error.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "smoll",
    version,
    about = "A local-first coding agent for small language models",
    long_about = "smoll reads your repository, plans a change, proposes a diff, asks before it \
                  writes, and runs the project's own tests to check itself. It answers from a \
                  model you run locally; nothing about your code leaves the machine unless you \
                  configure a remote provider."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Write a starter config file and show where it went
    Init,
    /// What this machine, this repository and this config add up to
    Doctor,
    /// Run one task end to end
    Task {
        /// The instruction, in plain language
        prompt: String,
    },
    /// List the tools this build exposes, with their permission class
    Tools,
    /// Show the resolved configuration
    Config,
}

fn main() {
    let cli = Cli::parse();
    let name = match cli.command {
        None => return print_usage_hint(),
        Some(Commands::Init) => "init",
        Some(Commands::Doctor) => "doctor",
        Some(Commands::Task { .. }) => "task",
        Some(Commands::Tools) => "tools",
        Some(Commands::Config) => "config",
    };
    eprintln!("`smoll {name}` is parsed but not wired up yet: the loop lands in Phase 5.");
    std::process::exit(2);
}

fn print_usage_hint() {
    eprintln!("smoll does something per subcommand; run `smoll --help` for the list.");
}
