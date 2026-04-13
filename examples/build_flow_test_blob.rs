//! Build the PolkaVM flow test blob using ProgramBlobBuilder.
//!
//! This avoids the need for a RISC-V cross-compilation toolchain. The blob
//! implements the same computation as `test-programs/rust/flow_test.rs`:
//!
//!   a            = 10
//!   b            = 32
//!   sum_val      = a + b        = 42
//!   doubled      = sum_val * 2  = 84
//!   final_result = doubled + a  = 94
//!
//! The blob is written to `test-programs/rust/flow_test.polkavm`.
//!
//! Usage:
//!   cargo run --example build_flow_test_blob

use polkavm_common::program::{asm, InstructionSetKind, Reg::*};
use polkavm_common::writer::ProgramBlobBuilder;

fn main() {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.add_export_by_basic_block(0, b"main");

    // Replicate the logic from flow_test.rs:
    //   fn compute() -> u32 {
    //       let a: u32 = 10;
    //       let b: u32 = 32;
    //       let sum_val: u32 = a + b;
    //       let doubled: u32 = sum_val * 2;
    //       let final_result: u32 = doubled + a;
    //       final_result
    //   }
    builder.set_code(
        &[
            // a = 10 (A0)
            asm::load_imm(A0, 10),
            // b = 32 (A1)
            asm::load_imm(A1, 32),
            // sum_val = a + b (S0 = A0 + A1 = 42)
            asm::add_32(S0, A0, A1),
            // doubled = sum_val * 2 (S1 = S0 + S0 = 84)
            asm::add_32(S1, S0, S0),
            // final_result = doubled + a (A0 = S1 + A0 = 94)
            asm::add_32(A0, S1, A0),
            // Return with final_result in A0
            asm::ret(),
        ],
        &[],
    );

    let blob_bytes = builder.into_vec().expect("failed to build program blob");

    // Write next to the .rs source file.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir.join("test-programs/rust/flow_test.polkavm");

    std::fs::write(&output_path, &blob_bytes)
        .expect("failed to write blob");

    println!(
        "Wrote {} bytes to {}",
        blob_bytes.len(),
        output_path.display()
    );
}
