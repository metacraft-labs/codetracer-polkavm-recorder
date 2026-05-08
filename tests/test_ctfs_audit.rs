//! CTFS-format audit tests for the PolkaVM recorder.
//!
//! Added in the 1.55 audit (`AUDIT-CTFS-2026-05.md`).  Verifies the
//! canonical-CTFS pipeline checklist items closed by the audit:
//!
//!   * (a) The `record` subcommand writes the CTFS multi-stream
//!     container and the resulting `.ct` file starts with the canonical
//!     magic bytes.
//!   * (d) Routing FuelVM-style structured side effects through
//!     `register_special_event` does not regress the size or magic of
//!     the `.ct` container; the canonical writer still produces a
//!     materially populated trace when an ecalli host function is
//!     invoked.
//!
//! History note: pre-2026-05-08 this file also contained a
//! `ctfs_format_advertised_in_record_help` test that asserted
//! `record --help` listed `ctfs` as a `--format` value with
//! `[default: ctfs]`.  The 2026-05-08 convention-compliance pass
//! removed the `--format` flag entirely (recorder is CTFS-only); the
//! replacement assertions live in `tests/test_cli.rs`
//! (`test_no_format_flag_in_help`, `test_help_mentions_ct_print`,
//! `test_format_flag_rejected_by_clap`).  See `AUDIT-CTFS-2026-05.md`
//! ("Convention compliance follow-up — 2026-05-08") for the full
//! record.

use std::path::Path;

use polkavm_common::program::{InstructionSetKind, Reg::*, asm};
use polkavm_common::writer::ProgramBlobBuilder;

/// Canonical CTFS container magic bytes.  Mirrors the constant
/// `CTFS_MAGIC` in `codetracer-trace-format-spec/src/container.rs`.
const CTFS_MAGIC: [u8; 5] = [0xC0, 0xDE, 0x72, 0xAC, 0xE2];

/// Build a minimal program that adds two immediates and returns.  This
/// mirrors `create_add_program_blob` in `tests/test_tracer.rs` but stays
/// local so the audit tests do not depend on cross-test helpers.
fn build_add_program_blob() -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            asm::load_imm(A0, 10),
            asm::load_imm(A1, 32),
            asm::add_32(A0, A0, A1),
            asm::ret(),
        ],
        &[],
    );
    builder.into_vec().expect("failed to build program blob")
}

/// Build a program that issues a known ecalli (host function index) so the
/// audit-time `register_special_event` routing for `seal_debug_message`
/// (idx 28) can be exercised without a full ink! environment.  The
/// program loads two arguments into A0/A1, calls ecalli(28), then returns.
fn build_ecalli_program_blob(ecalli_index: u32) -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            // Set up the calling-convention argument registers so the
            // recorder's call-arg staging has non-trivial values.
            asm::load_imm(A0, 0xCAFE),
            asm::load_imm(A1, 16),
            asm::ecalli(ecalli_index),
            asm::load_imm(A0, 0),
            asm::ret(),
        ],
        &[],
    );
    builder
        .into_vec()
        .expect("failed to build ecalli program blob")
}

/// Find the single `.ct` file in `out_dir`, asserting that exactly one
/// exists and that it begins with the canonical CTFS magic bytes.
fn read_ct_container(out_dir: &Path) -> Vec<u8> {
    let entries: Vec<_> = std::fs::read_dir(out_dir)
        .expect("read output directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("ct"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one .ct file in {:?}, got {:?}",
        out_dir,
        entries
    );
    let bytes = std::fs::read(&entries[0]).expect("read .ct file");
    assert!(
        bytes.len() >= CTFS_MAGIC.len(),
        ".ct file too short to contain CTFS magic"
    );
    assert_eq!(
        &bytes[..CTFS_MAGIC.len()],
        &CTFS_MAGIC,
        ".ct file does not start with canonical CTFS magic bytes"
    );
    bytes
}

/// Audit (a)+(g): writing through the canonical CTFS pipeline produces a
/// `.ct` container with the canonical magic header.  Pre-fix the recorder
/// had no way to request CTFS at all (only legacy `Binary` / `Json`).
#[test]
fn ctfs_writer_produces_ct_container() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("ct-out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob_path = tmp.path().join("simple.polkavm");
    std::fs::write(&blob_path, build_add_program_blob()).unwrap();

    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder must produce a CTFS bundle");

    let bytes = read_ct_container(&out_dir);
    // A materially populated trace contains far more than just the magic
    // header (program metadata, type id table, register-name table, step
    // events).  64 bytes is a loose lower bound that catches a regression
    // where the audit fix path silently empties the event stream.
    assert!(
        bytes.len() > 64,
        ".ct container suspiciously small ({} bytes)",
        bytes.len()
    );
}

/// Audit (d): routing host-function side effects (e.g.
/// `seal_debug_message` for ink! debug output) onto the structured
/// event-log channel via `register_special_event` must not empty the
/// trace.  Pre-fix the recorder did not call `register_special_event`
/// at all; this test asserts the post-fix path still produces a
/// canonical-CTFS container with content.
#[test]
fn ecalli_special_event_does_not_empty_trace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("ct-out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // ecalli index 28 == `seal_debug_message`.  Routed through
    // `EventLogKind::Write` in `tracer.rs` (see the post-1.55 ecalli
    // dispatch table).
    let blob_path = tmp.path().join("ecalli.polkavm");
    std::fs::write(&blob_path, build_ecalli_program_blob(28)).unwrap();

    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder must complete on ecalli 28");

    let bytes = read_ct_container(&out_dir);
    assert!(
        bytes.len() > 64,
        ".ct container suspiciously small after ecalli({})",
        28
    );
}
