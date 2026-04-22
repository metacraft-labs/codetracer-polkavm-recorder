//! Integration tests for the PolkaVM tracer.
//!
//! These tests use polkavm-common's ProgramBlobBuilder to construct
//! test programs programmatically, avoiding the need for a RISC-V
//! cross-compilation toolchain.
//!
//! Tests verify actual trace content with specific computed values,
//! not just file existence or non-emptiness.

use std::path::Path;

use codetracer_trace_writer_nim::TraceEventsFileFormat;
use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
use polkavm_common::writer::ProgramBlobBuilder;

/// Helper: create a simple PolkaVM program blob that adds two numbers.
///
/// The program loads two immediates into registers, adds them,
/// and returns the result in A0.
///
/// Computes: A0 = 10 + 32 = 42
fn create_add_program_blob() -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            // Load 10 into A0
            asm::load_imm(A0, 10),
            // Load 32 into A1
            asm::load_imm(A1, 32),
            // A0 = A0 + A1
            asm::add_32(A0, A0, A1),
            // Return (jump to RA)
            asm::ret(),
        ],
        &[],
    );
    builder.into_vec().expect("failed to build program blob")
}

/// Helper: create a slightly more complex program that exercises
/// multiple operations (load, add, multiply, store).
///
/// Computes: result = (10 + 32) * 2 + 10 = 94
fn create_compute_program_blob() -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            // a = 10
            asm::load_imm(A0, 10),
            // b = 32
            asm::load_imm(A1, 32),
            // sum = a + b (S0 = A0 + A1)
            asm::add_32(S0, A0, A1),
            // doubled = sum * 2 (S1 = S0 + S0, since mul_32 needs 3 regs)
            asm::add_32(S1, S0, S0),
            // final = doubled + a (A0 = S1 + A0)
            asm::add_32(A0, S1, A0),
            // Return
            asm::ret(),
        ],
        &[],
    );
    builder.into_vec().expect("failed to build program blob")
}

/// Helper: write a blob to a temp file and run the tracer on it.
fn run_tracer_on_blob(blob_bytes: &[u8], out_dir: &Path) {
    let blob_path = out_dir.join("test_program.polkavm");
    std::fs::write(&blob_path, blob_bytes).expect("failed to write blob");

    codetracer_polkavm_recorder::recorder::record(&blob_path, out_dir, TraceEventsFileFormat::Json)
        .expect("trace_program should succeed");
}

/// Helper: parse the trace events JSON from the output directory.
fn load_trace_events(out_dir: &Path) -> Vec<serde_json::Value> {
    let events_path = out_dir.join("trace.json");
    let content = std::fs::read_to_string(&events_path).expect("failed to read trace events");
    let events: serde_json::Value =
        serde_json::from_str(&content).expect("trace events should be valid JSON");
    events
        .as_array()
        .expect("events should be an array")
        .clone()
}

/// Helper: parse trace_metadata.json from the output directory.
fn load_trace_metadata(out_dir: &Path) -> serde_json::Value {
    let metadata_path = out_dir.join("trace_metadata.json");
    let content =
        std::fs::read_to_string(&metadata_path).expect("failed to read trace_metadata.json");
    serde_json::from_str(&content).expect("trace_metadata.json should be valid JSON")
}

/// Helper: collect all Int values from Value events in the trace.
/// Returns a vec of (variable_id, i64_value) pairs.
fn collect_int_values(events: &[serde_json::Value]) -> Vec<(i64, i64)> {
    events
        .iter()
        .filter_map(|e| {
            let val = e.get("Value")?;
            let variable_id = val.get("variable_id")?.as_i64()?;
            let value = val.get("value")?;
            if value.get("kind").and_then(|k| k.as_str()) == Some("Int") {
                let i = value.get("i").and_then(|v| v.as_i64())?;
                Some((variable_id, i))
            } else {
                None
            }
        })
        .collect()
}

/// Helper: collect all VariableName events and build a map from variable_id to name.
/// Variable IDs are assigned sequentially starting from 0 in the order VariableName events appear.
fn collect_variable_names(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| {
            e.get("VariableName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Helper: find all Int values for a given register name across the trace.
fn find_register_values(events: &[serde_json::Value], register_name: &str) -> Vec<i64> {
    let var_names = collect_variable_names(events);
    let var_id = var_names.iter().position(|name| name == register_name);

    match var_id {
        Some(id) => {
            let int_values = collect_int_values(events);
            int_values
                .iter()
                .filter(|(vid, _)| *vid == id as i64)
                .map(|(_, v)| *v)
                .collect()
        }
        None => vec![],
    }
}

// ---------------------------------------------------------------------------
// Test 1: Basic execution and output file content verification
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_tracer_basic_execution() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    // Verify the three output files exist and are non-empty.
    for filename in &["trace.json", "trace_metadata.json", "trace_paths.json"] {
        let path = out_dir.join(filename);
        assert!(path.exists(), "{} should exist", filename);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 0, "{} should be non-empty", filename);
    }

    // trace.json should be valid JSON containing an array of events.
    let events = load_trace_events(&out_dir);
    assert!(!events.is_empty(), "trace should have at least one event");

    // There should be Step events (actual execution was recorded).
    let step_count = events.iter().filter(|e| e.get("Step").is_some()).count();
    assert!(
        step_count > 0,
        "trace should contain at least one Step event, got none"
    );

    // There should be VariableName events (register names were recorded).
    let var_names = collect_variable_names(&events);
    assert!(
        !var_names.is_empty(),
        "trace should contain VariableName events for registers"
    );

    // There should be Value events (register values were recorded).
    let value_count = events.iter().filter(|e| e.get("Value").is_some()).count();
    assert!(
        value_count > 0,
        "trace should contain Value events for register values"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Compute program final result value (critical E2E test)
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_compute_value_at_return() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_compute_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // The compute program calculates: (10 + 32) * 2 + 10 = 94
    // At the end, arg0 (A0) should contain 94.
    // (A0 is renamed to arg0 within the "main" function scope)
    let arg0_values = find_register_values(&events, "arg0");
    assert!(
        !arg0_values.is_empty(),
        "should have arg0 (A0) values in the trace"
    );

    // Verify that arg0 contains 94 at some point (the final computed result).
    assert!(
        arg0_values.contains(&94),
        "arg0 should contain 94 (the final result of (10+32)*2+10) at some step, got values: {:?}",
        arg0_values
    );

    // Also verify intermediate values appear:
    // After load_imm(A0, 10), arg0 = 10
    assert!(
        arg0_values.contains(&10),
        "arg0 should contain 10 (initial load) at some step, got values: {:?}",
        arg0_values
    );

    // S0 should contain 42 (sum = 10 + 32)
    let s0_values = find_register_values(&events, "S0");
    assert!(
        s0_values.contains(&42),
        "S0 should contain 42 (sum of 10+32) at some step, got values: {:?}",
        s0_values
    );

    // S1 should contain 84 (doubled = 42 * 2)
    let s1_values = find_register_values(&events, "S1");
    assert!(
        s1_values.contains(&84),
        "S1 should contain 84 (doubled = 42*2) at some step, got values: {:?}",
        s1_values
    );
}

// ---------------------------------------------------------------------------
// Test 3: Register values captured for add program
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_register_values_captured() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // The add program: arg0 = 10, arg1 = 32, arg0 = arg0 + arg1 = 42
    // (A0/A1 are renamed to arg0/arg1 within the "main" function scope)
    let arg0_values = find_register_values(&events, "arg0");
    let arg1_values = find_register_values(&events, "arg1");

    // arg0 should have value 10 at some point (after first load_imm).
    assert!(
        arg0_values.contains(&10),
        "arg0 should contain 10 at some step, got: {:?}",
        arg0_values
    );

    // arg1 should have value 32 at some point (after second load_imm).
    assert!(
        arg1_values.contains(&32),
        "arg1 should contain 32 at some step, got: {:?}",
        arg1_values
    );

    // arg0 should have value 42 at some point (after add).
    assert!(
        arg0_values.contains(&42),
        "arg0 should contain 42 (10+32) at some step, got: {:?}",
        arg0_values
    );

    // arg0=42 should appear after arg0=10 in the trace (ordering matters).
    let first_10_pos = arg0_values.iter().position(|&v| v == 10).unwrap();
    let first_42_pos = arg0_values.iter().position(|&v| v == 42).unwrap();
    assert!(
        first_42_pos > first_10_pos,
        "arg0=42 should appear after arg0=10 in the trace (10 at index {}, 42 at index {})",
        first_10_pos,
        first_42_pos
    );
}

// ---------------------------------------------------------------------------
// Test 4: Step count is reasonable
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_step_count_reasonable() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // Count Step events in the trace.
    let step_count = events.iter().filter(|e| e.get("Step").is_some()).count();

    // The add program has 4 instructions (load_imm, load_imm, add_32, ret).
    // Step count should be reasonable: at least 1, at most 50.
    assert!(
        step_count >= 1,
        "should have at least 1 step event, got {}",
        step_count
    );
    assert!(
        step_count <= 50,
        "should have at most 50 step events for a 4-instruction program, got {}",
        step_count
    );

    // Verify step events have valid structure (path_id and line fields).
    for event in events.iter().filter(|e| e.get("Step").is_some()) {
        let step = event.get("Step").unwrap();
        assert!(
            step.get("path_id").is_some(),
            "Step event should have path_id field"
        );
        let line = step["line"]
            .as_i64()
            .expect("Step line should be an integer");
        assert!(line > 0, "Step line should be positive, got {}", line);
    }
}

// ---------------------------------------------------------------------------
// Test 5: Metadata has all required fields
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_metadata_has_required_fields() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let metadata = load_trace_metadata(&out_dir);

    // TraceMetadata must contain "program", "args", and "workdir" fields.
    assert!(
        metadata.get("program").is_some(),
        "metadata should have 'program' field, got: {}",
        metadata
    );
    assert!(
        metadata["program"].is_string(),
        "metadata 'program' should be a string"
    );
    // The program field should reference the test program.
    let program_str = metadata["program"].as_str().unwrap();
    assert!(
        program_str.contains("test_program.polkavm"),
        "metadata 'program' should reference the polkavm blob, got: {}",
        program_str
    );

    assert!(
        metadata.get("args").is_some(),
        "metadata should have 'args' field, got: {}",
        metadata
    );
    assert!(
        metadata["args"].is_array(),
        "metadata 'args' should be an array"
    );

    assert!(
        metadata.get("workdir").is_some(),
        "metadata should have 'workdir' field, got: {}",
        metadata
    );
    assert!(
        metadata["workdir"].is_string(),
        "metadata 'workdir' should be a string"
    );
}

// ---------------------------------------------------------------------------
// Test 6: trace_paths.json content validation
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_tracer_paths_valid() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let paths_content =
        std::fs::read_to_string(out_dir.join("trace_paths.json")).expect("failed to read paths");
    let paths: serde_json::Value =
        serde_json::from_str(&paths_content).expect("trace_paths.json should be valid JSON");
    assert!(paths.is_array(), "trace_paths.json should be a JSON array");
    // Paths should not be empty -- at least the blob path should be registered.
    let paths_arr = paths.as_array().unwrap();
    assert!(
        !paths_arr.is_empty(),
        "trace_paths.json should have at least one path entry"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Register names are properly emitted
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_register_names_emitted() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);
    let var_names = collect_variable_names(&events);

    // The tracer emits resolved variable names when debug info is available.
    // Within a function scope (like "main"), A0-A5 are renamed to arg0-arg5.
    // Non-argument registers keep their raw names: S0-S1, T0-T2, SP, RA.
    let expected_names = [
        "arg0", "arg1", "arg2", "arg3", "arg4", "arg5", "S0", "S1", "T0", "T1", "T2", "SP", "RA",
    ];
    for name in &expected_names {
        assert!(
            var_names.contains(&name.to_string()),
            "variable {} should appear in VariableName events, got names: {:?}",
            name,
            var_names
        );
    }
}

// ---------------------------------------------------------------------------
// Test 8: Function entry/exit events
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_function_entry_exit() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // The tracer emits a Return event when execution finishes.
    let return_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.get("Return").is_some())
        .collect();
    assert!(
        !return_events.is_empty(),
        "trace should contain at least one Return event"
    );

    // The Return event should be the last significant event
    // (possibly followed only by metadata events).
    // Find the index of the last Return event.
    let last_return_idx = events
        .iter()
        .rposition(|e| e.get("Return").is_some())
        .expect("should have a Return event");

    // No Step events should come after the Return.
    let steps_after_return = events[last_return_idx + 1..]
        .iter()
        .filter(|e| e.get("Step").is_some())
        .count();
    assert_eq!(
        steps_after_return, 0,
        "no Step events should appear after the final Return"
    );
}

// ---------------------------------------------------------------------------
// Test 9: Compute program trace has all intermediate register values
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_tracer_compute_program() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_compute_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    // Verify all three output files exist.
    assert!(out_dir.join("trace.json").exists());
    assert!(out_dir.join("trace_metadata.json").exists());
    assert!(out_dir.join("trace_paths.json").exists());

    let events = load_trace_events(&out_dir);
    assert!(!events.is_empty(), "compute program should produce events");

    // Collect all Int values across all Value events.
    let int_values = collect_int_values(&events);
    let all_values: Vec<i64> = int_values.iter().map(|(_, v)| *v).collect();

    // The compute program produces these key values:
    // a = 10, b = 32, sum = 42, doubled = 84, final = 94
    for expected in &[10i64, 32, 42, 84, 94] {
        assert!(
            all_values.contains(expected),
            "trace should contain value {} from compute program, got values: {:?}",
            expected,
            {
                let mut unique: Vec<i64> = all_values.clone();
                unique.sort();
                unique.dedup();
                unique
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Test 10: CLI record with programmatic blob
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_cli_record_with_blob() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("cli-traces");
    let blob_path = tmp_dir.path().join("test.polkavm");

    let blob = create_add_program_blob();
    std::fs::write(&blob_path, &blob).expect("failed to write blob");

    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "--",
            "record",
            blob_path.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("failed to run");

    assert!(
        output.status.success(),
        "record should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify output files exist.
    assert!(out_dir.join("trace.json").exists());
    assert!(out_dir.join("trace_metadata.json").exists());
    assert!(out_dir.join("trace_paths.json").exists());

    // Also verify the CLI-produced trace has actual content (not just empty files).
    let events = load_trace_events(&out_dir);
    assert!(!events.is_empty(), "CLI trace should have events");

    let step_count = events.iter().filter(|e| e.get("Step").is_some()).count();
    assert!(step_count > 0, "CLI trace should contain Step events");

    // Verify register values were captured through CLI too.
    // A0 is renamed to arg0 within the "main" function scope.
    let arg0_values = find_register_values(&events, "arg0");
    assert!(
        arg0_values.contains(&42),
        "CLI trace should capture arg0=42 (10+32), got: {:?}",
        arg0_values
    );
}

// ---------------------------------------------------------------------------
// Test 11: Variable name resolution - arg registers renamed in function scope
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_variable_names_resolved() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_add_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);
    let var_names = collect_variable_names(&events);

    // Within the "main" function, A0-A5 should be renamed to arg0-arg5.
    assert!(
        var_names.contains(&"arg0".to_string()),
        "arg0 should appear (A0 renamed within function scope), got: {:?}",
        var_names
    );
    assert!(
        var_names.contains(&"arg1".to_string()),
        "arg1 should appear (A1 renamed within function scope), got: {:?}",
        var_names
    );

    // Non-argument registers should keep their original names.
    assert!(
        var_names.contains(&"S0".to_string()),
        "S0 should keep its raw name, got: {:?}",
        var_names
    );
    assert!(
        var_names.contains(&"SP".to_string()),
        "SP should keep its raw name, got: {:?}",
        var_names
    );
    assert!(
        var_names.contains(&"RA".to_string()),
        "RA should keep its raw name, got: {:?}",
        var_names
    );

    // The raw register names A0-A5 should NOT appear (they are renamed).
    assert!(
        !var_names.contains(&"A0".to_string()),
        "A0 should not appear (renamed to arg0), got: {:?}",
        var_names
    );
    assert!(
        !var_names.contains(&"A1".to_string()),
        "A1 should not appear (renamed to arg1), got: {:?}",
        var_names
    );
}

// ---------------------------------------------------------------------------
// Test 12: Variable values accessible via resolved names
// ---------------------------------------------------------------------------

#[test]
fn test_polkavm_variable_values_via_resolved_names() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    let blob = create_compute_program_blob();
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // The compute program: arg0 = 10, arg1 = 32, S0 = 42, S1 = 84, arg0 = 94
    let arg0_values = find_register_values(&events, "arg0");
    let arg1_values = find_register_values(&events, "arg1");

    // arg0 should show the progression: 10 -> 94
    assert!(
        arg0_values.contains(&10),
        "arg0 should contain initial value 10, got: {:?}",
        arg0_values
    );
    assert!(
        arg0_values.contains(&94),
        "arg0 should contain final value 94, got: {:?}",
        arg0_values
    );

    // arg1 should contain 32
    assert!(
        arg1_values.contains(&32),
        "arg1 should contain 32, got: {:?}",
        arg1_values
    );

    // S0 and S1 keep their register names (not argument registers)
    let s0_values = find_register_values(&events, "S0");
    let s1_values = find_register_values(&events, "S1");
    assert!(
        s0_values.contains(&42),
        "S0 should contain 42, got: {:?}",
        s0_values
    );
    assert!(
        s1_values.contains(&84),
        "S1 should contain 84, got: {:?}",
        s1_values
    );
}

// ---------------------------------------------------------------------------
// Test 13: Ecalli generates Call and Return events in the trace
// ---------------------------------------------------------------------------

/// Helper: create a program that makes a known ecalli call (seal_input, index 0)
/// then continues and returns.
///
/// The program: load 10 into A0, ecalli 0 (seal_input), load 20 into A0, ret.
fn create_ecalli_program_blob(ecalli_index: u32) -> Vec<u8> {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            asm::load_imm(A0, 10),
            asm::ecalli(ecalli_index),
            asm::load_imm(A0, 20),
            asm::ret(),
        ],
        &[],
    );
    builder
        .into_vec()
        .expect("failed to build ecalli program blob")
}

#[test]
fn test_ecalli_generates_call_and_return_events() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Use ecalli index 0 = seal_input (a known host function).
    let blob = create_ecalli_program_blob(0);
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // There should be a Call event with function name "seal_input".
    let call_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e.get("Call").is_some()).collect();
    assert!(
        !call_events.is_empty(),
        "trace should contain a Call event for the ecalli, got events: {:?}",
        events
            .iter()
            .map(|e| e.as_object().unwrap().keys().next().unwrap().clone())
            .collect::<Vec<_>>()
    );

    // There should be Return events (at least one from the ecalli, one from program end).
    let return_count = events.iter().filter(|e| e.get("Return").is_some()).count();
    assert!(
        return_count >= 2,
        "should have at least 2 Return events (one for ecalli, one for program end), got {}",
        return_count
    );

    // Execution should continue after the ecalli: arg0 should have value 20.
    let arg0_values = find_register_values(&events, "arg0");
    assert!(
        arg0_values.contains(&20),
        "arg0 should contain 20 (loaded after ecalli), got: {:?}",
        arg0_values
    );
}

// ---------------------------------------------------------------------------
// Test 14: Unknown ecalli halts execution
// ---------------------------------------------------------------------------

#[test]
fn test_unknown_ecalli_halts_execution() {
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Use ecalli index 999 (unknown host function).
    let blob = create_ecalli_program_blob(999);
    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // A Call event should still be emitted for the unknown ecalli.
    let call_events: Vec<&serde_json::Value> =
        events.iter().filter(|e| e.get("Call").is_some()).collect();
    assert!(
        !call_events.is_empty(),
        "trace should contain a Call event even for unknown ecalli"
    );

    // Execution should halt after the unknown ecalli, so arg0 should NOT have value 20.
    let arg0_values = find_register_values(&events, "arg0");
    assert!(
        !arg0_values.contains(&20),
        "arg0 should NOT contain 20 (execution halted at unknown ecalli), got: {:?}",
        arg0_values
    );
}

// ---------------------------------------------------------------------------
// Test 15: Multiple ecalli calls generate multiple Call/Return pairs
// ---------------------------------------------------------------------------

#[test]
fn test_multiple_ecalli_calls() {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(
        &[
            asm::load_imm(A0, 1),
            asm::ecalli(0), // seal_input
            asm::load_imm(A0, 2),
            asm::ecalli(2), // seal_caller
            asm::load_imm(A0, 3),
            asm::ecalli(5), // seal_get_storage
            asm::load_imm(A0, 42),
            asm::ret(),
        ],
        &[],
    );
    let blob = builder
        .into_vec()
        .expect("failed to build multi-ecalli blob");

    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp_dir.path().join("traces");
    std::fs::create_dir_all(&out_dir).unwrap();

    run_tracer_on_blob(&blob, &out_dir);

    let events = load_trace_events(&out_dir);

    // Should have at least 3 Call events for the ecalli calls
    // (there may also be a Call for "main" from the trace start).
    let call_count = events.iter().filter(|e| e.get("Call").is_some()).count();
    assert!(
        call_count >= 3,
        "should have at least 3 Call events for 3 ecalli calls, got {}",
        call_count
    );

    // Should have at least 4 Return events (3 from ecalli + 1 from program end).
    let return_count = events.iter().filter(|e| e.get("Return").is_some()).count();
    assert!(
        return_count >= 4,
        "should have at least 4 Return events (3 ecalli + 1 program end), got {}",
        return_count
    );

    // Execution completed successfully: arg0 should contain 42.
    let arg0_values = find_register_values(&events, "arg0");
    assert!(
        arg0_values.contains(&42),
        "arg0 should contain 42 (final value), got: {:?}",
        arg0_values
    );
}

// ---------------------------------------------------------------------------
// Fixture export: build compute program blob and export trace for WDIO tests
// ---------------------------------------------------------------------------

/// Export a trace fixture for the VS Code extension's WDIO smoke tests.
///
/// This test builds the compute program blob programmatically (avoiding the
/// need for a RISC-V cross-compiler), records the trace, and writes the
/// output to the directory specified by `POLKAVM_FIXTURE_OUTPUT_DIR`.
///
/// Run with:
///   POLKAVM_FIXTURE_OUTPUT_DIR=<path> cargo test --test test_tracer -- --ignored export_fixture
#[test]
#[ignore]
fn export_fixture() {
    let _tmpdir;
    let out_dir = match std::env::var("POLKAVM_FIXTURE_OUTPUT_DIR") {
        Ok(dir) => {
            let p = Path::new(&dir).to_path_buf();
            std::fs::create_dir_all(&p).expect("failed to create fixture output directory");
            p
        }
        Err(_) => {
            _tmpdir = tempfile::tempdir().expect("failed to create temp directory");
            _tmpdir.path().to_path_buf()
        }
    };
    let out_dir = out_dir.as_path();

    let blob = create_compute_program_blob();
    run_tracer_on_blob(&blob, out_dir);

    // Verify the fixture was created.
    assert!(
        out_dir.join("trace.json").exists(),
        "trace.json should exist in fixture output"
    );
    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should exist in fixture output"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should exist in fixture output"
    );

    eprintln!("Fixture exported to {}", out_dir.display());
}
