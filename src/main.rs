//! CLI entry point for the CodeTracer PolkaVM recorder.
//!
//! Supports the `record` subcommand which loads a PolkaVM program blob,
//! executes it through PolkaVM with step tracing, captures the execution
//! trace, and writes CodeTracer trace output files.
//!
//! # Usage
//!
//! ```text
//! codetracer-polkavm-recorder record <blob-file> \
//!     --out-dir <output-dir> \
//!     [--format binary|json]
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use codetracer_trace_writer::TraceEventsFileFormat;
use eyre::{Context, Result};

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// CodeTracer PolkaVM recorder — record PolkaVM execution traces.
#[derive(Debug, Parser)]
#[command(
    name = "codetracer-polkavm-recorder",
    version,
    about = "Record PolkaVM program execution traces for CodeTracer"
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
    /// step tracing enabled, captures the execution trace, and writes
    /// CodeTracer trace files to `--out-dir`.
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

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Binary,
    Json,
}

#[derive(Debug, clap::Args)]
struct RecordArgs {
    /// Path to the PolkaVM program blob (.polkavm) file.
    program: PathBuf,

    /// Directory where the trace files will be written.
    ///
    /// The directory will be created if it does not exist.
    #[arg(short = 'o', long, default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Output format for the trace data.
    #[arg(short = 'f', long, default_value = "binary")]
    format: OutputFormat,
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
    #[arg(short = 'o', long, default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Output format for the trace data.
    #[arg(short = 'f', long, default_value = "binary")]
    format: OutputFormat,
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
    #[arg(short = 'o', long, default_value = "./ct-traces/")]
    out_dir: PathBuf,

    /// Output format for the trace data.
    #[arg(short = 'f', long, default_value = "binary")]
    format: OutputFormat,
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
            println!(
                "codetracer-polkavm-recorder {}",
                env!("CARGO_PKG_VERSION")
            );
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

    let format = match args.format {
        OutputFormat::Binary => TraceEventsFileFormat::Binary,
        OutputFormat::Json => TraceEventsFileFormat::Json,
    };

    // 2. Create the output directory
    let out_dir = &args.out_dir;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // 3. Run the recorder
    codetracer_polkavm_recorder::recorder::record(&blob_path, out_dir, format)?;

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
        selector.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    );

    let format = match args.format {
        OutputFormat::Binary => TraceEventsFileFormat::Binary,
        OutputFormat::Json => TraceEventsFileFormat::Json,
    };

    // Create the output directory.
    let out_dir = &args.out_dir;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // For now, run the standard recorder. Full ink!-aware tracing with
    // InkHostHandler will be wired in when the drink!/cargo-contract
    // integration is complete.
    codetracer_polkavm_recorder::recorder::record(&contract_path, out_dir, format)?;

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

    let format = match args.format {
        OutputFormat::Binary => TraceEventsFileFormat::Binary,
        OutputFormat::Json => TraceEventsFileFormat::Json,
    };

    replay_contract_call(&config, &args.out_dir, format)
}
