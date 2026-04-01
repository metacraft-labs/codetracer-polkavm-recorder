//! Host function resolution for pallet-revive Ecalli calls.
//!
//! Maps PolkaVM `ecalli` instruction indices to their pallet-revive
//! host function names. This is the foundation for M4 ink! v6 smart
//! contract debugging: when the VM hits an `ecalli`, we resolve the
//! human-readable name and emit proper Call/Return trace events.

/// Resolve an Ecalli index to the corresponding pallet-revive host function name.
///
/// The mapping follows the pallet-revive host function table used by
/// ink! v6 smart contracts. Returns `None` for unknown indices.
pub fn resolve_host_function(index: u32) -> Option<&'static str> {
    match index {
        0 => Some("seal_input"),
        1 => Some("seal_return"),
        2 => Some("seal_caller"),
        3 => Some("seal_value_transferred"),
        4 => Some("seal_deposit_event"),
        5 => Some("seal_get_storage"),
        6 => Some("seal_set_storage"),
        7 => Some("seal_call"),
        8 => Some("seal_instantiate"),
        9 => Some("seal_terminate"),
        10 => Some("seal_transfer"),
        11 => Some("seal_balance"),
        12 => Some("seal_address"),
        13 => Some("seal_gas_left"),
        14 => Some("seal_block_number"),
        15 => Some("seal_now"),
        16 => Some("seal_weight_to_fee"),
        17 => Some("seal_minimum_balance"),
        18 => Some("seal_hash_sha2_256"),
        19 => Some("seal_hash_keccak_256"),
        20 => Some("seal_hash_blake2_256"),
        21 => Some("seal_hash_blake2_128"),
        22 => Some("seal_clear_storage"),
        23 => Some("seal_contains_storage"),
        24 => Some("seal_code_hash"),
        25 => Some("seal_own_code_hash"),
        26 => Some("seal_caller_is_origin"),
        27 => Some("seal_caller_is_root"),
        28 => Some("seal_debug_message"),
        29 => Some("seal_delegate_call"),
        _ => None,
    }
}

/// Format a display name for an Ecalli call, falling back to the raw
/// index when the host function is not recognized.
pub fn ecalli_display_name(index: u32) -> String {
    match resolve_host_function(index) {
        Some(name) => name.to_string(),
        None => format!("ecalli_{}", index),
    }
}

/// Trait for implementing host function behavior during tracing.
///
/// In production, host functions require the full Substrate runtime.
/// This trait allows test harnesses to supply mock implementations
/// so that programs containing `ecalli` instructions can continue
/// execution instead of halting.
pub trait HostFunctionHandler {
    /// Handle an Ecalli call. Returns `true` if the call was handled
    /// and execution should continue, `false` to halt.
    fn handle_ecalli(
        &mut self,
        index: u32,
        instance: &mut polkavm::RawInstance,
    ) -> bool;
}

/// A default handler that allows all known host functions to proceed
/// as no-ops (the call is acknowledged but no side effects occur).
/// Unknown host functions cause execution to halt.
pub struct NoOpHostFunctionHandler;

impl HostFunctionHandler for NoOpHostFunctionHandler {
    fn handle_ecalli(
        &mut self,
        index: u32,
        _instance: &mut polkavm::RawInstance,
    ) -> bool {
        resolve_host_function(index).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_known_host_functions() {
        assert_eq!(resolve_host_function(0), Some("seal_input"));
        assert_eq!(resolve_host_function(1), Some("seal_return"));
        assert_eq!(resolve_host_function(2), Some("seal_caller"));
        assert_eq!(resolve_host_function(3), Some("seal_value_transferred"));
        assert_eq!(resolve_host_function(4), Some("seal_deposit_event"));
        assert_eq!(resolve_host_function(5), Some("seal_get_storage"));
        assert_eq!(resolve_host_function(6), Some("seal_set_storage"));
        assert_eq!(resolve_host_function(7), Some("seal_call"));
        assert_eq!(resolve_host_function(28), Some("seal_debug_message"));
        assert_eq!(resolve_host_function(29), Some("seal_delegate_call"));
    }

    #[test]
    fn test_resolve_unknown_returns_none() {
        assert_eq!(resolve_host_function(100), None);
        assert_eq!(resolve_host_function(999), None);
        assert_eq!(resolve_host_function(u32::MAX), None);
    }

    #[test]
    fn test_ecalli_display_name_known() {
        assert_eq!(ecalli_display_name(0), "seal_input");
        assert_eq!(ecalli_display_name(7), "seal_call");
    }

    #[test]
    fn test_ecalli_display_name_unknown() {
        assert_eq!(ecalli_display_name(100), "ecalli_100");
        assert_eq!(ecalli_display_name(999), "ecalli_999");
    }
}
