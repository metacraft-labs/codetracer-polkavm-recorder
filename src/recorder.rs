//! Recording logic for PolkaVM execution traces.
//!
//! This module provides the top-level `record` function that reads a PolkaVM
//! program blob, runs it through the tracer, and writes a CodeTracer CTFS
//! trace bundle.
//!
//! The output format is fixed to CTFS — see
//! `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`.  Use
//! `ct print` (from `codetracer-trace-format-nim`) for human-readable
//! conversion of the produced bundle.

use std::path::Path;

use eyre::{Context, Result};

use crate::tracer::PolkaVmTracer;

/// Record a PolkaVM execution trace.
///
/// Reads the program at `blob_path`, executes it with step tracing, and
/// writes a CodeTracer CTFS trace bundle to `out_dir`. The program is a
/// PolkaVM blob, or the RISC-V ELF a guest crate builds for the PolkaVM
/// target, which is linked here (debug info kept) so the recording steps
/// through the guest's source.
pub fn record(blob_path: &Path, out_dir: &Path) -> Result<()> {
    let bytes = std::fs::read(blob_path)
        .with_context(|| format!("failed to read blob file: {}", blob_path.display()))?;
    let blob_bytes = if bytes.starts_with(b"\x7fELF") {
        link_elf(&bytes).with_context(|| {
            format!("failed to link {} into a PolkaVM blob", blob_path.display())
        })?
    } else {
        bytes
    };

    PolkaVmTracer::trace_program(blob_path, &blob_bytes, out_dir)
}

/// The PolkaVM blob of a guest ELF, linked with its debug info kept: the line
/// table that maps every instruction to its file, line and column.
pub fn link_elf(elf: &[u8]) -> Result<Vec<u8>> {
    let mut config = polkavm_linker::Config::default();
    config.set_strip(false);
    polkavm_linker::program_from_elf(config, polkavm_linker::TargetInstructionSet::Latest, elf)
        .map_err(|e| eyre::eyre!("{e}"))
}
