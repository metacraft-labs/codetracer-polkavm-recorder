//! CodeTracer recorder for PolkaVM programs.
//!
//! This crate captures execution traces from the PolkaVM virtual machine and
//! converts them into the CodeTracer trace format for debugging and analysis.

pub mod dwarf_variables;
pub mod host_functions;
pub mod ink_testing;
pub mod recorder;
pub mod replay;
pub mod solidity;
pub mod source_map;
pub mod tracer;
