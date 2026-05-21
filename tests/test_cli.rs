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
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX))
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

/// Record the canonical `flow_test.polkavm` fixture, then convert the
/// produced `.ct` container to JSON via `ct-print` and assert on:
///
/// 1. **Structural anchors** (legacy layer): `ct-print --json` output
///    contains the source filename and at least one PolkaVM register
///    name somewhere in the textual rendering.
/// 2. **Exact decoded values** (the layer enabled by `ct-print --full`):
///    `flow_test.rs` runs `compute()` which evaluates
///    `(10 + 32) * 2 + 10 = 94` via let-bindings `a=10`, `b=32`,
///    `sum_val=42`, `doubled=84`, `final_result=94`.  The PolkaVM
///    recorder snapshots register state on each step (no DWARF-aware
///    let-binding recovery yet — that's a separate follow-up), so the
///    canonical values surface through the register stream:
///    `arg0` carries `a=10` (then `final_result=94` at the return),
///    `arg1` carries `b=32`, `S0` carries `sum_val=42`, `S1` carries
///    `doubled=84`.  Each value must surface in the trace as a step
///    variable with a decoded `Int` ValueRecord whose `i` field matches
///    the canonical literal from the source program.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and a
/// `trace.json` file was written directly.  The convention now mandates
/// CTFS-only output; `ct print` is the canonical conversion tool.  See
/// `Recorder-CLI-Conventions.md` §4.  `ct-print --full` (added 2026-05
/// in `codetracer-trace-format-nim`) is what enables the exact-value
/// layer — its output is a deterministic JSON document with every CBOR
/// `ValueRecord` decoded to a structured form like
/// `{"kind":"Int","i":42,"type_id":N}`.
///
/// The note in earlier revisions of this test about register integer
/// payloads not round-tripping through `ct-print --json` is empirically
/// obsolete for `--full`: the recorder's `register_full_value` path
/// decodes back to `{"kind":"Int","i":<n>,"type_id":N}` with values
/// intact (a=10, b=32, sum_val=42, doubled=84, final_result=94 all
/// verified).  The strict `value.kind == "Int"` invariant means: if a
/// future PolkaVM recorder upgrade emits a different `ValueRecord`
/// variant for register values (e.g. `Raw` for a 32-byte register
/// snapshot), this test fails loudly and the next maintainer extends
/// the assertion to the new variant rather than silently accepting it.
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

    // Use the canonical fixture so the test pins down the exact let-
    // binding values from `flow_test.rs`.  A programmatic
    // `build_add_program_blob` would only exercise A0/A1 and miss the
    // S0/S1 saved-register transitions for `sum_val` and `doubled`.
    let blob_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-programs")
        .join("rust")
        .join("flow_test.polkavm");
    assert!(
        blob_path.exists(),
        "canonical fixture missing at {}",
        blob_path.display()
    );

    codetracer_polkavm_recorder::recorder::record(&blob_path, &out_dir)
        .expect("recorder should succeed on the canonical flow_test.polkavm fixture");

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

    // -----------------------------------------------------------------
    // Layer 1 (legacy): ct-print --json — substring presence checks.
    // Kept as a safety net so a regression in the textual rendering
    // is caught even if --full's JSON shape evolves.
    // -----------------------------------------------------------------
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_json = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout_json.is_empty(),
        "ct-print --json produced empty output"
    );

    // Structural anchor 1: the fixture source path name appears in the
    // path stream rendered by ct-print.
    assert!(
        stdout_json.contains("flow_test.polkavm"),
        "ct-print --json output should mention the fixture source path \
         (flow_test.polkavm); got:\n{stdout_json}"
    );

    // Structural anchor 2: at least one of the PolkaVM register names
    // captured by the tracer appears.  Within `main` the argument
    // registers A0..A5 are renamed to arg0..arg5; the saved /
    // temp / SP / RA registers keep their raw names.  The set used
    // here is broad enough that all of them rotating out of the
    // recorder at once is unlikely.
    let register_anchor = ["arg0", "arg1", "S0", "S1", "T0", "SP", "RA"]
        .iter()
        .any(|v| stdout_json.contains(v));
    assert!(
        register_anchor,
        "ct-print --json output should mention at least one of the \
         PolkaVM register names \
         (arg0/arg1/S0/S1/T0/SP/RA); got:\n{stdout_json}"
    );

    // -----------------------------------------------------------------
    // Layer 2 (the upgrade): ct-print --full — exact decoded values.
    // -----------------------------------------------------------------
    let full_output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print --full");

    assert!(
        full_output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&full_output.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&full_output.stdout)
        .expect("ct-print --full should emit valid JSON");

    // ----- Path table: the canonical fixture path must appear ---------
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("flow_test.polkavm")),
        "expected flow_test.polkavm in paths table; got {:?}",
        paths
    );

    // ----- Function table: only the synthesised entry-point ----------
    // The PolkaVM recorder doesn't yet resolve DWARF function names or
    // synthesise solc-style `fn_at_pc_*` placeholders for in-program
    // subroutine calls in this fixture (flow_test.polkavm uses no
    // `load_imm_and_jump` calls).  It does, however, register the
    // program entry point (`main` for non-Solidity blobs) so the
    // calltrace pane has a root frame.  Pin this down so a future
    // upgrade that adds DWARF function-name resolution trips this
    // assertion and the next maintainer extends the call-sequence
    // checks below to cover the new behaviour rather than silently
    // accepting the change.
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["main"],
        "expected only the synthesised entry-point function for the \
         PolkaVM recorder; got {:?} — if DWARF function-name resolution \
         has landed, extend this test to assert on the resolved names \
         (e.g. `compute`, `main`) via `ends_with` matching",
        functions
    );

    // ----- Step / call counts ----------------------------------------
    // The recorder steps PolkaVM bytecode one host instruction at a
    // time and emits register snapshots for the entry, the `compute()`
    // dispatch, the five let-binding transitions, and the return.
    // 7 step events / 0 call events are stable properties of the
    // canonical fixture under the current PolkaVM recorder — if they
    // change, that's a real regression to investigate, not a flake.
    let counts = &doc["counts"];
    assert_eq!(
        counts["steps"].as_u64(),
        Some(7),
        "expected 7 step events for flow_test.polkavm; counts={counts}",
    );
    assert_eq!(
        counts["calls"].as_u64(),
        Some(1),
        "expected exactly 1 call event (the synthesised entry-point \
         Call(main)); counts={counts}",
    );
    assert_eq!(
        counts["paths"].as_u64(),
        Some(1),
        "expected exactly 1 path (flow_test.polkavm); counts={counts}",
    );

    let events = doc["events"].as_array().expect("events array");

    // ----- Call sequence: only the synthesised entry-point Call(main)
    let call_sequence: Vec<&str> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .filter_map(|e| e["function"].as_str())
        .collect();
    assert_eq!(
        call_sequence,
        vec!["main"],
        "expected only the synthesised entry-point Call(main) for the \
         PolkaVM recorder; got {:?} — if DWARF-driven call-frame \
         synthesis has landed, extend this test to verify the call \
         sequence ends with `compute` (etc.) via `ends_with` matching",
        call_sequence
    );

    // ----- Strict ValueRecord variant + exact decoded values ----------
    // Collect every (varname, i64) pair surfaced by step events.  The
    // PolkaVM recorder writes register values as `ValueRecord::Int`
    // CBOR blobs; ct-print --full decodes them back to
    // `{"kind":"Int","i":<n>,...}`.  Each step also carries one
    // synthetic `args` variable encoded as a `ValueRecord::Sequence`
    // bundling the PolkaVM ABI argument registers (A0..A5) — that
    // variant is strictly asserted in-line and excluded from the Int
    // collection used for the canonical-flow value check below.
    // If any OTHER non-Int variant surfaces (e.g. a future `Raw`
    // 4-byte register snapshot, or `BigInt` for 64-bit register
    // values on Latest64), fail loudly so the test author can decide
    // whether to extend the assertions or accept the new variant.
    let mut observed_vars: Vec<(String, i64)> = Vec::new();
    for ev in events.iter().filter(|e| e["kind"] == "step") {
        let vars = ev["vars"].as_array().cloned().unwrap_or_default();
        for v in vars {
            let name = v["varname"]
                .as_str()
                .expect("step var should have a varname")
                .to_string();
            let value = &v["value"];
            if name == "args" {
                // Strictly assert the synthetic `args` variable shape
                // (Sequence of 6 Int elements, one per A0..A5).
                assert_eq!(
                    value["kind"].as_str(),
                    Some("Sequence"),
                    "synthetic `args` variable must decode as Sequence; got {value}"
                );
                let elements = value["elements"]
                    .as_array()
                    .expect("Sequence.elements array");
                assert_eq!(
                    elements.len(),
                    6,
                    "synthetic `args` Sequence must hold A0..A5 (6 elements); got {elements:?}"
                );
                for (idx, el) in elements.iter().enumerate() {
                    assert_eq!(
                        el["kind"].as_str(),
                        Some("Int"),
                        "args[{idx}] must be an Int element; got {el}"
                    );
                    el["i"]
                        .as_i64()
                        .unwrap_or_else(|| panic!("args[{idx}].i must be i64; got {el}"));
                }
                continue;
            }
            assert_eq!(
                value["kind"].as_str(),
                Some("Int"),
                "register `{}` should decode as Int, got {}; \
                 if a new ValueRecord variant has landed for PolkaVM \
                 register values, extend this test to assert on it \
                 explicitly rather than weakening the check",
                name,
                value
            );
            let i = value["i"]
                .as_i64()
                .unwrap_or_else(|| panic!("Int.i must be i64 for `{name}`; got {value}"));
            observed_vars.push((name, i));
        }
    }

    // The canonical flow from `flow_test.rs::compute()`:
    //   a=10, b=32, sum_val=a+b=42, doubled=sum_val*2=84,
    //   final_result=doubled+a=94.
    // The PolkaVM recorder doesn't recover Rust let-binding names;
    // values surface through the registers the riscv32 codegen
    // happens to use:
    //   arg0 (=A0)   carries `a=10` first, then `final_result=94`
    //                at the return.
    //   arg1 (=A1)   carries `b=32`.
    //   S0           carries `sum_val=42`.
    //   S1           carries `doubled=84`.
    // Each (register, value) pair must surface at least once across
    // the step stream — if the codegen rotates registers on a
    // toolchain bump, regenerate `flow_test.polkavm` from
    // `flow_test.rs` and update this list to match.  Same canonical
    // fixture as cairo/cardano/circom/etc. — if these five values
    // don't surface, that's the bug to chase.
    let expected: &[(&str, i64)] = &[
        ("arg0", 10),
        ("arg1", 32),
        ("S0", 42),
        ("S1", 84),
        ("arg0", 94),
    ];
    for (name, value) in expected {
        assert!(
            observed_vars
                .iter()
                .any(|(n, v)| n == name && v == value),
            "expected step variable `{name}` = {value} in --full output; \
             observed = {observed_vars:?}"
        );
    }
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
