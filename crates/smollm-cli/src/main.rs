//! `smollm` — the command line companion to SmolLLM Studio.
//!
//! It uses the same crates as the desktop app: the same catalog, the same
//! download manager, the same engine abstraction and the same loopback-only
//! server. Nothing is uploaded anywhere; the only network traffic is the HTTPS
//! fetch of a model you asked for.

mod actions;
mod report;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "smollm",
    version,
    about = "Run small local LLMs beautifully — from the terminal.",
    long_about = "smollm shares every crate with the SmolLLM Studio desktop app: the same curated\ncatalog, the same downloads, the same engine layer and the same loopback-only\nOpenAI-compatible server.\n\nThe only network traffic is the model download you explicitly ask for."
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct GlobalArgs {
    /// Keep downloaded models in another directory
    #[arg(long, global = true, value_name = "DIR")]
    models_dir: Option<PathBuf>,
    /// Show engine, download and HTTP internals on stderr
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Report what this machine can run.
    Hardware {
        #[arg(long)]
        json: bool,
    },
    /// Readiness check: RAM, disk, GPU and a short verdict.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Browse the curated catalog and the local library.
    #[command(subcommand)]
    Models(ModelsCommand),
    /// Load a model and answer one prompt.
    Run(RunArgs),
    /// Serve an OpenAI-compatible API on localhost.
    Serve(ServeArgs),
    /// Measure load, prefill and generation speed.
    Bench(BenchArgs),
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// List catalog entries.
    List {
        /// Substring matched against id, name, family and tags
        #[arg(short, long, default_value = "")]
        query: String,
        /// recommended | smallest | largest | fastest | name
        #[arg(short, long, default_value = "recommended")]
        sort: String,
        /// Hide entries above this parameter count
        #[arg(long, value_name = "B")]
        max_params: Option<f32>,
        /// Keep catalog entries whose Hugging Face file is unconfirmed
        #[arg(long)]
        include_placeholders: bool,
        #[arg(long)]
        json: bool,
    },
    /// Download a model into the local library.
    Pull {
        #[arg(value_name = "MODEL")]
        model: String,
        /// Re-download even when the file is already present
        #[arg(long)]
        force: bool,
    },
    /// List models already on disk.
    Local {
        #[arg(long)]
        json: bool,
    },
    /// Copy a .gguf you already have into the local library.
    Import {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Delete a downloaded file by name.
    Rm {
        #[arg(value_name = "FILE")]
        file: String,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Catalog id, or the file name of a downloaded model
    #[arg(value_name = "MODEL")]
    model: String,
    /// Text to send
    #[arg(short, long, default_value = "Introduce yourself in one sentence.")]
    prompt: String,
    /// Read the prompt from a file, or `-` for stdin
    #[arg(long, value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// System prompt
    #[arg(short, long)]
    system: Option<String>,
    /// fast | balanced | creative | coding | precise
    #[arg(long, default_value = "balanced")]
    preset: String,
    #[arg(long)]
    temperature: Option<f32>,
    #[arg(long)]
    top_p: Option<f32>,
    #[arg(long)]
    max_tokens: Option<u32>,
    #[arg(long, default_value_t = 4096)]
    context: u32,
    #[arg(long, default_value_t = -1)]
    gpu_layers: i32,
    /// auto | mock | llama-cpp | candle | gguf-metadata
    #[arg(long, default_value = "auto")]
    engine: String,
    /// Print the answer once instead of streaming tokens
    #[arg(long)]
    buffer: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ServeArgs {
    /// Loopback host only; a public address is refused
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    /// Catalog id or file name to load before serving
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// auto | mock | llama-cpp | candle | gguf-metadata
    #[arg(long, default_value = "auto")]
    engine: String,
    #[arg(long, default_value_t = 4096)]
    context: u32,
    /// Print the curl and Python snippets without starting the server
    #[arg(long)]
    examples_only: bool,
}

#[derive(Args)]
struct BenchArgs {
    #[arg(value_name = "MODEL")]
    model: String,
    #[arg(long, default_value_t = 128)]
    prompt_tokens: u32,
    #[arg(long, default_value_t = 128)]
    max_tokens: u32,
    /// Repeat the generation phase and report the best run
    #[arg(long, default_value_t = 1)]
    runs: u32,
    #[arg(long, default_value_t = 4096)]
    context: u32,
    #[arg(long, default_value_t = -1)]
    gpu_layers: i32,
    /// auto | mock | llama-cpp | candle | gguf-metadata
    #[arg(long, default_value = "auto")]
    engine: String,
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    actions::install_tracing(cli.global.verbose);

    let mut ctx = actions::Context::open(cli.global.models_dir)?;
    match cli.command {
        Command::Hardware { json } => actions::hardware(&mut ctx, json),
        Command::Doctor { json } => actions::doctor(&mut ctx, json),
        Command::Models(ModelsCommand::List {
            query,
            sort,
            max_params,
            include_placeholders,
            json,
        }) => actions::list_catalog(&ctx, &query, &sort, max_params, include_placeholders, json),
        Command::Models(ModelsCommand::Pull { model, force }) => {
            actions::pull(&ctx, &model, force).await
        }
        Command::Models(ModelsCommand::Local { json }) => actions::list_local(&ctx, json),
        Command::Models(ModelsCommand::Import { path }) => actions::import(&ctx, &path),
        Command::Models(ModelsCommand::Rm { file }) => actions::remove(&ctx, &file),
        Command::Run(args) => actions::run(&mut ctx, &args).await,
        Command::Serve(args) => actions::serve(&mut ctx, &args).await,
        Command::Bench(args) => actions::bench(&mut ctx, &args).await,
    }
}
