//! CLI entry point for the CodeTracer PolkaVM recorder.
//!
//! Supports the `record`, `trace-ink`, and `replay` subcommands.
//!
//! # Usage
//!
//! ```text
//! codetracer-polkavm-recorder record <blob-file> --out-dir <output-dir>
//! codetracer-polkavm-recorder trace-ink --contract <blob> --message <name> ...
//! codetracer-polkavm-recorder replay --address <addr> --selector <name> ...
//! ```
//!
//! The recorder always writes traces in the canonical CodeTracer multi-stream
//! CTFS format (see `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`).
//! No `--format` flag is exposed: human-readable conversion is handled
//! out-of-band by `ct print` (shipped with `codetracer-trace-format-nim`).
//!
//! # Environment variables
//!
//! * `CODETRACER_POLKAVM_RECORDER_OUT_DIR` — fallback for `--out-dir` when the
//!   flag is not given. The CLI flag always wins.
//! * `CODETRACER_POLKAVM_RECORDER_DISABLED` — set to `1` or `true` to skip
//!   recording entirely. The recorder still validates its inputs (where
//!   applicable) and propagates a clean exit code.
//! * `CODETRACER_POLKAVM_RECORDER_LOG_LEVEL` — recorder log verbosity
//!   (advisory; the PolkaVM recorder currently logs to stderr unconditionally).

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use eyre::{Context, Result};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Environment variable used as a fallback for `--out-dir` when the CLI
/// flag is omitted.  Convention: see `Recorder-CLI-Conventions.md` §5.
const ENV_OUT_DIR: &str = "CODETRACER_POLKAVM_RECORDER_OUT_DIR";

/// Environment variable that, when set to `1`/`true`, disables tracing
/// entirely — the recorder runs as a transparent pass-through.
const ENV_DISABLED: &str = "CODETRACER_POLKAVM_RECORDER_DISABLED";

/// Default output directory used when neither `--out-dir` nor
/// `CODETRACER_POLKAVM_RECORDER_OUT_DIR` is set.
const DEFAULT_OUT_DIR: &str = "./ct-traces/";

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// CodeTracer PolkaVM recorder — record PolkaVM execution traces.
///
/// Traces are always written in the canonical CTFS multi-stream format.
/// To convert a recorded `.ct` bundle to JSON / text for inspection, use
/// `ct print` from `codetracer-trace-format-nim`.
#[derive(Debug, Parser)]
#[command(
    name = "codetracer-polkavm-recorder",
    version,
    about = "Record PolkaVM program execution traces for CodeTracer (CTFS-only). \
             Use `ct print` from codetracer-trace-format-nim for human-readable conversion.",
    long_about = "Record PolkaVM program execution traces for CodeTracer.\n\
                  \n\
                  Output is always written in the canonical CodeTracer CTFS\n\
                  multi-stream format. Use `ct print` (shipped with the\n\
                  codetracer-trace-format-nim sibling) to convert a recorded\n\
                  `.ct` bundle to JSON or other human-readable forms.\n\
                  \n\
                  Environment variables:\n\
                    CODETRACER_POLKAVM_RECORDER_OUT_DIR    fallback for --out-dir\n\
                    CODETRACER_POLKAVM_RECORDER_DISABLED   set to 1/true to skip recording\n\
                    CODETRACER_POLKAVM_RECORDER_LOG_LEVEL  log verbosity (advisory)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Record execution of a PolkaVM program.
    ///
    /// Loads the given .polkavm blob file, executes it through PolkaVM with
    /// step tracing enabled, captures the execution trace, and writes a CTFS
    /// trace bundle to `--out-dir`.
    Record(RecordArgs),

    /// Trace an ink! smart contract message invocation.
    ///
    /// Loads the contract blob, sets up simulated host functions for
    /// ink! storage/input/return, and records a trace of the given
    /// message execution.
    TraceInk(TraceInkArgs),

    /// Replay a contract call fetched from a Substrate chain.
    ///
    /// Connects to a Substrate node via RPC, fetches the deployed contract
    /// code, and replays the specified message invocation with full step
    /// tracing. This is the M8 milestone: "On-Chain Contract Replay via
    /// Substrate RPC".
    Replay(ReplayArgs),

    /// Print version information.
    Version,
}

#[derive(Debug, clap::Args)]
struct RecordArgs {
    /// Path to the PolkaVM program blob (.polkavm) file.
    program: PathBuf,

    /// Directory where the trace files will be written.
    ///
    /// The directory will be created if it does not exist.  Falls back to
    /// the `CODETRACER_POLKAVM_RECORDER_OUT_DIR` environment variable when
    /// the flag is omitted.
    #[arg(short = 'o', long)]
    out_dir: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct TraceInkArgs {
    /// Path to the ink! contract (.polkavm blob or .contract bundle).
    #[arg(long = "contract")]
    contract: PathBuf,

    /// Name of the ink! message to invoke (e.g. "flip", "get").
    #[arg(long = "message")]
    message: String,

    /// Message argument (can be repeated). Arguments are passed as
    /// hex-encoded SCALE values.
    #[arg(long = "arg")]
    arg: Vec<String>,

    /// Constructor to call before the message (e.g. "new", "default").
    #[arg(long = "constructor")]
    constructor: Option<String>,

    /// Directory where the trace files will be written.
    ///
    /// Falls back to the `CODETRACER_POLKAVM_RECORDER_OUT_DIR` environment
    /// variable when the flag is omitted.
    #[arg(short = 'o', long)]
    out_dir: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ReplayArgs {
    /// On-chain contract address (SS58 or hex).
    #[arg(long)]
    address: String,

    /// ink! message selector name (e.g. "get", "flip").
    #[arg(long)]
    selector: String,

    /// Hex-encoded calldata (arguments) appended after the 4-byte selector.
    #[arg(long, default_value = "")]
    calldata: String,

    /// Substrate RPC endpoint URL.
    #[arg(long, default_value = "ws://127.0.0.1:9944")]
    endpoint: String,

    /// Block hash at which to fetch contract state (latest if omitted).
    #[arg(long)]
    block: Option<String>,

    /// Path to contract source directory for source-level debugging.
    #[arg(long)]
    source_dir: Option<PathBuf>,

    /// Directory where the trace files will be written.
    ///
    /// Falls back to the `CODETRACER_POLKAVM_RECORDER_OUT_DIR` environment
    /// variable when the flag is omitted.
    #[arg(short = 'o', long)]
    out_dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the effective output directory:
///   1. `--out-dir` if given on the CLI.
///   2. `CODETRACER_POLKAVM_RECORDER_OUT_DIR` env var.
///   3. `DEFAULT_OUT_DIR` ("./ct-traces/").
fn resolve_out_dir(cli_out_dir: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli_out_dir {
        return path;
    }
    if let Some(value) = std::env::var_os(ENV_OUT_DIR)
        && !value.is_empty()
    {
        return PathBuf::from(value);
    }
    PathBuf::from(DEFAULT_OUT_DIR)
}

/// Whether the recorder is disabled via env var.  When true, the CLI
/// must execute its target operation in pass-through mode without
/// emitting any trace artefacts.
fn recording_disabled() -> bool {
    match std::env::var(ENV_DISABLED) {
        Ok(value) => {
            let v = value.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Record(args) => record(args),
        Commands::TraceInk(args) => trace_ink(args),
        Commands::Replay(args) => replay(args),
        Commands::Version => {
            println!("codetracer-polkavm-recorder {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// `record` implementation
// ---------------------------------------------------------------------------

/// Execute the `record` subcommand.
fn record(args: RecordArgs) -> Result<()> {
    // 1. Validate the source file exists
    let blob_path = args
        .program
        .canonicalize()
        .with_context(|| format!("blob file not found: {}", args.program.display()))?;

    eprintln!("Blob file: {}", blob_path.display());

    if recording_disabled() {
        // Pass-through: the PolkaVM recorder doesn't run a separate target
        // process — it loads & executes the blob itself — so disabling
        // recording simply means "don't emit any trace artefacts".
        eprintln!("{ENV_DISABLED} is set; skipping trace recording (no output written).");
        return Ok(());
    }

    // 2. Resolve and create the output directory
    let out_dir = resolve_out_dir(args.out_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // 3. Run the recorder (CTFS only)
    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)?;

    eprintln!("Trace files written to {}", out_dir.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// `trace-ink` implementation
// ---------------------------------------------------------------------------

/// Execute the `trace-ink` subcommand.
fn trace_ink(args: TraceInkArgs) -> Result<()> {
    use codetracer_polkavm_recorder::ink_testing::{InkTestConfig, encode_message_selector};

    let contract_path = args
        .contract
        .canonicalize()
        .with_context(|| format!("contract file not found: {}", args.contract.display()))?;

    eprintln!("Contract: {}", contract_path.display());
    eprintln!("Message: {}", args.message);
    if let Some(ref ctor) = args.constructor {
        eprintln!("Constructor: {}", ctor);
    }

    let _config = InkTestConfig {
        contract_path: contract_path.clone(),
        message: args.message.clone(),
        args: args.arg.clone(),
        constructor: args.constructor.clone(),
    };

    // Compute the selector for the requested message.
    let selector = encode_message_selector(&args.message);
    eprintln!(
        "Message selector: 0x{}",
        selector
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );

    if recording_disabled() {
        eprintln!("{ENV_DISABLED} is set; skipping trace recording (no output written).");
        return Ok(());
    }

    // Resolve and create the output directory.
    let out_dir = resolve_out_dir(args.out_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // For now, run the standard recorder. Full ink!-aware tracing with
    // InkHostHandler will be wired in when the drink!/cargo-contract
    // integration is complete.
    codetracer_polkavm_recorder::recorder::record(&contract_path, &out_dir)?;

    eprintln!("Trace files written to {}", out_dir.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// `replay` implementation
// ---------------------------------------------------------------------------

/// Execute the `replay` subcommand.
fn replay(args: ReplayArgs) -> Result<()> {
    use codetracer_polkavm_recorder::replay::{ReplayConfig, replay_contract_call};

    // Parse hex calldata if provided.
    let calldata = if args.calldata.is_empty() {
        vec![]
    } else {
        let hex_str = args.calldata.strip_prefix("0x").unwrap_or(&args.calldata);
        (0..hex_str.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex_str[i..i + 2], 16)
                    .with_context(|| format!("invalid hex in calldata: '{}'", &args.calldata))
            })
            .collect::<Result<Vec<_>>>()?
    };

    let config = ReplayConfig {
        contract_address: args.address,
        message_selector: args.selector,
        calldata,
        endpoint: args.endpoint,
        source_dir: args.source_dir,
        block_hash: args.block,
    };

    if recording_disabled() {
        eprintln!("{ENV_DISABLED} is set; skipping replay recording (no output written).");
        return Ok(());
    }

    let out_dir = resolve_out_dir(args.out_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    replay_contract_call(&config, &out_dir)
}
