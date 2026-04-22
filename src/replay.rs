//! On-chain contract replay via Substrate RPC.
//!
//! M8 milestone: fetches PolkaVM contracts from Substrate-based chains
//! and replays their execution locally with full step tracing.
//!
//! # Overview
//!
//! - Connect to a Substrate node via JSON-RPC endpoint.
//! - Fetch contract code using `contracts_getContractInfo` and
//!   `contracts_getContractCode` RPCs.
//! - Parse the code as a PolkaVM [`ProgramBlob`].
//! - Execute with [`InkHostHandler`] from M7 and record a CodeTracer trace.

use std::path::{Path, PathBuf};

use codetracer_trace_writer_nim::TraceEventsFileFormat;
use eyre::{eyre, Context, Result};
use polkavm::ProgramBlob;

use crate::ink_testing::{encode_message_selector, InkHostHandler};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A lightweight client handle for a Substrate JSON-RPC endpoint.
#[derive(Debug, Clone)]
pub struct SubstrateRpcClient {
    /// The WebSocket or HTTP endpoint URL (e.g. `ws://127.0.0.1:9944`).
    pub endpoint: String,
}

impl SubstrateRpcClient {
    /// Create a new client targeting the given endpoint.
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
        }
    }
}

/// Configuration for replaying a contract call fetched from the chain.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// On-chain contract address (SS58 or hex).
    pub contract_address: String,
    /// The ink! message selector name (e.g. "get", "flip").
    pub message_selector: String,
    /// Additional calldata bytes (SCALE-encoded arguments) appended
    /// after the 4-byte selector.
    pub calldata: Vec<u8>,
    /// Substrate RPC endpoint URL.
    pub endpoint: String,
    /// Optional path to source directory for source-level debugging.
    pub source_dir: Option<PathBuf>,
    /// Optional block hash at which to fetch the contract state.
    /// When `None`, the latest finalized block is used.
    pub block_hash: Option<String>,
}

/// The on-chain code for a deployed contract.
#[derive(Debug, Clone)]
pub struct ContractCode {
    /// The hash of the contract code (hex string).
    pub code_hash: String,
    /// The raw PolkaVM blob bytes.
    pub code: Vec<u8>,
}

// ---------------------------------------------------------------------------
// RPC interaction (placeholder)
// ---------------------------------------------------------------------------

/// Fetch the deployed contract code from the chain.
///
/// Uses the `contracts_getContractInfo` RPC to look up the code hash for
/// `address`, then `contracts_getContractCode` to retrieve the actual
/// PolkaVM blob bytes.
///
/// # Errors
///
/// Returns an error if the RPC call fails, the contract is not found, or
/// the returned data is not a valid PolkaVM blob.
///
/// **Note**: This is currently a placeholder. The real implementation will
/// use `jsonrpsee` or `subxt` to make the RPC calls.
pub fn fetch_contract_code(_client: &SubstrateRpcClient, _address: &str) -> Result<ContractCode> {
    // TODO(M8): Implement actual Substrate RPC calls:
    //   1. contracts_getContractInfo(address, block_hash) -> { code_hash, ... }
    //   2. contracts_getContractCode(code_hash) -> Vec<u8>
    Err(eyre!(
        "fetch_contract_code is not yet implemented — \
         requires a running Substrate node with pallet-contracts"
    ))
}

/// Fetch contract code, returning mock data when available for testing.
///
/// This wrapper tries `fetch_contract_code` first; if the real RPC is not
/// available it falls back to reading a local blob file when the address
/// looks like a file path (useful during development).
fn fetch_or_load_contract_code(client: &SubstrateRpcClient, address: &str) -> Result<ContractCode> {
    // Try the real RPC first.
    match fetch_contract_code(client, address) {
        Ok(code) => Ok(code),
        Err(_rpc_err) => {
            // Fall back: if address looks like a file path, load from disk.
            let path = Path::new(address);
            if path.exists() {
                let code = std::fs::read(path).with_context(|| {
                    format!("failed to read local blob file: {}", path.display())
                })?;
                Ok(ContractCode {
                    code_hash: format!("local:{}", path.display()),
                    code,
                })
            } else {
                Err(eyre!(
                    "cannot fetch contract code for '{}': \
                     RPC not implemented and address is not a local file",
                    address
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Replay pipeline
// ---------------------------------------------------------------------------

/// Replay a contract call: fetch code from the chain, parse it, and trace
/// its execution with the ink! host function handler.
///
/// The full pipeline:
/// 1. Connect to the Substrate RPC endpoint.
/// 2. Fetch the contract code for `config.contract_address`.
/// 3. Parse the code as a PolkaVM `ProgramBlob`.
/// 4. Build the input data (selector + calldata).
/// 5. Run the program through the tracer with [`InkHostHandler`].
/// 6. Write CodeTracer trace files to `out_dir`.
pub fn replay_contract_call(
    config: &ReplayConfig,
    out_dir: &Path,
    format: TraceEventsFileFormat,
) -> Result<()> {
    eprintln!("Replay: contract={}", config.contract_address);
    eprintln!("Replay: message={}", config.message_selector);
    eprintln!("Replay: endpoint={}", config.endpoint);
    if let Some(ref bh) = config.block_hash {
        eprintln!("Replay: block={}", bh);
    }

    // 1. Fetch contract code.
    let client = SubstrateRpcClient::new(&config.endpoint);
    let contract_code = fetch_or_load_contract_code(&client, &config.contract_address)?;

    eprintln!(
        "Replay: fetched {} bytes of code (hash: {})",
        contract_code.code.len(),
        contract_code.code_hash,
    );

    // 2. Validate the code parses as a ProgramBlob.
    let _blob = ProgramBlob::parse(contract_code.code.clone().into())
        .map_err(|e| eyre!("failed to parse contract code as PolkaVM blob: {e}"))?;

    // 3. Build input data: 4-byte selector + calldata.
    let selector = encode_message_selector(&config.message_selector);
    let mut input_data = selector.to_vec();
    input_data.extend_from_slice(&config.calldata);

    eprintln!(
        "Replay: input data ({} bytes): 0x{}",
        input_data.len(),
        input_data
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );

    // 4. Set up the host handler.
    let _host_handler = InkHostHandler::new(input_data);

    // 5. Create output directory and run the tracer.
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

    // For now, use the standard trace_program path. A future iteration
    // will wire InkHostHandler into the tracer so that seal_input returns
    // the correct selector + calldata and seal_return captures the result.
    //
    // We write the blob to a temp file so trace_program can use it as the
    // "source path" in the trace metadata.
    let blob_path = out_dir.join("replayed_contract.polkavm");
    std::fs::write(&blob_path, &contract_code.code)
        .with_context(|| "failed to write temporary blob file")?;

    crate::recorder::record(&blob_path, out_dir, format)?;

    eprintln!("Replay: trace files written to {}", out_dir.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Config construction tests -------------------------------------------

    #[test]
    fn test_replay_config_construction() {
        let config = ReplayConfig {
            contract_address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            message_selector: "get".into(),
            calldata: vec![0x01, 0x02],
            endpoint: "ws://127.0.0.1:9944".into(),
            source_dir: Some(PathBuf::from("/tmp/contract-src")),
            block_hash: Some("0xabc123".into()),
        };

        assert_eq!(
            config.contract_address,
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        );
        assert_eq!(config.message_selector, "get");
        assert_eq!(config.calldata, vec![0x01, 0x02]);
        assert_eq!(config.endpoint, "ws://127.0.0.1:9944");
        assert_eq!(config.source_dir, Some(PathBuf::from("/tmp/contract-src")));
        assert_eq!(config.block_hash, Some("0xabc123".into()));
    }

    #[test]
    fn test_replay_config_defaults() {
        let config = ReplayConfig {
            contract_address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            message_selector: "flip".into(),
            calldata: vec![],
            endpoint: "ws://127.0.0.1:9944".into(),
            source_dir: None,
            block_hash: None,
        };

        assert!(config.source_dir.is_none());
        assert!(config.block_hash.is_none());
        assert!(config.calldata.is_empty());
    }

    // -- SubstrateRpcClient tests --------------------------------------------

    #[test]
    fn test_substrate_rpc_client_new() {
        let client = SubstrateRpcClient::new("ws://localhost:9944");
        assert_eq!(client.endpoint, "ws://localhost:9944");
    }

    #[test]
    fn test_substrate_rpc_client_clone() {
        let client = SubstrateRpcClient::new("wss://rpc.polkadot.io");
        let cloned = client.clone();
        assert_eq!(client.endpoint, cloned.endpoint);
    }

    // -- ContractCode tests --------------------------------------------------

    #[test]
    fn test_contract_code_struct() {
        let code = ContractCode {
            code_hash: "0xdeadbeef".into(),
            code: vec![0x00, 0x61, 0x73, 0x6d],
        };
        assert_eq!(code.code_hash, "0xdeadbeef");
        assert_eq!(code.code.len(), 4);
    }

    // -- fetch_contract_code placeholder test --------------------------------

    #[test]
    fn test_fetch_contract_code_returns_error() {
        // The placeholder always returns an error since there is no
        // running Substrate node.
        let client = SubstrateRpcClient::new("ws://127.0.0.1:9944");
        let result = fetch_contract_code(&client, "5GrwvaEF");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not yet implemented"));
    }

    // -- Contract code parsing test ------------------------------------------

    #[test]
    fn test_contract_code_parse_invalid_blob() {
        // Random bytes should fail to parse as a PolkaVM blob.
        let bad_code = ContractCode {
            code_hash: "0xbad".into(),
            code: vec![0xFF, 0xFE, 0xFD, 0xFC],
        };
        let result = ProgramBlob::parse(bad_code.code.into());
        assert!(result.is_err());
    }

    // -- Replay pipeline test with mock data ---------------------------------

    #[test]
    fn test_replay_contract_call_no_rpc() {
        // Without a running Substrate node and without a local file,
        // replay_contract_call should fail gracefully.
        let config = ReplayConfig {
            contract_address: "nonexistent_address".into(),
            message_selector: "get".into(),
            calldata: vec![],
            endpoint: "ws://127.0.0.1:9944".into(),
            source_dir: None,
            block_hash: None,
        };

        let tmp = tempfile::tempdir().unwrap();
        let result = replay_contract_call(&config, tmp.path(), TraceEventsFileFormat::Json);

        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("cannot fetch contract code"));
    }

    #[test]
    fn test_replay_input_data_construction() {
        // Verify that selector + calldata are concatenated correctly.
        let selector = encode_message_selector("flip");
        let calldata: Vec<u8> = vec![0x01, 0x02, 0x03];

        let mut input_data = selector.to_vec();
        input_data.extend_from_slice(&calldata);

        // Should be 4 (selector) + 3 (calldata) = 7 bytes.
        assert_eq!(input_data.len(), 7);
        // First 4 bytes are the selector.
        assert_eq!(&input_data[..4], &selector);
        // Remaining bytes are the calldata.
        assert_eq!(&input_data[4..], &calldata[..]);
    }

    #[test]
    fn test_replay_input_data_no_calldata() {
        let selector = encode_message_selector("get");
        let input_data = selector.to_vec();

        // With no calldata, input is just the 4-byte selector.
        assert_eq!(input_data.len(), 4);
    }
}
