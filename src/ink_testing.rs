//! ink! contract testing infrastructure.
//!
//! Provides types and utilities for tracing ink! smart contract execution
//! in a simulated environment, without requiring the full Substrate runtime.
//! This is the foundation for the M7 milestone: "ink! Contract Testing via
//! drink! and cargo-contract".
//!
//! # Overview
//!
//! - Parse ink! contract metadata JSON to extract messages, constructors,
//!   and selectors.
//! - Encode message selectors using the Blake2 hash scheme ink! uses.
//! - Simulate host function behavior (storage, input, return) via
//!   [`InkHostHandler`], which implements [`HostFunctionHandler`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::{eyre, Context, Result};
use serde::Deserialize;

use crate::host_functions::HostFunctionHandler;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for an ink! contract test run.
#[derive(Debug, Clone)]
pub struct InkTestConfig {
    /// Path to the compiled contract (.contract or .polkavm blob).
    pub contract_path: PathBuf,
    /// Name of the message to invoke (e.g. "get", "flip").
    pub message: String,
    /// SCALE-encoded argument bytes (one entry per argument).
    pub args: Vec<String>,
    /// Optional constructor name. When set, the contract is first
    /// instantiated via this constructor before calling `message`.
    pub constructor: Option<String>,
}

// ---------------------------------------------------------------------------
// Metadata types
// ---------------------------------------------------------------------------

/// High-level information about an ink! contract, extracted from its
/// metadata JSON.
#[derive(Debug, Clone)]
pub struct InkContractInfo {
    /// Human-readable contract name.
    pub name: String,
    /// Callable messages exposed by the contract.
    pub messages: Vec<InkMessage>,
    /// Constructors for instantiating the contract.
    pub constructors: Vec<InkConstructor>,
}

/// A callable message defined in an ink! contract.
#[derive(Debug, Clone)]
pub struct InkMessage {
    /// The message name (e.g. "flip", "get").
    pub name: String,
    /// The 4-byte selector used to dispatch the message.
    pub selector: [u8; 4],
    /// Argument names (types are erased here; full type info lives in
    /// the metadata JSON's type registry).
    pub args: Vec<String>,
}

/// A constructor defined in an ink! contract.
#[derive(Debug, Clone)]
pub struct InkConstructor {
    /// Constructor name (e.g. "new", "default").
    pub name: String,
    /// The 4-byte selector.
    pub selector: [u8; 4],
    /// Argument names.
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// Metadata JSON shapes (serde)
// ---------------------------------------------------------------------------

/// Top-level ink! metadata JSON structure (V3+).
#[derive(Deserialize)]
struct RawInkMetadata {
    contract: RawContract,
    spec: RawSpec,
}

#[derive(Deserialize)]
struct RawContract {
    name: String,
}

#[derive(Deserialize)]
struct RawSpec {
    constructors: Vec<RawCallable>,
    messages: Vec<RawCallable>,
}

#[derive(Deserialize)]
struct RawCallable {
    label: String,
    selector: String,
    args: Vec<RawArg>,
}

#[derive(Deserialize)]
struct RawArg {
    label: String,
}

// ---------------------------------------------------------------------------
// Metadata parsing
// ---------------------------------------------------------------------------

/// Parse an ink! contract metadata JSON file and return structured
/// contract information.
///
/// Supports the V3+ metadata layout where `contract.name`, `spec.messages`,
/// and `spec.constructors` are at the top level.
pub fn parse_ink_metadata(metadata_path: &Path) -> Result<InkContractInfo> {
    let content = std::fs::read_to_string(metadata_path)
        .with_context(|| format!("failed to read metadata file: {}", metadata_path.display()))?;

    parse_ink_metadata_str(&content)
}

/// Parse ink! metadata from a JSON string (useful for testing without files).
pub fn parse_ink_metadata_str(json: &str) -> Result<InkContractInfo> {
    let raw: RawInkMetadata =
        serde_json::from_str(json).with_context(|| "failed to parse ink! metadata JSON")?;

    let messages = raw
        .spec
        .messages
        .iter()
        .map(|m| {
            Ok(InkMessage {
                name: m.label.clone(),
                selector: parse_selector_hex(&m.selector)?,
                args: m.args.iter().map(|a| a.label.clone()).collect(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let constructors = raw
        .spec
        .constructors
        .iter()
        .map(|c| {
            Ok(InkConstructor {
                name: c.label.clone(),
                selector: parse_selector_hex(&c.selector)?,
                args: c.args.iter().map(|a| a.label.clone()).collect(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(InkContractInfo {
        name: raw.contract.name,
        messages,
        constructors,
    })
}

/// Parse a hex-encoded selector string like "0xabcdef01" into 4 bytes.
fn parse_selector_hex(hex_str: &str) -> Result<[u8; 4]> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if hex_str.len() != 8 {
        return Err(eyre!(
            "selector hex must be 8 hex chars, got '{}' ({})",
            hex_str,
            hex_str.len()
        ));
    }
    let bytes = (0..4)
        .map(|i| {
            u8::from_str_radix(&hex_str[i * 2..i * 2 + 2], 16)
                .with_context(|| format!("invalid hex in selector: '{}'", hex_str))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// ---------------------------------------------------------------------------
// Selector encoding
// ---------------------------------------------------------------------------

/// Compute the ink! message selector for a given message name.
///
/// ink! selectors are the first 4 bytes of the Blake2b-256 hash of the
/// message name (as UTF-8 bytes). This matches the `#[ink(message)]`
/// default selector computation.
pub fn encode_message_selector(name: &str) -> [u8; 4] {
    use blake2::digest::typenum::U32;
    use blake2::digest::Digest;
    let hash = blake2::Blake2b::<U32>::digest(name.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

// ---------------------------------------------------------------------------
// InkHostHandler — simulated host functions
// ---------------------------------------------------------------------------

/// A [`HostFunctionHandler`] that simulates ink! host function behaviour
/// for contract testing.
///
/// Maintains an in-memory key-value store for contract storage and
/// captures message input/return values.
pub struct InkHostHandler {
    /// Simulated contract storage (key → value).
    storage: HashMap<Vec<u8>, Vec<u8>>,
    /// The encoded message input (selector + SCALE-encoded args) that
    /// `seal_input` returns to the contract.
    input_data: Vec<u8>,
    /// The return value captured from `seal_return`.
    return_data: Option<Vec<u8>>,
}

impl InkHostHandler {
    /// Create a new handler with the given input data.
    ///
    /// `input_data` is typically the 4-byte selector followed by
    /// SCALE-encoded arguments.
    pub fn new(input_data: Vec<u8>) -> Self {
        Self {
            storage: HashMap::new(),
            input_data,
            return_data: None,
        }
    }

    /// Access the simulated storage map.
    pub fn storage(&self) -> &HashMap<Vec<u8>, Vec<u8>> {
        &self.storage
    }

    /// Access the captured return data (set by `seal_return`).
    pub fn return_data(&self) -> Option<&[u8]> {
        self.return_data.as_deref()
    }

    /// Insert a value into the simulated storage (useful for test setup).
    pub fn set_storage(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.storage.insert(key, value);
    }

    // -- helpers for reading/writing guest memory via PolkaVM registers ------

    /// Read `len` bytes from guest memory starting at `ptr`.
    fn read_guest_memory(instance: &mut polkavm::RawInstance, ptr: u32, len: u32) -> Vec<u8> {
        instance.read_memory(ptr, len).unwrap_or_default()
    }

    /// Write `data` into guest memory at `ptr`.
    fn write_guest_memory(instance: &mut polkavm::RawInstance, ptr: u32, data: &[u8]) {
        instance.write_memory(ptr, data).ok();
    }
}

impl HostFunctionHandler for InkHostHandler {
    fn handle_ecalli(&mut self, index: u32, instance: &mut polkavm::RawInstance) -> bool {
        use polkavm::Reg;

        match index {
            // -- seal_input (index 0) ----------------------------------------
            // Contract calls seal_input(out_ptr, out_len_ptr) to receive
            // the encoded message selector + arguments.
            0 => {
                let out_ptr = instance.reg(Reg::A0) as u32;
                let out_len_ptr = instance.reg(Reg::A1) as u32;

                let data = &self.input_data;
                Self::write_guest_memory(instance, out_ptr, data);

                // Write the length as a little-endian u32 at out_len_ptr.
                let len_bytes = (data.len() as u32).to_le_bytes();
                Self::write_guest_memory(instance, out_len_ptr, &len_bytes);

                // Return success (0) in A0.
                instance.set_reg(Reg::A0, 0);
                true
            }

            // -- seal_return (index 1) ---------------------------------------
            // Contract calls seal_return(flags, data_ptr, data_len).
            1 => {
                let data_ptr = instance.reg(Reg::A1) as u32;
                let data_len = instance.reg(Reg::A2) as u32;

                let data = Self::read_guest_memory(instance, data_ptr, data_len);
                self.return_data = Some(data);

                // seal_return terminates execution; we return false to
                // signal the tracer to stop the step loop.
                false
            }

            // -- seal_get_storage (index 5) ----------------------------------
            // seal_get_storage(key_ptr, key_len, out_ptr, out_len_ptr) -> status
            5 => {
                let key_ptr = instance.reg(Reg::A0) as u32;
                let key_len = instance.reg(Reg::A1) as u32;
                let out_ptr = instance.reg(Reg::A2) as u32;
                let out_len_ptr = instance.reg(Reg::A3) as u32;

                let key = Self::read_guest_memory(instance, key_ptr, key_len);

                if let Some(value) = self.storage.get(&key) {
                    Self::write_guest_memory(instance, out_ptr, value);
                    let len_bytes = (value.len() as u32).to_le_bytes();
                    Self::write_guest_memory(instance, out_len_ptr, &len_bytes);
                    // Return success (0).
                    instance.set_reg(Reg::A0, 0);
                } else {
                    // Key not found — return error code 1.
                    instance.set_reg(Reg::A0, 1);
                }
                true
            }

            // -- seal_set_storage (index 6) ----------------------------------
            // seal_set_storage(key_ptr, key_len, value_ptr, value_len) -> status
            6 => {
                let key_ptr = instance.reg(Reg::A0) as u32;
                let key_len = instance.reg(Reg::A1) as u32;
                let value_ptr = instance.reg(Reg::A2) as u32;
                let value_len = instance.reg(Reg::A3) as u32;

                let key = Self::read_guest_memory(instance, key_ptr, key_len);
                let value = Self::read_guest_memory(instance, value_ptr, value_len);

                let was_set = self.storage.contains_key(&key);
                self.storage.insert(key, value);

                // Return 0 if the key was newly written, 1 if overwritten.
                instance.set_reg(Reg::A0, if was_set { 1 } else { 0 });
                true
            }

            // -- all other known host functions: no-op -----------------------
            idx if crate::host_functions::resolve_host_function(idx).is_some() => {
                instance.set_reg(Reg::A0, 0);
                true
            }

            // -- unknown host function: halt ---------------------------------
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Metadata parsing tests ----------------------------------------------

    fn sample_metadata_json() -> &'static str {
        r#"{
            "contract": {
                "name": "flipper"
            },
            "spec": {
                "constructors": [
                    {
                        "label": "new",
                        "selector": "0x9bae9d5e",
                        "args": [
                            { "label": "init_value" }
                        ]
                    },
                    {
                        "label": "default",
                        "selector": "0xed4b9d1b",
                        "args": []
                    }
                ],
                "messages": [
                    {
                        "label": "flip",
                        "selector": "0x633aa551",
                        "args": []
                    },
                    {
                        "label": "get",
                        "selector": "0x2f865bd9",
                        "args": []
                    }
                ]
            }
        }"#
    }

    #[test]
    fn test_parse_ink_metadata_contract_name() {
        let info = parse_ink_metadata_str(sample_metadata_json()).unwrap();
        assert_eq!(info.name, "flipper");
    }

    #[test]
    fn test_parse_ink_metadata_messages() {
        let info = parse_ink_metadata_str(sample_metadata_json()).unwrap();
        assert_eq!(info.messages.len(), 2);

        let flip = &info.messages[0];
        assert_eq!(flip.name, "flip");
        assert_eq!(flip.selector, [0x63, 0x3a, 0xa5, 0x51]);
        assert!(flip.args.is_empty());

        let get = &info.messages[1];
        assert_eq!(get.name, "get");
        assert_eq!(get.selector, [0x2f, 0x86, 0x5b, 0xd9]);
    }

    #[test]
    fn test_parse_ink_metadata_constructors() {
        let info = parse_ink_metadata_str(sample_metadata_json()).unwrap();
        assert_eq!(info.constructors.len(), 2);

        let new_ctor = &info.constructors[0];
        assert_eq!(new_ctor.name, "new");
        assert_eq!(new_ctor.selector, [0x9b, 0xae, 0x9d, 0x5e]);
        assert_eq!(new_ctor.args, vec!["init_value"]);

        let default_ctor = &info.constructors[1];
        assert_eq!(default_ctor.name, "default");
        assert!(default_ctor.args.is_empty());
    }

    #[test]
    fn test_parse_ink_metadata_invalid_json() {
        let result = parse_ink_metadata_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_ink_metadata_missing_fields() {
        let result = parse_ink_metadata_str(r#"{"contract": {"name": "x"}}"#);
        assert!(result.is_err());
    }

    // -- Selector encoding tests ---------------------------------------------

    #[test]
    fn test_encode_message_selector_deterministic() {
        let sel1 = encode_message_selector("flip");
        let sel2 = encode_message_selector("flip");
        assert_eq!(sel1, sel2);
    }

    #[test]
    fn test_encode_message_selector_different_names() {
        let flip = encode_message_selector("flip");
        let get = encode_message_selector("get");
        assert_ne!(flip, get);
    }

    #[test]
    fn test_encode_message_selector_length() {
        let sel = encode_message_selector("any_message");
        assert_eq!(sel.len(), 4);
    }

    // -- InkHostHandler tests ------------------------------------------------

    #[test]
    fn test_ink_host_handler_storage_roundtrip() {
        let mut handler = InkHostHandler::new(vec![]);

        // Initially empty.
        assert!(handler.storage().is_empty());

        // Insert a key.
        handler.set_storage(vec![1, 2, 3], vec![10, 20, 30]);
        assert_eq!(
            handler.storage().get(&vec![1, 2, 3]),
            Some(&vec![10, 20, 30])
        );

        // Overwrite.
        handler.set_storage(vec![1, 2, 3], vec![99]);
        assert_eq!(handler.storage().get(&vec![1, 2, 3]), Some(&vec![99]));
    }

    #[test]
    fn test_ink_host_handler_return_data_initially_none() {
        let handler = InkHostHandler::new(vec![0xAA, 0xBB]);
        assert!(handler.return_data().is_none());
    }

    #[test]
    fn test_ink_host_handler_input_data_stored() {
        let input = vec![0x63, 0x3a, 0xa5, 0x51]; // "flip" selector
        let handler = InkHostHandler::new(input.clone());
        assert_eq!(handler.input_data, input);
    }

    #[test]
    fn test_ink_host_handler_multiple_storage_keys() {
        let mut handler = InkHostHandler::new(vec![]);

        handler.set_storage(vec![1], vec![10]);
        handler.set_storage(vec![2], vec![20]);
        handler.set_storage(vec![3], vec![30]);

        assert_eq!(handler.storage().len(), 3);
        assert_eq!(handler.storage().get(&vec![1]), Some(&vec![10]));
        assert_eq!(handler.storage().get(&vec![2]), Some(&vec![20]));
        assert_eq!(handler.storage().get(&vec![3]), Some(&vec![30]));
    }

    // -- Ecalli dispatch tests (without a full VM instance) -------------------
    // These verify the handler structure; full integration tests would need
    // a PolkaVM instance with guest memory.

    #[test]
    fn test_parse_selector_hex_valid() {
        let sel = parse_selector_hex("0xabcdef01").unwrap();
        assert_eq!(sel, [0xab, 0xcd, 0xef, 0x01]);
    }

    #[test]
    fn test_parse_selector_hex_no_prefix() {
        let sel = parse_selector_hex("abcdef01").unwrap();
        assert_eq!(sel, [0xab, 0xcd, 0xef, 0x01]);
    }

    #[test]
    fn test_parse_selector_hex_invalid_length() {
        assert!(parse_selector_hex("0xab").is_err());
        assert!(parse_selector_hex("0xabcdef0102").is_err());
    }

    #[test]
    fn test_parse_selector_hex_invalid_chars() {
        assert!(parse_selector_hex("0xzzzzzzzz").is_err());
    }
}
