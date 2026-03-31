//! Source mapping for PolkaVM programs.
//!
//! Reads debug line program information from a ProgramBlob to map
//! program counter values back to source file paths and line numbers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use polkavm::ProgramBlob;
use polkavm_common::program::ProgramCounter;

/// Cached source location entry.
struct SourceLocation {
    path: PathBuf,
    line: u32,
}

/// Maps PolkaVM program counters to source file locations.
///
/// Uses the debug line program embedded in ProgramBlob to resolve
/// source file and line number for each instruction offset.
pub struct SourceMapper {
    /// Cache of resolved locations, keyed by program counter offset.
    locations: HashMap<u32, SourceLocation>,
}

impl SourceMapper {
    /// Build a `SourceMapper` from a ProgramBlob's debug information.
    ///
    /// Iterates through the blob's debug line programs and pre-caches
    /// source locations for all instruction offsets that have debug info.
    pub fn from_blob(blob: &ProgramBlob) -> Self {
        let mut locations = HashMap::new();

        // Walk through all instructions and try to resolve debug info
        // for each program counter.
        for parsed in blob.instructions() {
            let pc = parsed.offset;
            if let Ok(Some(mut line_program)) = blob.get_debug_line_program_at(pc) {
                // Run the line program to find regions covering this PC.
                while let Ok(Some(region_info)) = line_program.run() {
                    let range = region_info.instruction_range();
                    if pc >= range.start && pc < range.end {
                        // Use the innermost (last) frame for source info.
                        if let Some(frame) = region_info.frames().last() {
                            let path = frame
                                .path()
                                .ok()
                                .flatten()
                                .map(PathBuf::from)
                                .unwrap_or_default();
                            let line = frame.line().unwrap_or(0);
                            if line > 0 {
                                locations.insert(pc.0, SourceLocation { path, line });
                            }
                        }
                        break;
                    }
                }
            }
        }

        Self { locations }
    }

    /// Create an empty source mapper (no debug info available).
    pub fn empty() -> Self {
        Self {
            locations: HashMap::new(),
        }
    }

    /// Resolve a program counter to a source file path and line number.
    ///
    /// Returns `None` if no debug information is available for this PC.
    pub fn resolve(&self, pc: ProgramCounter) -> Option<(&Path, u32)> {
        self.locations
            .get(&pc.0)
            .map(|loc| (loc.path.as_path(), loc.line))
    }

    /// Returns the number of cached source locations.
    pub fn location_count(&self) -> usize {
        self.locations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_source_mapper() {
        let mapper = SourceMapper::empty();
        assert_eq!(mapper.location_count(), 0);
        assert!(mapper.resolve(ProgramCounter(0)).is_none());
    }
}
