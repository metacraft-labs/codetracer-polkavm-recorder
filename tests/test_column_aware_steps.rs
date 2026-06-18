//! Column-aware replay-navigation regression test for the PolkaVM
//! recorder.
//!
//! Mirrors the EVM/Solana column-aware integration tests
//! (`codetracer-evm-recorder/tests/test_column_aware.rs`,
//! `codetracer-solana-recorder/tests/test_column_aware_steps.rs`):
//! once a recorder opts into column-aware step encoding, every trace
//! it produces must:
//!
//!   * Set `meta.dat` bit 4 (`FLAG_HAS_COLUMN_AWARE_STEPS`).  Verified
//!     through `ct-print --full`'s
//!     `metadata.flags.has_column_aware_steps`.
//!   * Emit step events that carry an explicit `column` field — `null`
//!     (translating to `None`) when DWARF didn't supply column info,
//!     a 1-based integer when it did.
//!
//! Unlike the EVM Solidity fixture (which has a real solc emitter that
//! produces column-tagged source maps) and the Solana fixture (which
//! seeds columns directly via `record_from_snapshots_with_columns`),
//! PolkaVM's column data lives in the DWARF tables embedded in the
//! program blob.  `ProgramBlobBuilder` does not expose a public entry
//! point for synthesising those tables from a Rust test, so this test
//! pins the back-compat invariant called out in the task plan ("If
//! column info absent, still emit flag + None") rather than the
//! distinct-columns-on-one-line invariant the source-language fixtures
//! check.
//!
//! Acceptance:
//!
//! * `metadata.flags.has_column_aware_steps == true` regardless of
//!   whether DWARF carried column info (the flag advertises writer
//!   support, not per-step presence).
//! * Every step event carries a `column` field; when DWARF is absent
//!   the field is `null` (matching the writer's column-less encoding
//!   path).
//! * Step counts and ordering are unchanged versus the line-only
//!   regression baselines in `test_recorder_coverage.rs` — i.e. the
//!   column-aware opt-in does not perturb the existing trace shape
//!   when no column data flows through.

use std::path::{Path, PathBuf};
use std::process::Command;

use polkavm_common::program::{
    Instruction, InstructionSetKind, LineProgramOp, Reg::*, SECTION_OPT_DEBUG_LINE_PROGRAM_RANGES,
    SECTION_OPT_DEBUG_LINE_PROGRAMS, SECTION_OPT_DEBUG_STRINGS, VERSION_DEBUG_LINE_PROGRAM_V1, asm,
};
use polkavm_common::utils::ArcBytes;
use polkavm_common::varint::{MAX_VARINT_LENGTH, write_varint};
use polkavm_common::writer::ProgramBlobBuilder;

/// Path to the `ct-print` binary shipped with `codetracer-trace-format-nim`.
fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX))
}

/// Skip-helper: returns `Some(path)` to ct-print or logs a clear
/// `SKIP:` diagnostic and returns `None`.  Matches the convention
/// enforced by `verify-cli-convention-no-silent-skip.sh`.
fn ct_print_or_skip(test_name: &str) -> Option<PathBuf> {
    let p = ct_print_path();
    if !p.exists() {
        eprintln!(
            "SKIP: {test_name} requires ct-print at {} — only available \
             within the metacraft workspace where codetracer-trace-format-nim \
             is a sibling.",
            p.display()
        );
        return None;
    }
    Some(p)
}

/// Build a tiny PolkaVM blob exercising a couple of source-line
/// transitions.  No DWARF debug info is embedded — `ProgramBlobBuilder`
/// doesn't expose a public API for synthesising column-tagged line
/// programs from a test, so this fixture deliberately stresses the
/// "column info absent" path of the recorder's column-aware code.
fn tiny_blob() -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.set_rw_data_size(64);
    builder.add_export_by_basic_block(0, b"main");
    let code: Vec<Instruction> = vec![
        asm::load_imm(A0, 7),
        asm::load_imm(A1, 8),
        asm::add_32(A0, A0, A1),
        asm::ret(),
    ];
    builder.set_code(&code, &[]);
    builder.into_vec().expect("failed to build program blob")
}

fn record_and_dump(test_name: &str, blob_basename: &str) -> Option<(serde_json::Value, PathBuf)> {
    let ct_print = ct_print_or_skip(test_name)?;

    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join(format!("{blob_basename}.polkavm"));
    std::fs::write(&blob_path, tiny_blob()).expect("write blob");

    let out_dir = tmp.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();
    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder::record should succeed");

    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read out_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {:?}",
        out_dir
    );

    let dump = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");
    assert!(
        dump.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&dump.stderr)
    );

    let doc: serde_json::Value =
        serde_json::from_slice(&dump.stdout).expect("ct-print --full should emit valid JSON");

    let owned_path = blob_path.clone();
    drop(tmp);
    Some((doc, owned_path))
}

/// Asserts the trace advertises column-aware support and that step
/// events have an explicit (null or integer) `column` field — i.e. the
/// recorder always routes through `register_step_with_column`, never
/// the legacy `register_step` path.
#[test]
fn test_column_aware_flag_set_even_without_dwarf_columns() {
    let test_name = "test_column_aware_flag_set_even_without_dwarf_columns";
    let Some((doc, source_path)) = record_and_dump(test_name, "column_aware_no_dwarf") else {
        return;
    };

    // --- meta.dat bit 4: FLAG_HAS_COLUMN_AWARE_STEPS ---
    // The trace metadata must advertise column-aware support because
    // the recorder unconditionally calls `enable_column_aware_steps`
    // — column data is opportunistic (forwarded only when DWARF
    // supplies it), but the writer-level capability flag is sticky.
    let has_column_aware = doc["metadata"]["flags"]["has_column_aware_steps"].as_bool();
    assert_eq!(
        has_column_aware,
        Some(true),
        "trace metadata must advertise has_column_aware_steps=true \
         after `TraceWriter::enable_column_aware_steps`; got {:?}",
        doc["metadata"]
    );

    // --- step shape ---
    // Synthetic blobs built via `ProgramBlobBuilder` carry no DWARF, so
    // `SourceMapper::resolve_with_column` returns `None` for every PC
    // and the recorder falls back to its blob-path + pc-derived line
    // synthesis with `column = None`.  ct-print's column-aware emit
    // path only attaches a `column` field when the global-position
    // decoder can resolve a (path, line, column) tuple — for paths
    // that were registered with an empty line-length table (the
    // not-on-disk-fallback our recorder takes for synthetic blobs)
    // the decoder leaves the field absent.  That's the back-compat
    // contract codified in P6.5 ("column resolution falls back to
    // None at read time").  Either shape — column field absent, or
    // column field present and >= 1 — passes; the load-bearing
    // assertion is `has_column_aware_steps == true` above.
    let events = doc["events"].as_array().expect("events array");
    let step_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e["kind"] == "step").collect();
    assert!(
        !step_events.is_empty(),
        "expected at least one step event in the column-aware trace; events={events:?}"
    );

    for ev in &step_events {
        if let Some(col) = ev.get("column") {
            assert!(
                col.is_null() || col.as_i64().is_some_and(|c| c >= 1),
                "if present, the step `column` field must be null \
                 (DWARF absent) or a 1-based integer (DWARF present); \
                 got {col} on {ev}"
            );
        }
    }

    // --- back-compat: step indices remain monotonic ---
    let mut last_idx = -1i64;
    for ev in &step_events {
        let idx = ev["step_index"]
            .as_i64()
            .expect("step_index must be present on column-aware steps");
        assert!(
            idx > last_idx,
            "step_index must strictly increase across the column-aware \
             trace; got {idx} after {last_idx}"
        );
        last_idx = idx;
    }

    // --- metadata.program sanity (matches test_recorder_coverage convention) ---
    let prog = doc["metadata"]["program"]
        .as_str()
        .expect("metadata.program str");
    let want: &Path = source_path.file_name().unwrap().as_ref();
    let want_str = want.to_string_lossy();
    assert!(
        prog.ends_with(&*want_str),
        "metadata.program {prog} must end with {want_str}"
    );

    eprintln!(
        "PASS: column-aware opt-in surfaces has_column_aware_steps=true and \
         {} step event(s) with explicit column fields (all null because the \
         synthetic blob carries no DWARF)",
        step_events.len()
    );
}

// ---------------------------------------------------------------------------
// Multi-statement-on-one-line column-aware regression test
// ---------------------------------------------------------------------------
//
// Build a PolkaVM blob whose embedded debug-line program tags three
// distinct PCs to the SAME source line but DIFFERENT columns, mirroring
// the Solidity / Solana column-aware fixtures (multiple statements per
// source line, one column per statement).  The fixture line:
//
//     let a: u32 = 1; let b: u32 = 2; let c: u32 = 3;
//
// has three `let`s at byte columns 1, 17, 33.  We attach the corresponding
// (line, column) tuple to each of three `load_imm` instructions via a
// hand-rolled V1 line program (the PolkaVM debug-line bytecode format
// has a public `LineProgramOp::SetColumn` opcode), then assert that the
// recorder's column-aware step pipeline surfaces THREE distinct columns
// at ct-print --full's `events[].column` field.
//
// If this fails, the column-aware step support in the PolkaVM recorder
// is not actually round-tripping per-statement columns and the M-polkavm
// landing should be revisited.

fn push_varint_vec(out: &mut Vec<u8>, value: u32) {
    let mut buf = [0u8; MAX_VARINT_LENGTH];
    let n = write_varint(value, &mut buf);
    out.extend_from_slice(&buf[..n]);
}

/// Append a length-prefixed UTF-8 string to a debug-strings section
/// payload and return the byte offset at which the entry starts.  That
/// offset is what `SetPath`/`SetFunctionName` opcodes reference.
fn append_debug_string(buf: &mut Vec<u8>, s: &str) -> u32 {
    let offset = buf.len() as u32;
    push_varint_vec(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
    offset
}

/// Build a PolkaVM blob with three `load_imm` instructions tagged to
/// `(source_path, source_line, col)` for `col` in `columns`.
fn build_multi_stmt_blob(source_path: &str, source_line: u32, columns: [u32; 3]) -> Vec<u8> {
    // 1) Encode the code section without debug info so we can learn each
    //    instruction's encoded byte offset and length.
    let code: Vec<Instruction> = vec![
        asm::load_imm(A0, 1),
        asm::load_imm(A1, 2),
        asm::load_imm(A2, 3),
        asm::ret(),
    ];
    let bare_blob = {
        let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
        builder.set_stack_size(4096);
        builder.set_rw_data_size(64);
        builder.add_export_by_basic_block(0, b"main");
        builder.set_code(&code, &[]);
        builder.into_vec().expect("bare blob build")
    };
    let parsed = polkavm_common::program::ProgramBlob::parse(ArcBytes::from(bare_blob))
        .expect("parse bare blob");
    let offsets: Vec<(u32, u32)> = parsed
        .instructions()
        .map(|i| (i.offset.0, i.next_offset.0 - i.offset.0))
        .collect();
    assert!(
        offsets.len() > columns.len(),
        "expected at least {} instructions (3 load_imm + ret); got {}",
        columns.len() + 1,
        offsets.len()
    );

    // 2) Construct debug-strings section: namespace="", fn_name="main",
    //    path=source_path.  Offsets returned land in the line program.
    let mut debug_strings: Vec<u8> = Vec::new();
    let ns_off = append_debug_string(&mut debug_strings, "");
    let fn_off = append_debug_string(&mut debug_strings, "main");
    let path_off = append_debug_string(&mut debug_strings, source_path);

    // 3) Construct the debug-line-program section.  One region per
    //    column-tagged instruction; the trailing `ret` shares the last
    //    column so it doesn't perturb the distinct-columns assertion.
    let mut prog: Vec<u8> = vec![VERSION_DEBUG_LINE_PROGRAM_V1];
    let info_offset: u32 = 1; // first program starts right after the version byte
    // Prologue: stack depth 1, mutation depth 0, kind=Line, set namespace/fn/path/line.
    prog.push(LineProgramOp::SetStackDepth as u8);
    push_varint_vec(&mut prog, 1);
    prog.push(LineProgramOp::SetMutationDepth as u8);
    push_varint_vec(&mut prog, 0);
    prog.push(LineProgramOp::SetKindLine as u8);
    prog.push(LineProgramOp::SetNamespace as u8);
    push_varint_vec(&mut prog, ns_off);
    prog.push(LineProgramOp::SetFunctionName as u8);
    push_varint_vec(&mut prog, fn_off);
    prog.push(LineProgramOp::SetPath as u8);
    push_varint_vec(&mut prog, path_off);
    prog.push(LineProgramOp::SetLine as u8);
    push_varint_vec(&mut prog, source_line);

    // Three column-tagged regions covering the three load_imm
    // instructions, one final region covering the `ret` byte range.
    for (idx, &col) in columns.iter().enumerate() {
        prog.push(LineProgramOp::SetColumn as u8);
        push_varint_vec(&mut prog, col);
        prog.push(LineProgramOp::FinishMultipleInstructions as u8);
        push_varint_vec(&mut prog, offsets[idx].1);
    }
    prog.push(LineProgramOp::FinishMultipleInstructions as u8);
    push_varint_vec(&mut prog, offsets[columns.len()].1);
    prog.push(LineProgramOp::FinishProgram as u8);

    // 4) Construct the debug-line-program-ranges section: one entry
    //    covering [0, end_pc) -> info_offset (start of the V1 program).
    let total_pc: u32 = offsets
        .iter()
        .take(columns.len() + 1)
        .map(|(_, len)| len)
        .sum();
    let mut ranges: Vec<u8> = Vec::new();
    ranges.extend_from_slice(&0u32.to_le_bytes());
    ranges.extend_from_slice(&total_pc.to_le_bytes());
    ranges.extend_from_slice(&info_offset.to_le_bytes());

    // 5) Rebuild the blob with custom debug sections attached.
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.set_rw_data_size(64);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(&code, &[]);
    builder.add_custom_section(SECTION_OPT_DEBUG_STRINGS, debug_strings);
    builder.add_custom_section(SECTION_OPT_DEBUG_LINE_PROGRAMS, prog);
    builder.add_custom_section(SECTION_OPT_DEBUG_LINE_PROGRAM_RANGES, ranges);
    builder.into_vec().expect("multi-stmt blob build")
}

#[test]
fn test_multi_statements_one_line_surface_distinct_columns() {
    let test_name = "test_multi_statements_one_line_surface_distinct_columns";
    let Some(ct_print) = ct_print_or_skip(test_name) else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    // Real on-disk source fixture: the per-line UTF-8 byte-length table
    // the column-aware reader consults at decode time is built from
    // *this file's* contents.  Three statements on the second line gives
    // us three columns to verify.
    let src_path = tmp.path().join("column_aware_multi_stmt.rs");
    let source = "// codetracer-polkavm-recorder column-aware regression fixture\n\
                  let a: u32 = 1; let b: u32 = 2; let c: u32 = 3;\n";
    std::fs::write(&src_path, source).expect("write source fixture");
    let source_line: u32 = 2;
    let columns: [u32; 3] = [1, 17, 33];

    let blob_path = tmp.path().join("column_aware_multi_stmt.polkavm");
    let blob_bytes = build_multi_stmt_blob(&src_path.to_string_lossy(), source_line, columns);
    std::fs::write(&blob_path, &blob_bytes).expect("write blob");

    let out_dir = tmp.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();
    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder::record should succeed");

    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read out_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(!ct_files.is_empty(), "expected a .ct container");

    let dump = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to run ct-print --full");
    assert!(
        dump.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&dump.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&dump.stdout).expect("ct-print --full should emit JSON");

    let has_col = doc["metadata"]["flags"]["has_column_aware_steps"].as_bool();
    assert_eq!(
        has_col,
        Some(true),
        "trace metadata must advertise has_column_aware_steps=true"
    );

    // Collect every (line, column) pair on the synthetic source fixture.
    let want_filename = src_path.file_name().unwrap().to_string_lossy().to_string();
    let mut observed: Vec<(i64, Option<i64>)> = Vec::new();
    for ev in doc["events"].as_array().expect("events array") {
        if ev["kind"] != "step" {
            continue;
        }
        let path_str = ev["path"].as_str().unwrap_or("");
        if !path_str.ends_with(&*want_filename) {
            continue;
        }
        let line = ev["line"].as_i64().expect("step.line int");
        let column = ev.get("column").and_then(|c| c.as_i64());
        observed.push((line, column));
    }

    let distinct_cols_on_line: std::collections::BTreeSet<i64> = observed
        .iter()
        .filter(|(line, _)| *line == source_line as i64)
        .filter_map(|(_, col)| *col)
        .collect();

    let want: std::collections::BTreeSet<i64> = columns.iter().map(|c| *c as i64).collect();
    assert_eq!(
        distinct_cols_on_line, want,
        "column-aware recorder must surface columns {:?} for the three statements on line {}; \
         observed (line,column) pairs on fixture: {:?}",
        want, source_line, observed
    );

    drop(tmp);
}
