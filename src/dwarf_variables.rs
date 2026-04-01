//! Variable name resolution for PolkaVM programs.
//!
//! Extracts function information from ProgramBlob debug data (line programs
//! and exports) to provide meaningful variable names instead of raw register
//! names. Uses RISC-V calling convention knowledge to map argument registers
//! (A0-A5) to named parameters within function scopes.
//!
//! When full DWARF debug info is not available (PolkaVM uses its own compact
//! debug format via LineProgram), this module uses:
//! - Export symbols for function names
//! - LineProgram frame info for function boundaries and names
//! - RISC-V calling convention heuristics for parameter naming

use std::collections::HashMap;

use polkavm::ProgramBlob;
use polkavm_common::program::ProgramCounter;

/// Information about a function extracted from debug data.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// The function name (from export symbol or debug line program).
    pub name: String,
    /// The program counter where this function starts.
    pub start_pc: u32,
    /// The program counter where this function ends (exclusive).
    /// `None` if the end is unknown (last function in the blob).
    pub end_pc: Option<u32>,
}

/// Maps program counters to variable name information.
///
/// Provides two levels of variable naming:
/// 1. Function-scoped argument naming (A0 -> "arg0", A1 -> "arg1", etc.)
/// 2. Function context (which function a PC belongs to)
pub struct DwarfVariableInfo {
    /// Functions sorted by start PC.
    functions: Vec<FunctionInfo>,
    /// Map from PC to function name extracted from line program frames.
    /// This provides finer-grained function info than exports alone.
    frame_functions: HashMap<u32, String>,
}

/// A resolved variable: a human-readable name and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedVariable {
    /// The variable name (e.g., "arg0", "arg1", or register name as fallback).
    pub name: String,
    /// The register value.
    pub value: u64,
}

impl DwarfVariableInfo {
    /// Build variable info from a ProgramBlob's debug information.
    ///
    /// Extracts function boundaries from:
    /// 1. Exported symbols (always available)
    /// 2. Line program frame function names (when debug info is present)
    pub fn from_blob(blob: &ProgramBlob) -> Self {
        let mut functions = Vec::new();

        // Collect exported function entry points.
        let mut export_pcs: Vec<(u32, String)> = blob
            .exports()
            .map(|export| {
                let name = String::from_utf8_lossy(export.symbol().as_bytes()).to_string();
                let pc = export.program_counter().0;
                (pc, name)
            })
            .collect();
        export_pcs.sort_by_key(|(pc, _)| *pc);

        // Build function entries from exports.
        for i in 0..export_pcs.len() {
            let (pc, ref name) = export_pcs[i];
            let end_pc = export_pcs.get(i + 1).map(|(next_pc, _)| *next_pc);
            functions.push(FunctionInfo {
                name: name.clone(),
                start_pc: pc,
                end_pc,
            });
        }

        // Also scan line program frames for function names at each PC.
        let mut frame_functions = HashMap::new();
        for parsed in blob.instructions() {
            let pc = parsed.offset;
            if let Ok(Some(mut line_program)) = blob.get_debug_line_program_at(pc) {
                while let Ok(Some(region_info)) = line_program.run() {
                    let range = region_info.instruction_range();
                    if pc >= range.start && pc < range.end {
                        // Use the outermost (first) frame for function name -
                        // this gives us the actual function, not inlined callees.
                        if let Some(frame) = region_info.frames().next() {
                            if let Ok(Some(func_name)) = frame.function_name_without_namespace() {
                                frame_functions.insert(pc.0, func_name.to_string());
                            }
                        }
                        break;
                    }
                }
            }
        }

        Self {
            functions,
            frame_functions,
        }
    }

    /// Create empty variable info (no debug information available).
    pub fn empty() -> Self {
        Self {
            functions: Vec::new(),
            frame_functions: HashMap::new(),
        }
    }

    /// Find the function containing the given program counter.
    pub fn function_at(&self, pc: ProgramCounter) -> Option<&FunctionInfo> {
        // Binary search for the function whose range contains this PC.
        let idx = self.functions.partition_point(|f| f.start_pc <= pc.0);
        if idx == 0 {
            return None;
        }
        let func = &self.functions[idx - 1];
        // Check if PC is within the function's range.
        match func.end_pc {
            Some(end) if pc.0 >= end => None,
            _ => Some(func),
        }
    }

    /// Get the function name at a given PC, preferring line program info
    /// over export symbols.
    pub fn function_name_at(&self, pc: ProgramCounter) -> Option<&str> {
        // Prefer line program frame function names (more detailed).
        if let Some(name) = self.frame_functions.get(&pc.0) {
            return Some(name.as_str());
        }
        // Fall back to export-derived function info.
        self.function_at(pc).map(|f| f.name.as_str())
    }

    /// Resolve argument register names for the given PC.
    ///
    /// Within a function scope, argument registers A0-A5 are renamed to
    /// "arg0" through "arg5" to indicate they hold function parameters.
    /// Other registers keep their original names.
    ///
    /// Returns `None` if no function context is available at this PC,
    /// meaning the caller should fall back to raw register names.
    pub fn resolve_register_name(&self, pc: ProgramCounter, register_name: &str) -> Option<String> {
        // Only rename argument registers within known function scopes.
        let _func = self.function_at(pc).or_else(|| {
            // If we have frame function info for this PC, treat it as
            // being inside a function even without export boundaries.
            self.frame_functions.get(&pc.0).and(self.functions.first())
        })?;

        // Map A0-A5 to arg0-arg5 based on RISC-V calling convention.
        match register_name {
            "A0" => Some("arg0".to_string()),
            "A1" => Some("arg1".to_string()),
            "A2" => Some("arg2".to_string()),
            "A3" => Some("arg3".to_string()),
            "A4" => Some("arg4".to_string()),
            "A5" => Some("arg5".to_string()),
            _ => None, // Non-argument registers keep their names.
        }
    }

    /// Get the display name for a register at a given PC.
    ///
    /// Returns the resolved variable name if available (e.g., "arg0"),
    /// or the original register name as fallback (e.g., "A0").
    pub fn display_name_for_register(&self, pc: ProgramCounter, register_name: &str) -> String {
        self.resolve_register_name(pc, register_name)
            .unwrap_or_else(|| register_name.to_string())
    }

    /// Returns the number of known functions.
    pub fn function_count(&self) -> usize {
        self.functions.len()
    }

    /// Returns the number of PC-to-function-name mappings from line programs.
    pub fn frame_function_count(&self) -> usize {
        self.frame_functions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_variable_info() {
        let info = DwarfVariableInfo::empty();
        assert_eq!(info.function_count(), 0);
        assert_eq!(info.frame_function_count(), 0);
        assert!(info.function_at(ProgramCounter(0)).is_none());
        assert!(info.function_name_at(ProgramCounter(0)).is_none());
    }

    #[test]
    fn test_display_name_fallback() {
        let info = DwarfVariableInfo::empty();
        // With no function context, should return the original register name.
        assert_eq!(
            info.display_name_for_register(ProgramCounter(0), "A0"),
            "A0"
        );
        assert_eq!(
            info.display_name_for_register(ProgramCounter(0), "SP"),
            "SP"
        );
    }

    #[test]
    fn test_function_lookup() {
        let info = DwarfVariableInfo {
            functions: vec![
                FunctionInfo {
                    name: "main".to_string(),
                    start_pc: 0,
                    end_pc: Some(10),
                },
                FunctionInfo {
                    name: "helper".to_string(),
                    start_pc: 10,
                    end_pc: Some(20),
                },
            ],
            frame_functions: HashMap::new(),
        };

        // PC 0 is in "main"
        assert_eq!(info.function_at(ProgramCounter(0)).unwrap().name, "main");
        // PC 5 is in "main"
        assert_eq!(info.function_at(ProgramCounter(5)).unwrap().name, "main");
        // PC 10 is in "helper"
        assert_eq!(info.function_at(ProgramCounter(10)).unwrap().name, "helper");
        // PC 20 is past "helper"
        assert!(info.function_at(ProgramCounter(20)).is_none());
    }

    #[test]
    fn test_argument_register_naming() {
        let info = DwarfVariableInfo {
            functions: vec![FunctionInfo {
                name: "main".to_string(),
                start_pc: 0,
                end_pc: Some(100),
            }],
            frame_functions: HashMap::new(),
        };

        // Within a function, A0-A5 should be renamed to arg0-arg5.
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "A0"),
            "arg0"
        );
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "A1"),
            "arg1"
        );
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "A5"),
            "arg5"
        );

        // Non-argument registers keep their names.
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "S0"),
            "S0"
        );
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "SP"),
            "SP"
        );
        assert_eq!(
            info.display_name_for_register(ProgramCounter(5), "RA"),
            "RA"
        );
    }

    #[test]
    fn test_from_blob_with_exports() {
        use polkavm_common::program::{InstructionSetKind, Reg::*, asm};
        use polkavm_common::writer::ProgramBlobBuilder;

        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.add_export_by_basic_block(0, b"main");
        builder.set_code(&[asm::load_imm(A0, 42), asm::ret()], &[]);
        let blob_bytes = builder.into_vec().expect("failed to build blob");
        let blob = ProgramBlob::parse(blob_bytes.into()).expect("failed to parse blob");

        let info = DwarfVariableInfo::from_blob(&blob);

        // Should have one function from the export.
        assert_eq!(info.function_count(), 1);

        // The function should be named "main".
        // Note: we can't predict the exact PC assigned by the blob builder,
        // so we just check the function exists.
        assert_eq!(info.functions[0].name, "main");
    }
}
