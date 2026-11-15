//! A Rust guest recorded from its ELF steps through its source.
//!
//! `test-programs/rust-guest` builds `test-programs/rust/flow_test.rs` for
//! the PolkaVM target with `core` built from source. The recorder takes the
//! `.elf`, links it into a blob with its debug info kept, and records it.
//! Every step of `compute()` must land on `flow_test.rs`, at its own line, in
//! the order the compiled code runs it: `b` (17) before `a` (16), as the
//! compiler schedules the two stores, then `sum_val` (18), `doubled` (19) and
//! `final_result` (20).
//!
//! The linker is polkavm-linker 0.32 patched in `vendor/polkavm-linker`.
//! Unpatched, it filed every line-table row of a function at the function's
//! first instruction, so no instruction of the blob had a file or a line and
//! the recording stepped through blob offsets.
//!
//! Prerequisite: a Rust toolchain that can build `core` from source
//! (`-Zbuild-std`): `rust-src` in the sysroot, or `__CARGO_TESTS_ONLY_SRC_ROOT`
//! naming a Rust `library/` directory (the dev shell sets it). Without one the
//! test FAILS naming the prerequisite.

use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_trace_types::TraceLowLevelEvent;

fn guest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-programs")
        .join("rust-guest")
}

fn build_guest(target_dir: &Path) -> PathBuf {
    let sysroot = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("run rustc");
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    let has_src = sysroot.join("lib/rustlib/src/rust/library/core").is_dir()
        || std::env::var_os("__CARGO_TESTS_ONLY_SRC_ROOT").is_some();
    assert!(
        has_src,
        "missing prerequisite: building the guest needs Rust's library source (-Zbuild-std): \
         rust-src in {} or __CARGO_TESTS_ONLY_SRC_ROOT; run inside `nix develop`",
        sysroot.display()
    );
    let guest = guest_dir();
    let build = Command::new("cargo")
        .current_dir(&guest)
        .args(["build", "--release", "-Zbuild-std=core,alloc", "--target"])
        .arg(guest.join("riscv32emac-unknown-none-polkavm.json"))
        .env("RUSTC_BOOTSTRAP", "1")
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .expect("run cargo");
    assert!(
        build.status.success(),
        "building the guest failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let elf = target_dir
        .join("riscv32emac-unknown-none-polkavm")
        .join("release")
        .join("flow_test.elf");
    assert!(elf.is_file(), "no guest ELF at {}", elf.display());
    elf
}

#[test]
fn a_rust_guest_recorded_from_its_elf_steps_through_its_source() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let elf = build_guest(&scratch.path().join("target"));

    let out_dir = scratch.path().join("trace");
    let record = Command::new(env!("CARGO_BIN_EXE_codetracer-polkavm-recorder"))
        .arg("record")
        .arg(&elf)
        .arg("--out-dir")
        .arg(&out_dir)
        .output()
        .expect("run the recorder");
    assert!(
        record.status.success(),
        "recorder failed:\n{}",
        String::from_utf8_lossy(&record.stderr)
    );

    let ct = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "ct"))
        .expect("a .ct container");
    let events = codetracer_trace_reader::ctfs_reader::read_trace_from_ctfs(&ct).expect("decode");
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut steps: Vec<(PathBuf, i64)> = Vec::new();
    for event in events {
        match event {
            TraceLowLevelEvent::Path(p) => paths.push(p),
            TraceLowLevelEvent::Step(s) => steps.push((paths[s.path_id.0].clone(), s.line.0)),
            _ => {}
        }
    }

    let source = guest_dir()
        .join("..")
        .join("rust")
        .join("flow_test.rs")
        .canonicalize()
        .unwrap();
    let source_lines: Vec<i64> = steps
        .iter()
        .filter(|(p, _)| p.canonicalize().ok().as_deref() == Some(source.as_path()))
        .map(|(_, line)| *line)
        .collect();
    assert!(
        !source_lines.is_empty(),
        "no step is in {}; step paths: {:?}",
        source.display(),
        {
            let mut p: Vec<&PathBuf> = steps.iter().map(|(p, _)| p).collect();
            p.dedup();
            p
        }
    );
    let compute: Vec<i64> = source_lines
        .iter()
        .copied()
        .filter(|l| (16..=20).contains(l))
        .collect();
    let mut first_visits = compute.clone();
    first_visits.dedup();
    assert_eq!(
        first_visits,
        vec![17, 16, 18, 19, 20],
        "compute()'s statements, in order, each at its own line; flow_test.rs steps: {source_lines:?}"
    );
}
