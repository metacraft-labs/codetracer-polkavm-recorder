//! Solidity-via-Revive support for PolkaVM tracing.
//!
//! The [Revive](https://github.com/matter-labs/revive) project provides `resolc`,
//! a Solidity compiler that targets PolkaVM (RISC-V) instead of the EVM. The
//! compilation pipeline is:
//!
//! ```text
//! Solidity source (.sol)
//!   → solc frontend (AST, type checking)
//!   → Revive / resolc (LLVM IR generation)
//!   → LLVM (RISC-V machine code)
//!   → polkavm-linker (PolkaVM program blob)
//! ```
//!
//! The resulting `.polkavm` blob is structurally identical to one produced from
//! Rust/ink!, but carries Solidity-specific patterns:
//!
//! - **Export names**: Revive emits `call` and `deploy` exports instead of Rust's
//!   `main`. The `deploy` export corresponds to the constructor, and `call`
//!   handles runtime dispatch.
//! - **Function selectors**: Solidity dispatches public functions by the first 4
//!   bytes of `keccak256(signature)`. The generated code reads 4 bytes from call
//!   data and branches on these selectors.
//! - **Host functions**: Solidity contracts use the same pallet-revive host
//!   functions as ink!, but the mapping from Solidity concepts differs:
//!   - `SLOAD` / `SSTORE` → `seal_get_storage` / `seal_set_storage`
//!   - `LOG0`..`LOG4` → `seal_deposit_event`
//!   - `CALL` / `DELEGATECALL` → `seal_call` / `seal_delegate_call`
//!   - `BALANCE` → `seal_balance`
//!   - `CALLER` → `seal_caller` (i.e. `msg.sender`)
//!   - `CALLVALUE` → `seal_value_transferred` (i.e. `msg.value`)
//!
//! # Debug info
//!
//! When compiled with `resolc --debug-info`, the blob may contain DWARF-style
//! line programs mapping PolkaVM PCs back to `.sol` source lines. The standard
//! [`SourceMapper`](crate::source_map::SourceMapper) handles this transparently
//! because PolkaVM's `LineProgram` format is compiler-agnostic.
//!
//! This module provides additional Solidity-aware heuristics: blob detection,
//! Solidity-specific source map enrichment, and EVM-opcode-to-host-function
//! mapping for more readable trace output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use polkavm::ProgramBlob;
use polkavm_common::program::ProgramCounter;

// ---------------------------------------------------------------------------
// Solidity blob detection
// ---------------------------------------------------------------------------

/// Heuristically determine whether a PolkaVM blob was compiled from Solidity
/// via `resolc`.
///
/// Detection strategy (any match is sufficient):
/// 1. The blob exports `call` and/or `deploy` (Revive convention) and does
///    **not** export `main` (Rust convention).
/// 2. Debug line-program paths reference `.sol` files.
pub fn detect_solidity_blob(blob: &ProgramBlob) -> bool {
    let mut has_call = false;
    let mut has_deploy = false;
    let mut has_main = false;

    for export in blob.exports() {
        let name = String::from_utf8_lossy(export.symbol().as_bytes());
        match name.as_ref() {
            "call" => has_call = true,
            "deploy" => has_deploy = true,
            "main" => has_main = true,
            _ => {}
        }
    }

    // Primary heuristic: Revive exports `call`/`deploy`, not `main`.
    if (has_call || has_deploy) && !has_main {
        return true;
    }

    // Secondary heuristic: look for .sol paths in debug line programs.
    if has_solidity_debug_paths(blob) {
        return true;
    }

    false
}

/// Check whether any debug line-program frame references a `.sol` source file.
fn has_solidity_debug_paths(blob: &ProgramBlob) -> bool {
    for parsed in blob.instructions() {
        let pc = parsed.offset;
        if let Ok(Some(mut line_program)) = blob.get_debug_line_program_at(pc) {
            while let Ok(Some(region_info)) = line_program.run() {
                for frame in region_info.frames() {
                    if let Ok(Some(path)) = frame.path() {
                        if path.ends_with(".sol") {
                            return true;
                        }
                    }
                }
                // Only check a handful of regions per PC to avoid full scan.
                break;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Solidity source map
// ---------------------------------------------------------------------------

/// A source location for Solidity code.
#[derive(Debug, Clone)]
pub struct SoliditySourceLocation {
    /// Path to the `.sol` source file.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: u32,
    /// Optional Solidity-level context (e.g. "constructor", "function transfer").
    pub context: Option<String>,
}

/// Source map enriched with Solidity-specific information.
///
/// Wraps the standard line-program source mapping with additional knowledge
/// about Solidity constructs: constructor vs runtime code regions, function
/// selector dispatch, etc.
pub struct SoliditySourceMap {
    /// PC → source location, extracted from debug line programs.
    locations: HashMap<u32, SoliditySourceLocation>,
    /// PC range for the deploy (constructor) entry point, if detected.
    pub deploy_range: Option<(u32, u32)>,
    /// PC range for the call (runtime) entry point, if detected.
    pub call_range: Option<(u32, u32)>,
    /// Detected function selectors mapped to their entry PCs.
    /// Key: 4-byte selector, Value: PC of the selector's handler.
    pub selectors: HashMap<[u8; 4], u32>,
}

impl SoliditySourceMap {
    /// Create an empty Solidity source map.
    pub fn empty() -> Self {
        Self {
            locations: HashMap::new(),
            deploy_range: None,
            call_range: None,
            selectors: HashMap::new(),
        }
    }

    /// Returns the number of mapped locations.
    pub fn location_count(&self) -> usize {
        self.locations.len()
    }

    /// Resolve a program counter to a Solidity source location.
    pub fn resolve(&self, pc: ProgramCounter) -> Option<(&Path, u32)> {
        self.locations
            .get(&pc.0)
            .map(|loc| (loc.path.as_path(), loc.line))
    }

    /// Determine whether a PC is inside the constructor (deploy) region.
    pub fn is_constructor(&self, pc: ProgramCounter) -> bool {
        self.deploy_range
            .map(|(start, end)| pc.0 >= start && pc.0 < end)
            .unwrap_or(false)
    }

    /// Determine whether a PC is inside the runtime (call) region.
    pub fn is_runtime(&self, pc: ProgramCounter) -> bool {
        self.call_range
            .map(|(start, end)| pc.0 >= start && pc.0 < end)
            .unwrap_or(false)
    }
}

/// Parse Solidity-specific debug information from a PolkaVM blob.
///
/// If the blob contains Solidity debug annotations (`.sol` paths in line
/// programs, `call`/`deploy` exports), this builds a [`SoliditySourceMap`]
/// with enriched context. Returns `None` if the blob does not appear to be
/// a Solidity-compiled blob.
pub fn parse_resolc_debug_info(blob: &ProgramBlob) -> Option<SoliditySourceMap> {
    if !detect_solidity_blob(blob) {
        return None;
    }

    let mut locations = HashMap::new();
    let mut deploy_range: Option<(u32, u32)> = None;
    let mut call_range: Option<(u32, u32)> = None;

    // --- Determine deploy/call ranges from exports ---
    let mut exports: Vec<(u32, String)> = blob
        .exports()
        .map(|e| {
            let name = String::from_utf8_lossy(e.symbol().as_bytes()).to_string();
            (e.program_counter().0, name)
        })
        .collect();
    exports.sort_by_key(|(pc, _)| *pc);

    for i in 0..exports.len() {
        let (pc, ref name) = exports[i];
        let end_pc = exports.get(i + 1).map(|(next, _)| *next);
        match name.as_str() {
            "deploy" => deploy_range = Some((pc, end_pc.unwrap_or(u32::MAX))),
            "call" => call_range = Some((pc, end_pc.unwrap_or(u32::MAX))),
            _ => {}
        }
    }

    // --- Walk debug line programs for source locations ---
    for parsed in blob.instructions() {
        let pc = parsed.offset;
        if let Ok(Some(mut line_program)) = blob.get_debug_line_program_at(pc) {
            while let Ok(Some(region_info)) = line_program.run() {
                let range = region_info.instruction_range();
                if pc >= range.start && pc < range.end {
                    if let Some(frame) = region_info.frames().last() {
                        let path = frame
                            .path()
                            .ok()
                            .flatten()
                            .map(PathBuf::from)
                            .unwrap_or_default();
                        let line = frame.line().unwrap_or(0);

                        if line > 0 {
                            // Determine Solidity-level context.
                            let context = if deploy_range
                                .map(|(s, e)| pc.0 >= s && pc.0 < e)
                                .unwrap_or(false)
                            {
                                Some("constructor".to_string())
                            } else {
                                frame
                                    .function_name_without_namespace()
                                    .ok()
                                    .flatten()
                                    .map(|n| n.to_string())
                            };

                            locations.insert(
                                pc.0,
                                SoliditySourceLocation {
                                    path,
                                    line,
                                    context,
                                },
                            );
                        }
                    }
                    break;
                }
            }
        }
    }

    Some(SoliditySourceMap {
        locations,
        deploy_range,
        call_range,
        selectors: HashMap::new(), // Selector detection requires code analysis.
    })
}

// ---------------------------------------------------------------------------
// Solidity host function mapping
// ---------------------------------------------------------------------------

/// Map a pallet-revive host function name to its Solidity/EVM equivalent.
///
/// This provides more intuitive names in trace output for developers
/// familiar with Solidity rather than pallet-revive internals.
pub fn solidity_host_function_alias(seal_name: &str) -> Option<&'static str> {
    match seal_name {
        "seal_get_storage" => Some("SLOAD"),
        "seal_set_storage" => Some("SSTORE"),
        "seal_clear_storage" => Some("SSTORE(0)"),
        "seal_contains_storage" => Some("SLOAD?"),
        "seal_deposit_event" => Some("LOG"),
        "seal_call" => Some("CALL"),
        "seal_delegate_call" => Some("DELEGATECALL"),
        "seal_instantiate" => Some("CREATE"),
        "seal_terminate" => Some("SELFDESTRUCT"),
        "seal_transfer" => Some("TRANSFER"),
        "seal_balance" => Some("BALANCE"),
        "seal_address" => Some("ADDRESS"),
        "seal_caller" => Some("CALLER"),
        "seal_value_transferred" => Some("CALLVALUE"),
        "seal_gas_left" => Some("GAS"),
        "seal_block_number" => Some("NUMBER"),
        "seal_now" => Some("TIMESTAMP"),
        "seal_minimum_balance" => Some("MINIMUM_BALANCE"),
        "seal_input" => Some("CALLDATALOAD"),
        "seal_return" => Some("RETURN"),
        "seal_hash_sha2_256" => Some("SHA256"),
        "seal_hash_keccak_256" => Some("KECCAK256"),
        "seal_hash_blake2_256" => Some("BLAKE2"),
        "seal_hash_blake2_128" => Some("BLAKE2_128"),
        "seal_code_hash" => Some("EXTCODEHASH"),
        "seal_own_code_hash" => Some("CODEHASH"),
        "seal_caller_is_origin" => Some("ORIGIN_CHECK"),
        "seal_debug_message" => Some("DEBUG"),
        _ => None,
    }
}

/// Format a display name for a host function call when tracing a Solidity blob.
///
/// Returns a combined name like `"SLOAD (seal_get_storage)"` for clarity,
/// or the plain seal name if no Solidity alias exists.
pub fn solidity_ecalli_display_name(seal_name: &str) -> String {
    match solidity_host_function_alias(seal_name) {
        Some(evm_name) => format!("{} ({})", evm_name, seal_name),
        None => seal_name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Blob detection tests -----------------------------------------------

    #[test]
    fn test_detect_solidity_blob_negative_with_rust_blob() {
        // A blob with a `main` export (typical Rust program) should not be
        // detected as Solidity.
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"main");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        assert!(
            !detect_solidity_blob(&blob),
            "Rust blob with 'main' export should not be detected as Solidity"
        );
    }

    #[test]
    fn test_detect_solidity_blob_positive_with_call_export() {
        // A blob with `call` export and no `main` should be detected.
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"call");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        assert!(
            detect_solidity_blob(&blob),
            "Blob with 'call' export and no 'main' should be detected as Solidity"
        );
    }

    #[test]
    fn test_detect_solidity_blob_positive_with_deploy_export() {
        // A blob with `deploy` export and no `main` should be detected.
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"deploy");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        assert!(
            detect_solidity_blob(&blob),
            "Blob with 'deploy' export and no 'main' should be detected as Solidity"
        );
    }

    #[test]
    fn test_detect_solidity_blob_negative_with_call_and_main() {
        // A blob with both `call` and `main` should NOT be detected
        // (ambiguous — could be a Rust program that happens to export `call`).
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"main");
        builder.add_export_by_basic_block(0, b"call");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        assert!(
            !detect_solidity_blob(&blob),
            "Blob with both 'main' and 'call' should not be detected as Solidity"
        );
    }

    // -- Source map parsing tests -------------------------------------------

    #[test]
    fn test_parse_resolc_debug_info_returns_none_for_rust_blob() {
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"main");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        assert!(
            parse_resolc_debug_info(&blob).is_none(),
            "Rust blob should not produce a SoliditySourceMap"
        );
    }

    #[test]
    fn test_parse_resolc_debug_info_returns_some_for_solidity_blob() {
        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"call");
        builder.set_code(&[asm::load_imm(A0, 0), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        let source_map = parse_resolc_debug_info(&blob);
        assert!(
            source_map.is_some(),
            "Solidity blob should produce a SoliditySourceMap"
        );

        let source_map = source_map.unwrap();
        // The mock blob has no debug line programs, so no locations are mapped,
        // but the call_range should be detected from the export.
        assert!(source_map.call_range.is_some());
    }

    #[test]
    fn test_solidity_source_map_constructor_detection() {
        let mut map = SoliditySourceMap::empty();
        map.deploy_range = Some((0, 50));
        map.call_range = Some((50, 200));

        assert!(map.is_constructor(ProgramCounter(0)));
        assert!(map.is_constructor(ProgramCounter(25)));
        assert!(!map.is_constructor(ProgramCounter(50)));
        assert!(!map.is_constructor(ProgramCounter(100)));

        assert!(!map.is_runtime(ProgramCounter(0)));
        assert!(map.is_runtime(ProgramCounter(50)));
        assert!(map.is_runtime(ProgramCounter(100)));
        assert!(!map.is_runtime(ProgramCounter(200)));
    }

    // -- Host function alias tests ------------------------------------------

    #[test]
    fn test_solidity_host_function_aliases() {
        assert_eq!(
            solidity_host_function_alias("seal_get_storage"),
            Some("SLOAD")
        );
        assert_eq!(
            solidity_host_function_alias("seal_set_storage"),
            Some("SSTORE")
        );
        assert_eq!(
            solidity_host_function_alias("seal_deposit_event"),
            Some("LOG")
        );
        assert_eq!(solidity_host_function_alias("seal_call"), Some("CALL"));
        assert_eq!(
            solidity_host_function_alias("seal_delegate_call"),
            Some("DELEGATECALL")
        );
        assert_eq!(solidity_host_function_alias("seal_caller"), Some("CALLER"));
        assert_eq!(
            solidity_host_function_alias("seal_value_transferred"),
            Some("CALLVALUE")
        );
        assert_eq!(
            solidity_host_function_alias("seal_balance"),
            Some("BALANCE")
        );
        assert_eq!(solidity_host_function_alias("seal_input"), Some("CALLDATALOAD"));
        assert_eq!(solidity_host_function_alias("seal_return"), Some("RETURN"));
    }

    #[test]
    fn test_solidity_host_function_alias_unknown() {
        assert_eq!(solidity_host_function_alias("unknown_function"), None);
        assert_eq!(solidity_host_function_alias(""), None);
    }

    #[test]
    fn test_solidity_ecalli_display_name() {
        assert_eq!(
            solidity_ecalli_display_name("seal_get_storage"),
            "SLOAD (seal_get_storage)"
        );
        assert_eq!(
            solidity_ecalli_display_name("seal_set_storage"),
            "SSTORE (seal_set_storage)"
        );
        assert_eq!(
            solidity_ecalli_display_name("unknown_fn"),
            "unknown_fn"
        );
    }

    // -- Rust blob still works with existing infrastructure -----------------

    #[test]
    fn test_rust_blob_source_mapper_still_works() {
        // Verify that the standard SourceMapper and DwarfVariableInfo work
        // correctly with a Rust blob (regression check).
        use crate::dwarf_variables::DwarfVariableInfo;
        use crate::source_map::SourceMapper;

        use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"main");
        builder.set_code(
            &[asm::load_imm(A0, 42), asm::ret()],
            &[],
        );
        let blob_bytes = builder.into_vec().expect("build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("parse blob");

        // Standard source mapper should work (will be empty since no debug info).
        let mapper = SourceMapper::from_blob(&blob);
        assert_eq!(mapper.location_count(), 0);

        // DWARF variable info should detect the `main` export.
        let var_info = DwarfVariableInfo::from_blob(&blob);
        assert_eq!(var_info.function_count(), 1);

        // Solidity detection should be negative.
        assert!(!detect_solidity_blob(&blob));
    }
}
