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
/// Reads the program blob at `blob_path`, executes it with step tracing,
/// and writes a CodeTracer CTFS trace bundle to `out_dir`.
pub fn record(blob_path: &Path, out_dir: &Path) -> Result<()> {
    let blob_bytes = std::fs::read(blob_path)
        .with_context(|| format!("failed to read blob file: {}", blob_path.display()))?;

    PolkaVmTracer::trace_program(blob_path, &blob_bytes, out_dir)
}
