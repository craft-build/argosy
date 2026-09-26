//! The `argosy` binary: argument parsing + one library call + output
//! formatting per subcommand. Any real logic here is a bug — it belongs in
//! the library. Exit codes: `0` success, `1` command failure, `2` usage
//! error.

use std::path::PathBuf;
use std::process::ExitCode;

use argosy::Config;
use argosy::error::Result;
use clap::{Parser, Subcommand};
use serde::Serialize;

/// The `argosy` command line.
#[derive(Parser)]
#[command(
    name = "argosy",
    version,
    about = "Create, validate, package, and query OKF knowledge bundles"
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (default: the configured
    /// `output.json`).
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    json: Option<bool>,

    /// Suppress non-error human output (default: the configured
    /// `output.quiet`).
    #[arg(short, long, global = true, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    quiet: Option<bool>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    Validate(ValidateArgs),
    Package(PackageArgs),
    Pull(PullArgs),
    Index(IndexArgs),
    Convert(ConvertArgs),
    Catalog(CatalogArgs),
    Agent(AgentArgs),
    Mcp(McpArgs),
    Config(ConfigArgs),
}

mod args;
mod commands;

use args::*;

#[cfg(test)]
mod tests;

use commands::*;

/// Output control shared by all subcommands.
struct Output {
    json: bool,
    quiet: bool,
}

impl Output {
    /// Non-error human output; suppressed by `--quiet` and `--json`.
    fn note(&self, line: &str) {
        if !self.quiet && !self.json {
            println!("{line}");
        }
    }

    /// A safety warning: prints even under `--quiet` and `--json` — stderr
    /// is not part of the JSON contract, and the safeguard must never be
    /// silent.
    fn warn(&self, line: &str) {
        eprintln!("warning: {line}");
    }

    /// JSON stdout for machine consumers. A serialization failure is a
    /// command failure (exit 1) — never empty stdout plus a success code.
    fn json<T: Serialize>(&self, value: &T) -> Result<()> {
        match serde_json::to_string_pretty(value) {
            Ok(text) => {
                println!("{text}");
                Ok(())
            }
            Err(e) => Err(argosy::error::Error::Validation {
                reason: format!("failed to serialize JSON output: {e}"),
            }),
        }
    }
}

/// The library `Error` type of one subcommand execution; business failures
/// (non-conformant bundle, import findings) are the returned `ExitCode`.
fn cmd_result(out: &Output, config: &Config, command: &Command) -> Result<ExitCode> {
    match command {
        Command::Init(args) => cmd_init(out, args),
        Command::Validate(args) => cmd_validate(out, args),
        Command::Package(args) => cmd_package(out, config, args),
        Command::Pull(args) => cmd_pull(out, config, args),
        Command::Index(args) => cmd_index(out, config, args),
        Command::Convert(args) => cmd_convert(out, config, args),
        Command::Catalog(args) => cmd_catalog(out, config, args),
        Command::Agent(args) => cmd_agent(out, args),
        Command::Mcp(args) => cmd_mcp(out, config, args),
        Command::Config(args) => cmd_config(out, args, config),
    }
}

/// Entry point: parse, load the user configuration, and dispatch.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    let out = Output {
        json: cli.json.unwrap_or(config.output.json),
        quiet: cli.quiet.unwrap_or(config.output.quiet),
    };
    match cmd_result(&out, &config, &cli.command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The configured embedding-model cache, when to apply it: an explicitly
/// set `$ARGOSY_EMBED_CACHE_DIR` (or the legacy `$FASTEMBED_CACHE_DIR`)
/// always wins — then the library resolves it and `None` is returned.
pub(super) fn embed_cache_override(config: &Config) -> Option<PathBuf> {
    if std::env::var_os("ARGOSY_EMBED_CACHE_DIR")
        .or_else(|| std::env::var_os("FASTEMBED_CACHE_DIR"))
        .is_some()
    {
        return None;
    }
    config.embed_cache_dir()
}

/// The process working directory, mapped to the library's `Io` error.
/// Project-scoped commands (init/pull/convert/index) resolve their
/// default targets from it.
fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|source| argosy::error::Error::Io {
        path: ".".into(),
        source,
    })
}
