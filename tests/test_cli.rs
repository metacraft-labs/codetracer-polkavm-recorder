//! CLI-surface integration tests for `codetracer-polkavm-recorder`.
//!
//! Tests cover three areas:
//!
//! 1. **Smoke tests** — basic `--help`, `--version`, error paths.
//! 2. **`ct print` content** — record a fixture and pipe the resulting
//!    `.ct` container through `ct-print --json` from
//!    `codetracer-trace-format-nim` to make content-level assertions.
//!    Skips gracefully when `ct-print` is not present (i.e. when this
//!    crate is built outside the metacraft workspace).
//! 3. **CLI env-var contract** — exercise the post-2026-05-08
//!    `CODETRACER_POLKAVM_RECORDER_OUT_DIR` /
//!    `CODETRACER_POLKAVM_RECORDER_DISABLED` env vars and the
//!    no-`--format` invariant from `Recorder-CLI-Conventions.md` §4 / §5.
//!
//! History note: pre-2026-05-08 the recorder shipped a `--format
//! ctfs|binary|json` flag at three subcommand levels (`record`,
//! `trace-ink`, `replay`).  When the convention switched to CTFS-only
//! the `--format` argument was removed at every level and the dedicated
//! `ctfs_format_advertised_in_record_help` test in
//! `tests/test_ctfs_audit.rs` was deleted (it asserted on the OLD
//! `--format` contract).  See `AUDIT-CTFS-2026-05.md` ("Convention
//! compliance follow-up — 2026-05-08") for the full record.

use std::path::PathBuf;
use std::process::Command;

use polkavm_common::program::{InstructionSetKind, Reg::*, asm};
use polkavm_common::writer::ProgramBlobBuilder;

fn cargo_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "--quiet", "--"]);
    cmd
}

/// Path to the `ct-print` binary shipped with `codetracer-trace-format-nim`.
///
/// The PolkaVM recorder is CTFS-only; tests that need to make
/// content-level assertions on a recorded trace pipe the `.ct`
/// container through `ct-print --json` and assert on the resulting
/// JSON.  This is the same workflow that `Recorder-CLI-Conventions.md`
/// §4 prescribes for downstream tools / golden snapshots.
fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join("ct-print")
}

/// Build a small PolkaVM blob that adds two immediates and returns.
/// Mirrors `create_add_program_blob` in `tests/test_tracer.rs` but
/// stays local so the CLI tests do not depend on cross-test helpers.
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

// ===========================================================================
// Smoke tests
// ===========================================================================

#[test]
fn test_help_flag() {
    let output = cargo_bin().arg("--help").output().expect("failed to run");
    assert!(output.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("codetracer-polkavm-recorder"),
        "help output should mention the program name"
    );
}

#[test]
fn test_version_subcommand() {
    let output = cargo_bin().arg("version").output().expect("failed to run");
    assert!(output.status.success(), "version should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0.1.0"),
        "version output should contain the version number"
    );
}

#[test]
fn test_version_flag() {
    let output = cargo_bin()
        .arg("--version")
        .output()
        .expect("failed to run");
    assert!(output.status.success(), "--version should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0.1.0"),
        "version output should contain the version number"
    );
}

#[test]
fn test_record_nonexistent_file() {
    let output = cargo_bin()
        .args(["record", "nonexistent.polkavm"])
        .output()
        .expect("failed to run");
    assert!(
        !output.status.success(),
        "record with nonexistent file should fail"
    );
}

#[test]
fn test_record_invalid_blob() {
    // Create a temp file with non-polkavm data.
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let invalid_file = tmp_dir.path().join("invalid.polkavm");
    std::fs::write(&invalid_file, b"this is not a polkavm blob").expect("failed to write");

    let output = cargo_bin()
        .args(["record", invalid_file.to_str().unwrap()])
        .output()
        .expect("failed to run");

    assert!(
        !output.status.success(),
        "record with invalid blob should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to parse") || stderr.contains("Error"),
        "error message should indicate parse failure, got: {}",
        stderr
    );
}

// ===========================================================================
// CTFS content via `ct-print` — replaces the legacy `--format json` content
// assertions
// ===========================================================================

/// Record a programmatically-built PolkaVM blob, then convert the
/// produced `.ct` container to JSON via `ct-print --json` and assert on
/// the textual representation.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and a
/// `trace.json` file was written directly.  The convention now mandates
/// CTFS-only output; `ct print` is the canonical conversion tool.  See
/// `Recorder-CLI-Conventions.md` §4.
///
/// The PolkaVM recorder's variable payload (register values encoded as
/// `ValueRecord::Int { i, type_id }`) does not round-trip through
/// `ct print --json` today (same pre-existing limitation as cardano /
/// circom / flow / fuel / leo / miden / move), so this test asserts on
/// **structural anchors** — the fixture's source path file name and at
/// least one of the PolkaVM register names — rather than on integer
/// values.
#[test]
fn test_recorded_trace_via_ct_print_json() {
    let ct_print = ct_print_path();
    if !ct_print.exists() {
        eprintln!(
            "SKIP: ct-print not found at {} — only available within the \
             metacraft workspace where codetracer-trace-format-nim is a sibling.",
            ct_print.display()
        );
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob_path = tmp_dir.path().join("simple.polkavm");
    std::fs::write(&blob_path, build_add_program_blob()).unwrap();

    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder should succeed on the simple add-program blob");

    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("failed to read output directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected at least one .ct file in {}",
        out_dir.display()
    );
    let ct_path = &ct_files[0];

    // ct-print --json <file.ct>
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "ct-print --json produced empty output");

    // Structural anchor 1: the fixture source path name appears in the
    // path stream rendered by ct-print.
    assert!(
        stdout.contains("simple.polkavm"),
        "ct-print --json output should mention the fixture source path \
         (simple.polkavm); got:\n{stdout}"
    );

    // Structural anchor 2: at least one of the PolkaVM register names
    // captured by the tracer appears.  Within `main` the argument
    // registers A0..A5 are renamed to arg0..arg5; the saved /
    // temp / SP / RA registers keep their raw names.  The set used
    // here is broad enough that all of them rotating out of the
    // recorder at once is unlikely.
    let register_anchor = ["arg0", "arg1", "S0", "S1", "T0", "SP", "RA"]
        .iter()
        .any(|v| stdout.contains(v));
    assert!(
        register_anchor,
        "ct-print --json output should mention at least one of the \
         PolkaVM register names \
         (arg0/arg1/S0/S1/T0/SP/RA); got:\n{stdout}"
    );
}

// ===========================================================================
// CLI env-var contract
// ===========================================================================

/// `CODETRACER_POLKAVM_RECORDER_OUT_DIR` must be honoured as a fallback
/// for `--out-dir`.  Convention: `Recorder-CLI-Conventions.md` §5.
#[test]
fn test_env_out_dir_used_when_flag_omitted() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let env_out_dir = tmp_dir.path().join("via-env");

    let blob_path = tmp_dir.path().join("simple.polkavm");
    std::fs::write(&blob_path, build_add_program_blob()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .args(["record"])
        .arg(&blob_path)
        .env("CODETRACER_POLKAVM_RECORDER_OUT_DIR", &env_out_dir)
        // Make sure the env-var doesn't bleed in from the developer's shell.
        .env_remove("CODETRACER_POLKAVM_RECORDER_DISABLED")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed when CODETRACER_POLKAVM_RECORDER_OUT_DIR is set; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The env-var-supplied output dir must contain the .ct bundle.
    let ct_files: Vec<_> = std::fs::read_dir(&env_out_dir)
        .unwrap_or_else(|e| {
            panic!(
                "expected env-supplied out-dir {:?} to exist after record: {e}",
                env_out_dir
            )
        })
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected the env-supplied output dir {:?} to receive the .ct trace bundle",
        env_out_dir
    );
}

/// `CODETRACER_POLKAVM_RECORDER_DISABLED=1` must skip recording entirely.
/// The recorder process should still exit 0 (the PolkaVM recorder
/// doesn't run a separate target subprocess — it loads & executes the
/// blob itself — so "disabled" simply means "don't write any trace
/// artefacts").
#[test]
fn test_env_disabled_skips_recording() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp_dir.path().join("should-stay-empty");

    let blob_path = tmp_dir.path().join("simple.polkavm");
    std::fs::write(&blob_path, build_add_program_blob()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .args(["record"])
        .arg(&blob_path)
        .args(["--out-dir"])
        .arg(&out_dir)
        .env("CODETRACER_POLKAVM_RECORDER_DISABLED", "1")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed in disabled mode; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // No trace artefacts of any kind should have been written.
    let no_artefacts = !out_dir.exists()
        || (std::fs::read_dir(&out_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).next().is_none())
            .unwrap_or(true));
    assert!(
        no_artefacts,
        "no trace artefacts should be written when \
         CODETRACER_POLKAVM_RECORDER_DISABLED=1; got files in {:?}",
        out_dir
    );
}

/// `--format` is no longer accepted at any level — clap must reject it.
/// Convention: §4 (CTFS-only).  Pre-2026-05-08 the flag existed at all
/// three subcommand levels (`record`, `trace-ink`, `replay`); we
/// exercise each here so a partial regression is caught.
#[test]
fn test_format_flag_rejected_by_clap() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp_dir.path().join("traces");
    let blob_path = tmp_dir.path().join("simple.polkavm");
    std::fs::write(&blob_path, build_add_program_blob()).unwrap();

    // record --format json
    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .args(["record"])
        .arg(&blob_path)
        .args(["--out-dir"])
        .arg(&out_dir)
        .args(["--format", "json"])
        .output()
        .expect("failed to run recorder");

    assert!(
        !output.status.success(),
        "--format should be rejected by clap on `record`; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format")
            || stderr.contains("unexpected argument")
            || stderr.contains("unrecognized")
            || stderr.contains("found argument"),
        "clap error should mention the unknown --format flag on `record`; \
         got stderr:\n{stderr}"
    );

    // trace-ink --format json (clap should reject the flag before the
    // contract path even has to exist).
    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .args(["trace-ink"])
        .args(["--contract"])
        .arg(&blob_path)
        .args(["--message", "flip"])
        .args(["--format", "json"])
        .output()
        .expect("failed to run recorder");

    assert!(
        !output.status.success(),
        "--format should be rejected by clap on `trace-ink`"
    );

    // replay --format json
    let output = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .args(["replay"])
        .args(["--address", "0xdeadbeef"])
        .args(["--selector", "get"])
        .args(["--format", "json"])
        .output()
        .expect("failed to run recorder");

    assert!(
        !output.status.success(),
        "--format should be rejected by clap on `replay`"
    );
}

/// The CLI binary must not expose a `--format` flag at any level.
/// Convention: `Recorder-CLI-Conventions.md` §4 — recorders are
/// CTFS-only.
#[test]
fn test_no_format_flag_in_help() {
    let bin = env!("CARGO_BIN_EXE_codetracer-polkavm-recorder");

    for subcmd in [None, Some("record"), Some("trace-ink"), Some("replay")] {
        let mut cmd = Command::new(bin);
        if let Some(s) = subcmd {
            cmd.arg(s);
        }
        cmd.arg("--help");

        let output = cmd.output().expect("failed to run --help");
        assert!(
            output.status.success(),
            "--help (subcmd={:?}) should exit 0",
            subcmd
        );

        let help = String::from_utf8_lossy(&output.stdout);
        assert!(
            !help.contains("--format"),
            "--help (subcmd={:?}) must not advertise --format; got:\n{help}",
            subcmd
        );
        assert!(
            !help.contains("CODETRACER_FORMAT"),
            "--help (subcmd={:?}) must not advertise CODETRACER_FORMAT; got:\n{help}",
            subcmd
        );
    }
}

/// `--help` must mention `ct print` so users know where to go for
/// human-readable conversion of the recorded CTFS bundle.
#[test]
fn test_help_mentions_ct_print() {
    let bin = env!("CARGO_BIN_EXE_codetracer-polkavm-recorder");
    let output = Command::new(bin)
        .arg("--help")
        .output()
        .expect("failed to run --help");
    assert!(output.status.success(), "--help should exit 0");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("ct print"),
        "--help must mention `ct print` as the conversion tool; got:\n{help}"
    );
}
