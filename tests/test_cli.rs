use std::process::Command;

fn cargo_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "--quiet", "--"]);
    cmd
}

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
    let output = cargo_bin()
        .arg("version")
        .output()
        .expect("failed to run");
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
        .args([
            "record",
            invalid_file.to_str().unwrap(),
        ])
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
