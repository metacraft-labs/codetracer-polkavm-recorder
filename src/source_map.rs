//! Source mapping for PolkaVM programs.
//!
//! Reads debug line program information from a ProgramBlob to map
//! program counter values back to source file paths and line numbers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use polkavm::ProgramBlob;
use polkavm_common::program::ProgramCounter;

/// The filesystem path of a source path as a blob's debug info spells it.
///
/// polkavm-linker shortens every path under `/home/<user>/` to `~/...` when
/// it writes a blob's debug strings (`simplify_path` in
/// `polkavm-linker/src/program_from_elf.rs`), so the blob does not carry the
/// user name of the machine that linked it. polkavm's own source cache reads
/// such a path back against `$HOME` (`read_source_file` in
/// `polkavm/src/source_cache.rs`); this does the same, so a recording names
/// the file the program was built from rather than a path no tool opens.
/// Without a usable `$HOME`, and for any other path, the path is kept as it
/// is.
pub fn debug_info_path(path: &str) -> PathBuf {
    expand_home(path, std::env::var_os("HOME").as_deref())
}

fn expand_home(path: &str, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let (Some(relative), Some(home)) = (path.strip_prefix("~/"), home)
        && !home.is_empty()
    {
        return Path::new(home).join(relative);
    }
    PathBuf::from(path)
}

/// Cached source location entry.
struct SourceLocation {
    path: PathBuf,
    line: u32,
    /// Column from DWARF (1-based) when the debug-line program supplied
    /// it.  `None` when DWARF carried only file+line (e.g. `-g1` builds
    /// or compilers that don't emit column tables).  Forwarded as-is to
    /// `register_step_with_column`; the writer treats `None` as "no
    /// column info" and emits a column-less step.
    column: Option<u32>,
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
                                .map(debug_info_path)
                                .unwrap_or_default();
                            let line = frame.line().unwrap_or(0);
                            // Column-aware replay: DWARF's
                            // `DW_AT_decl_column` lands here when the
                            // upstream compiler emits it.  PolkaVM's
                            // `FrameInfo::column()` returns `Some(_)`
                            // only when the embedded line program uses
                            // the `Full` source-location variant — for
                            // line-only entries it returns `None` and
                            // we forward that downstream so the writer
                            // emits a column-less step.
                            let column = frame.column();
                            // A frame with no path names no file: the
                            // instruction is treated like one without
                            // debug information, and steps on the blob.
                            if line > 0 && !path.as_os_str().is_empty() {
                                locations.insert(pc.0, SourceLocation { path, line, column });
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

    /// Resolve a program counter to `(path, line, column)`.
    ///
    /// `column` is `Some(_)` when DWARF carried a 1-based column for
    /// this PC and `None` otherwise (line-only debug info, or the
    /// upstream compiler simply didn't emit column tables — see
    /// `polkavm-common::program::SourceLocation::column`).  Used by
    /// the column-aware step path in `tracer.rs`.
    pub fn resolve_with_column(&self, pc: ProgramCounter) -> Option<(&Path, u32, Option<u32>)> {
        self.locations
            .get(&pc.0)
            .map(|loc| (loc.path.as_path(), loc.line, loc.column))
    }

    /// Returns the number of cached source locations.
    pub fn location_count(&self) -> usize {
        self.locations.len()
    }

    /// Iterate over every distinct source path the mapper has cached,
    /// in the order of the lowest program counter that maps to it.
    /// Used by the tracer to register each path's per-line byte-length
    /// table with the writer before any step names it; the order is the
    /// order the paths get their ids in, so it must not depend on hash
    /// iteration.
    pub fn distinct_paths(&self) -> impl Iterator<Item = &Path> {
        let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        let mut paths: Vec<&Path> = Vec::new();
        let mut pcs: Vec<&u32> = self.locations.keys().collect();
        pcs.sort_unstable();
        for pc in pcs {
            let p: &Path = self.locations[pc].path.as_path();
            if seen.insert(p) {
                paths.push(p);
            }
        }
        paths.into_iter()
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

    #[test]
    fn a_home_relative_debug_path_is_read_against_home() {
        let home = std::ffi::OsStr::new("/home/someone");
        assert_eq!(
            expand_home("~/src/guest/lib.rs", Some(home)),
            PathBuf::from("/home/someone/src/guest/lib.rs")
        );
    }

    #[test]
    fn a_debug_path_without_the_home_prefix_is_kept() {
        let home = std::ffi::OsStr::new("/home/someone");
        for path in [
            "/nix/store/x-rust-lib-src/core/src/mem/mod.rs",
            "src/lib.rs",
            "~user/lib.rs",
            "~",
        ] {
            assert_eq!(expand_home(path, Some(home)), PathBuf::from(path));
        }
    }

    #[test]
    fn a_home_relative_debug_path_is_kept_without_a_home() {
        assert_eq!(expand_home("~/lib.rs", None), PathBuf::from("~/lib.rs"));
        assert_eq!(
            expand_home("~/lib.rs", Some(std::ffi::OsStr::new(""))),
            PathBuf::from("~/lib.rs")
        );
    }
}
