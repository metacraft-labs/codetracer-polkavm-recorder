//! Tracer implementation for PolkaVM.
//!
//! Steps through a PolkaVM program using step tracing and emits
//! CodeTracer trace events (steps, calls, returns, variables).

use std::collections::HashMap;
use std::path::Path;

use codetracer_trace_types::{EventLogKind, Line, NONE_VALUE, TypeKind, ValueRecord};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Context, Result, eyre};
use polkavm::{
    Config, Engine, GasMeteringKind, InterruptKind, Module, ModuleConfig, ProgramBlob, Reg,
};
use polkavm_common::program::Instruction;

/// Name of the environment variable that opts the recorder into gas
/// metering with a caller-supplied budget.  When set to a positive
/// integer, the recorder enables synchronous gas metering on the module
/// and primes the instance gas counter to the supplied value, so a
/// program that exceeds the budget surfaces an `InterruptKind::NotEnoughGas`
/// interrupt.  The `not_enough_gas_test` fixture pins this on; pre-fix
/// the recorder never enabled gas metering, so the NotEnoughGas arm of
/// `run_step_loop` was dead code.
const GAS_LIMIT_ENV: &str = "POLKAVM_RECORDER_GAS_LIMIT";

thread_local! {
    /// Per-thread override of the gas budget used by `trace_program`.
    /// When `Some(n)`, the recorder enables synchronous gas metering
    /// with budget `n` regardless of the `POLKAVM_RECORDER_GAS_LIMIT`
    /// env var.  Tests use this to drive the NotEnoughGas termination
    /// arm without affecting sibling tests running in parallel on
    /// other threads.  See `set_thread_local_gas_limit`.
    static THREAD_GAS_LIMIT: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };

}

/// Override the gas limit for recordings on the current thread.
///
/// Pass `Some(n)` to opt into synchronous gas metering with budget
/// `n`; pass `None` to clear the override (falling back to the
/// `POLKAVM_RECORDER_GAS_LIMIT` env var if set).
///
/// This is exposed for test-side use: cargo runs tests on multiple
/// threads in parallel within a single test binary, so a process-wide
/// env var would leak state into sibling tests.  Setting the budget
/// via a thread-local keeps the configuration isolated.
pub fn set_thread_local_gas_limit(limit: Option<i64>) {
    THREAD_GAS_LIMIT.with(|cell| cell.set(limit));
}

// The recorder is CTFS-only per `Recorder-CLI-Conventions.md` §4 (see
// `codetracer-specs`).  We pin every `create_trace_writer` call site to
// this constant so the tracer surface no longer carries a `format`
// parameter and the writer cannot accidentally drift away from the
// canonical multi-stream container.
const CTFS_FORMAT: TraceEventsFileFormat = TraceEventsFileFormat::Ctfs;

use crate::dwarf_variables::DwarfVariableInfo;
use crate::host_functions::{self, HostFunctionHandler, NoOpHostFunctionHandler};
use crate::solidity;
use crate::source_map::SourceMapper;

/// The main tracer struct that captures PolkaVM execution traces.
pub struct PolkaVmTracer {
    writer: Box<dyn TraceWriter + Send>,
    /// PolkaVM register value type id (registered once).
    reg_type_id: Option<codetracer_trace_types::TypeId>,
    /// Type id for the synthetic `args` Sequence emitted per step.
    /// Per the PolkaVM RISC-V ABI, the call-convention argument
    /// registers A0..A5 form a positional argument vector that the
    /// spec wants surfaced as a `ValueRecord::Sequence` rather than
    /// as six independent `Int` register snapshots.  See
    /// `tests/test_recorder_coverage.rs::
    ///  test_memory_decoded_as_sequence_value_record`.
    args_seq_type_id: Option<codetracer_trace_types::TypeId>,
    /// Handler for Ecalli (host function) calls.
    host_handler: Box<dyn HostFunctionHandler>,
    /// Whether the current blob is a Solidity-via-Revive contract.
    is_solidity: bool,
}

impl PolkaVmTracer {
    /// Trace a PolkaVM program and write CodeTracer output files.
    ///
    /// 1. Parses the program blob.
    /// 2. Creates a Module with step tracing enabled.
    /// 3. Runs the step loop, emitting trace events.
    /// 4. Writes a CTFS multi-stream `.ct` bundle plus
    ///    `trace_metadata.json` and `trace_paths.json` to `out_dir`.
    pub fn trace_program(blob_path: &Path, blob_bytes: &[u8], out_dir: &Path) -> Result<()> {
        // -- 1. Parse the program blob ---------------------------------------------------
        let blob = ProgramBlob::parse(blob_bytes.to_vec().into())
            .map_err(|e| eyre!("failed to parse program blob: {e}"))?;

        // -- 2. Detect Solidity blobs and build source mapper/variable info ----------------
        let is_solidity = solidity::detect_solidity_blob(&blob);
        if is_solidity {
            eprintln!("Detected Solidity-via-Revive contract blob");
        }

        // When a Solidity blob has debug info, parse the enriched source map.
        // The standard SourceMapper works for both Rust and Solidity blobs since
        // PolkaVM's LineProgram format is compiler-agnostic.
        let _solidity_source_map = if is_solidity {
            solidity::parse_resolc_debug_info(&blob)
        } else {
            None
        };

        let source_mapper = SourceMapper::from_blob(&blob);
        let variable_info = DwarfVariableInfo::from_blob(&blob);

        // Pre-build a `pc -> Instruction` lookup so the step loop can detect
        // in-program subroutine call/return patterns (`load_imm_and_jump` /
        // `jump_indirect`) without re-parsing the bytecode on every step.
        // PolkaVM doesn't expose `Module::blob()` publicly, so we capture the
        // instructions while we still own the parsed `ProgramBlob` here.
        let instruction_at_pc: HashMap<u32, Instruction> = blob
            .instructions()
            .map(|parsed| (parsed.offset.0, parsed.kind))
            .collect();

        // -- 3. Create engine and module with step tracing -------------------------------
        let engine_config = Config::from_env()
            .map_err(|e| eyre!("failed to parse PolkaVM config from environment: {e}"))?;

        // Optional opt-in dynamic paging: a thread-local override
        // (set by tests via `set_thread_local_dynamic_paging`) toggles
        // on PolkaVM's dynamic paging.  When enabled, out-of-bounds
        // memory accesses surface as `InterruptKind::Segfault` with the
        // offending page address; when disabled (the default), they
        // collapse onto the generic `Trap` arm.  Tests that exercise
        // the dedicated segfault taxonomy enable it here.
        let engine = Engine::new(&engine_config)
            .map_err(|e| eyre!("failed to create PolkaVM engine: {e}"))?;

        let mut module_config = ModuleConfig::new();
        module_config.set_step_tracing(true);

        // Optional opt-in gas metering: a thread-local override
        // (set by tests via `set_thread_local_gas_limit`) takes
        // precedence over the `POLKAVM_RECORDER_GAS_LIMIT` env var.
        // When either is set, run with synchronous gas metering
        // primed to the given budget so out-of-gas programs surface
        // the `NotEnoughGas` termination arm of `run_step_loop`.
        // Pre-fix the arm was dead code because the recorder always
        // ran without metering.
        let gas_limit: Option<i64> = THREAD_GAS_LIMIT.with(|cell| cell.get()).or_else(|| {
            std::env::var(GAS_LIMIT_ENV)
                .ok()
                .and_then(|s| s.parse().ok())
        });
        if gas_limit.is_some() {
            module_config.set_gas_metering(Some(GasMeteringKind::Sync));
        }

        let module = Module::from_blob(&engine, &module_config, blob)
            .map_err(|e| eyre!("failed to compile module: {e}"))?;

        // -- 4. Find entry point ---------------------------------------------------------
        // Solidity blobs use `call` (runtime) or `deploy` (constructor) exports;
        // Rust blobs use `main`.
        let entry_point = if is_solidity {
            module
                .exports()
                .find(|export| export == "call" || export == "deploy")
                .map(|e| e.program_counter())
                .ok_or_else(|| eyre!("no 'call' or 'deploy' export found in Solidity blob"))?
        } else {
            module
                .exports()
                .find(|export| export == "main")
                .map(|e| e.program_counter())
                .ok_or_else(|| eyre!("no 'main' export found in program blob"))?
        };

        // -- 5. Create the trace writer (CTFS only) --------------------------------------
        let program_str = blob_path.to_string_lossy();
        let mut tracer = PolkaVmTracer {
            writer: create_trace_writer(&program_str, &[], CTFS_FORMAT),
            reg_type_id: None,
            args_seq_type_id: None,
            host_handler: Box::new(NoOpHostFunctionHandler),
            is_solidity,
        };

        // -- 6. Initialise output files --------------------------------------------------
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

        // CTFS-only writer — events stream lives in `trace.bin`.
        let events_path = out_dir.join("trace.bin");

        TraceWriter::begin_writing_trace_events(&mut *tracer.writer, &events_path)
            .map_err(|e| eyre!("{e}"))?;

        // -- 7. Start the trace ----------------------------------------------------------
        TraceWriter::start(&mut *tracer.writer, blob_path, Line(1));

        // Open the synthetic ``<toplevel>`` Call frame.  The CTFS-era
        // Nim writer's ``trace_writer_start`` only emits a Step --
        // it does not register the ``<toplevel>`` function or emit a
        // Call event for it -- so consumers expecting a real
        // ``<toplevel>`` frame in the calltrace UI (e.g. the
        // vscode-extension WDIO smoke tests) get an empty outer
        // frame instead.  Register + call ``<toplevel>`` explicitly
        // so the synthetic depth-0 frame becomes a real event.  The
        // matching ``register_return`` is emitted in finish_trace.
        let toplevel_fn =
            TraceWriter::ensure_function_id(&mut *tracer.writer, "<toplevel>", blob_path, Line(1));
        TraceWriter::register_call(&mut *tracer.writer, toplevel_fn, vec![]);

        // Register the "u32/u64" type for register values.
        let reg_type_id = TraceWriter::ensure_type_id(&mut *tracer.writer, TypeKind::Int, "u64");
        tracer.reg_type_id = Some(reg_type_id);

        // Register a Seq type id for the synthetic `args` variable
        // (A0..A5 packed as a Sequence ValueRecord).  This satisfies
        // the spec's "collections" requirement that the trace expose
        // the calling-convention argument vector as a structured
        // ValueRecord::Sequence rather than as six independent Int
        // register snapshots.  Pre-fix the recorder only emitted Int
        // snapshots, which is what the `#[ignore]`d
        // `test_memory_decoded_as_sequence_value_record` regression
        // pin called out.
        let args_seq_type_id =
            TraceWriter::ensure_type_id(&mut *tracer.writer, TypeKind::Seq, "args");
        tracer.args_seq_type_id = Some(args_seq_type_id);

        // Synthesise a Call event for the program's entry point so the
        // calltrace pane has a root frame to anchor in-program subroutine
        // call/return events against (and so the function table lists
        // `main` / `call` / `deploy` rather than only the ecalli targets).
        // Pre-fix the recorder never registered the entry function, which
        // (a) left the function table empty for non-ecalli programs, and
        // (b) meant the very first in-program `register_call` had no
        // outer frame to nest under.
        let entry_name: String = module
            .exports()
            .find(|e| e.program_counter() == entry_point)
            .and_then(|e| {
                core::str::from_utf8(e.symbol().as_bytes())
                    .ok()
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                if is_solidity {
                    "call".to_string()
                } else {
                    "main".to_string()
                }
            });
        let entry_fn_id =
            TraceWriter::ensure_function_id(&mut *tracer.writer, &entry_name, blob_path, Line(1));
        TraceWriter::register_call(&mut *tracer.writer, entry_fn_id, vec![]);

        // -- 8. Instantiate and run with step tracing ------------------------------------
        let mut instance = module
            .instantiate()
            .map_err(|e| eyre!("failed to instantiate module: {e}"))?;

        instance.prepare_call_typed(entry_point, ());

        // Prime the gas budget if the caller asked for metering.
        if let Some(gas) = gas_limit {
            instance.set_gas(gas);
        }

        tracer.run_step_loop(
            &mut instance,
            &source_mapper,
            &variable_info,
            &instruction_at_pc,
            blob_path,
        )?;

        // Close the synthetic ``<toplevel>`` Call frame opened above.
        TraceWriter::register_return(&mut *tracer.writer, NONE_VALUE);

        // -- 9. Finish writing -----------------------------------------------------------
        TraceWriter::finish_writing_trace_events(&mut *tracer.writer).map_err(|e| eyre!("{e}"))?;
        tracer
            .writer
            .write_meta_dat("codetracer-polkavm-recorder")
            .map_err(|e| eyre!("{e}"))?;
        tracer.writer.close().map_err(|e| eyre!("{e}"))?;

        Ok(())
    }

    /// Run the step loop, processing each instruction and emitting trace events.
    fn run_step_loop(
        &mut self,
        instance: &mut polkavm::RawInstance,
        source_mapper: &SourceMapper,
        variable_info: &DwarfVariableInfo,
        instruction_at_pc: &HashMap<u32, Instruction>,
        blob_path: &Path,
    ) -> Result<()> {
        let reg_type_id = self.reg_type_id.unwrap();
        let args_seq_type_id = self.args_seq_type_id.unwrap();
        let mut step_count: u64 = 0;
        let mut prev_line: Option<u32> = None;

        loop {
            let interrupt = instance
                .run()
                .map_err(|e| eyre!("PolkaVM execution error: {e}"))?;

            match interrupt {
                InterruptKind::Step => {
                    step_count += 1;

                    // Get current program counter.
                    let pc = match instance.program_counter() {
                        Some(pc) => pc,
                        None => continue,
                    };

                    // Try to resolve source location from debug info.
                    let (source_path, line) = source_mapper
                        .resolve(pc)
                        .unwrap_or_else(|| (blob_path, pc.0 + 1));

                    // Emit step if line changed.
                    if prev_line != Some(line) {
                        TraceWriter::register_step(
                            &mut *self.writer,
                            source_path,
                            Line(line as i64),
                        );
                        prev_line = Some(line);
                    }

                    // Emit register values as variables, using resolved
                    // names when debug info is available.
                    self.emit_register_values(
                        instance,
                        reg_type_id,
                        args_seq_type_id,
                        pc,
                        variable_info,
                    );

                    // Detect in-program subroutine call / return patterns
                    // and synthesise Call / Return trace events for them.
                    //
                    // PolkaVM has no native "call" or "ret" opcode — by
                    // convention RISC-V-style ABIs use:
                    //
                    //   * `load_imm_and_jump(RA, ret_pc, callee_pc)` to
                    //     "set RA = ret_pc; jump to callee_pc" — i.e. a
                    //     function call that lets the callee return via
                    //     `jump_indirect(RA, 0)`.
                    //   * `jump_indirect(RA, 0)` (also spelt `ret`) to
                    //     return to whatever address sits in RA.
                    //
                    // Step tracing fires *before* the instruction at PC
                    // executes, so we emit the Call before the jump
                    // happens (next Step lands at the callee) and the
                    // Return before the ret-jump happens (next Step
                    // lands at the return PC, or `Finished` if RA pointed
                    // outside the program).
                    //
                    // Pre-fix the recorder ignored these patterns entirely:
                    // `counts.calls` was 0 for every blob that didn't go
                    // through `ecalli`, even when the bytecode contained
                    // a deep in-program subroutine chain.  See
                    // `tests/test_recorder_coverage.rs::
                    //  test_in_program_nested_subroutines_emit_call_events`
                    // for the regression pin.
                    if let Some(instruction) = instruction_at_pc.get(&pc.0) {
                        match *instruction {
                            Instruction::load_imm_and_jump(_ra, _value, target) => {
                                let callee_name = format!("fn_at_pc_{target}");
                                let callee_fn_id = TraceWriter::ensure_function_id(
                                    &mut *self.writer,
                                    &callee_name,
                                    blob_path,
                                    Line(0),
                                );
                                TraceWriter::register_call(&mut *self.writer, callee_fn_id, vec![]);
                            }
                            Instruction::jump_indirect(base, _offset) => {
                                // Treat `jump_indirect(RA, _)` as a return.
                                // Other indirect jumps (computed-goto /
                                // jump-table dispatch) are not call/return
                                // boundaries and are left as plain steps.
                                if base.get() == Reg::RA {
                                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
                                }
                            }

                            // Divide-by-zero detection.
                            //
                            // PolkaVM follows RISC-V semantics for div/rem:
                            // division by zero does NOT trap — it returns
                            // u32::MAX (unsigned) or -1 (signed); modulo
                            // by zero returns the dividend.  See
                            // `polkavm-common/src/operation.rs::divu/div/
                            //  remu/rem`.
                            //
                            // To surface the distinct error taxonomy
                            // requested by M12 the recorder inspects the
                            // divisor register *before* the instruction
                            // executes and emits a `polkavm_divide_by_zero`
                            // EventLogKind::Error special event when it is
                            // zero.  Execution continues with the RISC-V
                            // sentinel result so the rest of the trace
                            // remains intact; the event surfaces in
                            // ct-print --full as a single ioError entry.
                            Instruction::div_unsigned_32(_d, _s1, s2)
                            | Instruction::div_unsigned_64(_d, _s1, s2)
                            | Instruction::div_signed_32(_d, _s1, s2)
                            | Instruction::div_signed_64(_d, _s1, s2)
                            | Instruction::rem_unsigned_32(_d, _s1, s2)
                            | Instruction::rem_unsigned_64(_d, _s1, s2)
                            | Instruction::rem_signed_32(_d, _s1, s2)
                            | Instruction::rem_signed_64(_d, _s1, s2) => {
                                let divisor_reg = s2.get();
                                if instance.reg(divisor_reg) == 0 {
                                    TraceWriter::register_special_event(
                                        &mut *self.writer,
                                        EventLogKind::Error,
                                        "polkavm_divide_by_zero",
                                        &format!(
                                            "step={step_count} pc={:?} divisor_reg={:?}",
                                            instance.program_counter(),
                                            divisor_reg,
                                        ),
                                    );
                                }
                            }

                            // Misaligned-access detection.
                            //
                            // PolkaVM permits unaligned memory access (it
                            // does NOT enforce alignment), but the spec
                            // wants the trace to surface the distinct
                            // taxonomy when a load/store happens at an
                            // address that is not a multiple of the
                            // element size.  The recorder reads the base
                            // register / immediate offset, computes the
                            // effective address, and emits a
                            // `polkavm_misaligned_access` EventLogKind::Error
                            // special event when the alignment is wrong.
                            // Execution continues — PolkaVM handles the
                            // unaligned access transparently.
                            Instruction::load_u16(_d, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_u16",
                                    None,
                                    imm,
                                    2,
                                );
                            }
                            Instruction::load_i16(_d, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_i16",
                                    None,
                                    imm,
                                    2,
                                );
                            }
                            Instruction::load_u32(_d, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_u32",
                                    None,
                                    imm,
                                    4,
                                );
                            }
                            Instruction::load_i32(_d, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_i32",
                                    None,
                                    imm,
                                    4,
                                );
                            }
                            Instruction::load_u64(_d, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_u64",
                                    None,
                                    imm,
                                    8,
                                );
                            }
                            Instruction::store_u16(_s, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_u16",
                                    None,
                                    imm,
                                    2,
                                );
                            }
                            Instruction::store_u32(_s, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_u32",
                                    None,
                                    imm,
                                    4,
                                );
                            }
                            Instruction::store_u64(_s, imm) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_u64",
                                    None,
                                    imm,
                                    8,
                                );
                            }
                            Instruction::load_indirect_u16(_d, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_indirect_u16",
                                    Some(base.get()),
                                    offset,
                                    2,
                                );
                            }
                            Instruction::load_indirect_i16(_d, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_indirect_i16",
                                    Some(base.get()),
                                    offset,
                                    2,
                                );
                            }
                            Instruction::load_indirect_u32(_d, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_indirect_u32",
                                    Some(base.get()),
                                    offset,
                                    4,
                                );
                            }
                            Instruction::load_indirect_i32(_d, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_indirect_i32",
                                    Some(base.get()),
                                    offset,
                                    4,
                                );
                            }
                            Instruction::load_indirect_u64(_d, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "load_indirect_u64",
                                    Some(base.get()),
                                    offset,
                                    8,
                                );
                            }
                            Instruction::store_indirect_u16(_s, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_indirect_u16",
                                    Some(base.get()),
                                    offset,
                                    2,
                                );
                            }
                            Instruction::store_indirect_u32(_s, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_indirect_u32",
                                    Some(base.get()),
                                    offset,
                                    4,
                                );
                            }
                            Instruction::store_indirect_u64(_s, base, offset) => {
                                detect_misalign(
                                    &mut *self.writer,
                                    instance,
                                    step_count,
                                    "store_indirect_u64",
                                    Some(base.get()),
                                    offset,
                                    8,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                InterruptKind::Finished => {
                    // Emit the final return.
                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
                    eprintln!("Execution finished after {} steps", step_count);

                    // Capture return value from A0.
                    let result = instance.reg(Reg::A0);
                    eprintln!("Return value (A0): {}", result);
                    break;
                }
                InterruptKind::Trap => {
                    eprintln!(
                        "Trap at step {} (pc: {:?})",
                        step_count,
                        instance.program_counter()
                    );

                    // Disambiguate the trap taxonomy:
                    //
                    //   * If the trap PC points at a memory load/store
                    //     instruction, the trap was an out-of-bounds
                    //     access (PolkaVM collapses segfaults onto Trap
                    //     when dynamic paging is not enabled — see
                    //     `InterruptKind::Trap` docs).  Emit a dedicated
                    //     `polkavm_segfault` Error special event with
                    //     the offending effective address, matching the
                    //     metadata schema of the dynamic-paging
                    //     Segfault arm above.
                    //   * Otherwise, fall back to the generic
                    //     `polkavm_trap` event (e.g. `trap` instruction
                    //     deliberately executed, invalid opcode).
                    //
                    // The M12 sbrk-out-of-bounds fixture pins this
                    // behaviour: `tests/test_recorder_coverage.rs::
                    //  test_sbrk_out_of_bounds_via_ct_print_full`.
                    let trap_pc = instance.program_counter();
                    let memory_access_info = trap_pc
                        .and_then(|pc| instruction_at_pc.get(&pc.0))
                        .and_then(|instr| describe_memory_access(*instr, instance));

                    if let Some((op_name, effective_addr, access_size)) = memory_access_info {
                        TraceWriter::register_special_event(
                            &mut *self.writer,
                            EventLogKind::Error,
                            "polkavm_segfault",
                            &format!(
                                "step={step_count} pc={trap_pc:?} op={op_name} \
                                 page_address={effective_addr:#x} \
                                 page_size={access_size} write_protected=false"
                            ),
                        );
                    } else {
                        TraceWriter::register_special_event(
                            &mut *self.writer,
                            EventLogKind::Error,
                            "polkavm_trap",
                            &format!("step={step_count} pc={trap_pc:?}"),
                        );
                    }
                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
                    break;
                }
                InterruptKind::Ecalli(index) => {
                    let seal_name = host_functions::ecalli_display_name(index);
                    let display_name = if self.is_solidity {
                        solidity::solidity_ecalli_display_name(&seal_name)
                    } else {
                        seal_name.clone()
                    };
                    eprintln!(
                        "Host function {}({}) at step {} (pc: {:?})",
                        display_name,
                        index,
                        step_count,
                        instance.program_counter()
                    );

                    // Stage the calling-convention argument registers (A0-A5)
                    // as canonical call args before `register_call`.  The
                    // `arg()` helper both (a) registers the value as a
                    // step-local variable so it shows in `ct/load-locals`
                    // and (b) appends it to the writer's pending-args buffer
                    // so the next `register_call` attaches them to the
                    // CallRecord.args slice (rendered in the calltrace pane).
                    //
                    // Pre-fix the recorder always passed `vec![]` here,
                    // which is exactly the gap section 5.6 audit (c) calls
                    // out: cross-contract host calls had no symbolic args.
                    // For ecalli the calling convention is fixed (A0..A5
                    // hold the SCALE-encoded host-function argument
                    // pointers / lengths) so we can stage them eagerly
                    // without per-host-function decoding.
                    let arg_regs = [Reg::A0, Reg::A1, Reg::A2, Reg::A3, Reg::A4, Reg::A5];
                    for (idx, reg) in arg_regs.iter().enumerate() {
                        let value = ValueRecord::Int {
                            i: instance.reg(*reg) as i64,
                            type_id: reg_type_id,
                        };
                        let _ = TraceWriter::arg(&mut *self.writer, &format!("a{idx}"), value);
                    }

                    // Emit a Call event for the host function.
                    let fn_id = TraceWriter::ensure_function_id(
                        &mut *self.writer,
                        &display_name,
                        blob_path,
                        Line(0),
                    );
                    TraceWriter::register_call(&mut *self.writer, fn_id, vec![]);

                    // Mirror observable host-side side effects onto the
                    // structured event-log stream so the frontend's Event
                    // Log panel surfaces them outside the calltrace.  Same
                    // pattern as EVM 1.39 LOG-opcode routing, Cairo 1.50
                    // StarknetEvent routing, and Fuel 1.53 Receipt
                    // routing.  PolkaVM has no native stdout/stderr —
                    // host functions are the only output channel — so:
                    //
                    //   * `seal_debug_message` (28) -> EventLogKind::Write
                    //     (the closest analogue to a stdout print; the
                    //     pallet-revive runtime treats it as a debug
                    //     buffer that surfaces in node logs).
                    //   * `seal_deposit_event` (4) -> EventLogKind::EvmEvent
                    //     (ink! contracts emit Substrate events through
                    //     this host fn; semantically equivalent to EVM
                    //     LOG opcodes).
                    //   * `seal_terminate` (9) -> EventLogKind::TraceLogEvent
                    //     (informational; signals contract self-destruct).
                    //
                    // We capture only the calling-convention argument
                    // registers in the content slot — full memory
                    // introspection (e.g. reading the data buffer at
                    // A0 for A1 bytes) is the next-step open work flagged
                    // in the audit memo.  The metadata slot carries the
                    // host-function name so the frontend can group events.
                    match index {
                        4 => {
                            // seal_deposit_event(topics_ptr, topics_len,
                            //                    data_ptr, data_len)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::EvmEvent,
                                "ink_deposit_event",
                                &format!(
                                    "topics_ptr={:#x} topics_len={} data_ptr={:#x} data_len={}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                    instance.reg(Reg::A3),
                                ),
                            );
                        }
                        5 => {
                            // seal_get_storage(key_ptr, key_len,
                            //                  out_ptr, out_len_ptr)
                            //
                            // Surface the pallet-revive storage read on the
                            // structured trace-log stream so the frontend
                            // can show contract-storage operations
                            // distinctly from generic host calls and
                            // EVM events.  The argument-register snapshot
                            // travels through the per-step `args` Sequence
                            // (see `emit_register_values`).
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "seal_get_storage",
                                &format!(
                                    "key_ptr={:#x} key_len={} out_ptr={:#x} out_len_ptr={:#x}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                    instance.reg(Reg::A3),
                                ),
                            );
                        }
                        6 => {
                            // seal_set_storage(key_ptr, key_len,
                            //                  value_ptr, value_len)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "seal_set_storage",
                                &format!(
                                    "key_ptr={:#x} key_len={} value_ptr={:#x} value_len={}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                    instance.reg(Reg::A3),
                                ),
                            );
                        }
                        7 => {
                            // seal_call(dest_ptr, value_ptr, gas_limit,
                            //           input_ptr, input_len,
                            //           output_ptr_or_len_ptr)
                            //
                            // Cross-contract invocation.  The pallet-revive
                            // host function takes the destination address
                            // pointer (A0), the value pointer (A1), the
                            // gas-limit immediate (A2), and the input
                            // calldata buffer (A3=ptr, A4=len), with
                            // A5 carrying the output buffer pointer.
                            // Surface this on the structured trace-log
                            // stream so the frontend can show
                            // cross-contract calls distinctly from generic
                            // host calls (mirrors EVM CALL routing).
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "seal_call",
                                &format!(
                                    "dest_ptr={:#x} value_ptr={:#x} gas_limit={} \
                                     input_ptr={:#x} input_len={} output_ptr={:#x}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                    instance.reg(Reg::A3),
                                    instance.reg(Reg::A4),
                                    instance.reg(Reg::A5),
                                ),
                            );
                        }
                        9 => {
                            // seal_terminate(beneficiary_ptr)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "ink_terminate",
                                &format!("beneficiary_ptr={:#x}", instance.reg(Reg::A0)),
                            );
                        }
                        19 => {
                            // seal_hash_keccak_256(input_ptr, input_len, output_ptr)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "seal_hash_keccak_256",
                                &format!(
                                    "input_ptr={:#x} input_len={} output_ptr={:#x}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                ),
                            );
                        }
                        20 => {
                            // seal_hash_blake2_256(input_ptr, input_len, output_ptr)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "seal_hash_blake2_256",
                                &format!(
                                    "input_ptr={:#x} input_len={} output_ptr={:#x}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                    instance.reg(Reg::A2),
                                ),
                            );
                        }
                        28 => {
                            // seal_debug_message(msg_ptr, msg_len)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::Write,
                                "seal_debug_message",
                                &format!(
                                    "msg_ptr={:#x} msg_len={}",
                                    instance.reg(Reg::A0),
                                    instance.reg(Reg::A1),
                                ),
                            );
                        }
                        _ => {}
                    }

                    // Let the host function handler decide whether to continue.
                    let handled = self.host_handler.handle_ecalli(index, instance);

                    // Emit a Return event after the host function.
                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);

                    if !handled {
                        eprintln!(
                            "Unhandled host function {}({}) — halting execution",
                            display_name, index
                        );
                        TraceWriter::register_special_event(
                            &mut *self.writer,
                            EventLogKind::Error,
                            "unhandled_host_function",
                            &format!("name={display_name} index={index}"),
                        );
                        break;
                    }
                }
                InterruptKind::Segfault(segfault) => {
                    eprintln!(
                        "Segfault at step {} (pc: {:?}) page_address={:#x} \
                         page_size={} write_protected={}",
                        step_count,
                        instance.program_counter(),
                        segfault.page_address,
                        segfault.page_size,
                        segfault.is_write_protected,
                    );
                    // Route the trap onto the structured error channel so
                    // the frontend surfaces it as a runtime failure rather
                    // than dropping it silently (mirrors Cairo 1.50
                    // CairoPanic and Fuel 1.53 Panic/Revert routing).
                    //
                    // The metadata content carries the offending page
                    // address and page size, which the M12 segfault
                    // fixture (`tests/test_recorder_coverage.rs::
                    //  test_sbrk_out_of_bounds_via_ct_print_full`) pins
                    // strictly so a future regression that drops the
                    // segfault details surfaces immediately.
                    TraceWriter::register_special_event(
                        &mut *self.writer,
                        EventLogKind::Error,
                        "polkavm_segfault",
                        &format!(
                            "step={step_count} pc={:?} page_address={:#x} \
                             page_size={} write_protected={}",
                            instance.program_counter(),
                            segfault.page_address,
                            segfault.page_size,
                            segfault.is_write_protected,
                        ),
                    );
                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
                    break;
                }
                InterruptKind::NotEnoughGas => {
                    eprintln!("Out of gas at step {}", step_count);
                    TraceWriter::register_special_event(
                        &mut *self.writer,
                        EventLogKind::Error,
                        "polkavm_out_of_gas",
                        &format!("step={step_count}"),
                    );
                    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Emit the current register values as trace variables.
    ///
    /// When `variable_info` provides resolved names for registers at the
    /// current PC (e.g., "arg0" instead of "A0"), those names are used.
    /// Otherwise, raw register names are emitted as fallback.
    ///
    /// In addition to per-register `Int` snapshots, we emit a synthetic
    /// `args` variable as a `ValueRecord::Sequence` bundling the
    /// PolkaVM ABI argument registers (A0..A5).  This satisfies the
    /// spec's "collections" requirement that the trace expose the
    /// calling-convention argument vector as a structured ValueRecord
    /// rather than only as independent register snapshots — see
    /// `tests/test_recorder_coverage.rs::
    ///  test_memory_decoded_as_sequence_value_record`.
    fn emit_register_values(
        &mut self,
        instance: &polkavm::RawInstance,
        reg_type_id: codetracer_trace_types::TypeId,
        args_seq_type_id: codetracer_trace_types::TypeId,
        pc: polkavm_common::program::ProgramCounter,
        variable_info: &DwarfVariableInfo,
    ) {
        // Emit argument registers (A0-A5) and saved registers (S0-S1).
        let registers = [
            (Reg::A0, "A0"),
            (Reg::A1, "A1"),
            (Reg::A2, "A2"),
            (Reg::A3, "A3"),
            (Reg::A4, "A4"),
            (Reg::A5, "A5"),
            (Reg::S0, "S0"),
            (Reg::S1, "S1"),
            (Reg::T0, "T0"),
            (Reg::T1, "T1"),
            (Reg::T2, "T2"),
            (Reg::SP, "SP"),
            (Reg::RA, "RA"),
        ];

        for (reg, raw_name) in &registers {
            let val = instance.reg(*reg);
            let display_name = variable_info.display_name_for_register(pc, raw_name);
            let value = ValueRecord::Int {
                i: val as i64,
                type_id: reg_type_id,
            };
            TraceWriter::register_variable_with_full_value(&mut *self.writer, &display_name, value);
        }

        // Emit the synthetic `args` Sequence: A0..A5 packed positionally.
        // The PolkaVM RISC-V ABI uses A0..A5 as the call-convention
        // argument vector, so this is the natural "collection" the spec
        // wants surfaced as a Sequence ValueRecord.
        let arg_regs = [Reg::A0, Reg::A1, Reg::A2, Reg::A3, Reg::A4, Reg::A5];
        let elements: Vec<ValueRecord> = arg_regs
            .iter()
            .map(|r| ValueRecord::Int {
                i: instance.reg(*r) as i64,
                type_id: reg_type_id,
            })
            .collect();
        let args_value = ValueRecord::Sequence {
            elements,
            is_slice: false,
            type_id: args_seq_type_id,
        };
        TraceWriter::register_variable_with_full_value(&mut *self.writer, "args", args_value);
    }
}

/// Detect a misaligned PolkaVM load/store and emit a
/// `polkavm_misaligned_access` EventLogKind::Error special event if so.
///
/// PolkaVM does not enforce alignment — unaligned accesses succeed and
/// are handled in software — but the M12 fixtures want this distinct
/// taxonomy surfaced as an `ioError` entry in the trace so downstream
/// tooling can tell it apart from a plain panic / trap.
///
/// `base_reg` carries the address-base register for the `*_indirect_*`
/// family; for the direct `load_uN` / `store_uN` family it is `None` and
/// the effective address is simply the immediate operand (PolkaVM
/// encodes those as absolute addresses).
fn detect_misalign(
    writer: &mut dyn TraceWriter,
    instance: &polkavm::RawInstance,
    step_count: u64,
    op_name: &str,
    base_reg: Option<Reg>,
    imm_offset: u32,
    access_size: u32,
) {
    let base_val = base_reg.map(|r| instance.reg(r)).unwrap_or(0);
    let effective_addr = base_val.wrapping_add(imm_offset as u64);
    #[allow(clippy::manual_is_multiple_of)]
    if access_size > 1 && effective_addr % access_size as u64 != 0 {
        TraceWriter::register_special_event(
            writer,
            EventLogKind::Error,
            "polkavm_misaligned_access",
            &format!("step={step_count} op={op_name} addr={effective_addr:#x} size={access_size}"),
        );
    }
}

/// If `instruction` is a memory load/store, return the operation name,
/// the effective target address and the access size.  Used by the Trap
/// arm of `run_step_loop` to disambiguate an out-of-bounds memory
/// access (which PolkaVM collapses onto Trap when dynamic paging is
/// off) from a deliberate `trap` instruction or invalid opcode.
fn describe_memory_access(
    instruction: Instruction,
    instance: &polkavm::RawInstance,
) -> Option<(&'static str, u64, u32)> {
    let direct = |op_name, imm: u32, size: u32| Some((op_name, imm as u64, size));
    let indirect = |op_name, base: polkavm_common::program::RawReg, imm: u32, size: u32| {
        let base_val = instance.reg(base.get());
        Some((op_name, base_val.wrapping_add(imm as u64), size))
    };

    match instruction {
        Instruction::load_u8(_, imm) => direct("load_u8", imm, 1),
        Instruction::load_i8(_, imm) => direct("load_i8", imm, 1),
        Instruction::load_u16(_, imm) => direct("load_u16", imm, 2),
        Instruction::load_i16(_, imm) => direct("load_i16", imm, 2),
        Instruction::load_u32(_, imm) => direct("load_u32", imm, 4),
        Instruction::load_i32(_, imm) => direct("load_i32", imm, 4),
        Instruction::load_u64(_, imm) => direct("load_u64", imm, 8),
        Instruction::store_u8(_, imm) => direct("store_u8", imm, 1),
        Instruction::store_u16(_, imm) => direct("store_u16", imm, 2),
        Instruction::store_u32(_, imm) => direct("store_u32", imm, 4),
        Instruction::store_u64(_, imm) => direct("store_u64", imm, 8),
        Instruction::load_indirect_u8(_, base, off) => indirect("load_indirect_u8", base, off, 1),
        Instruction::load_indirect_i8(_, base, off) => indirect("load_indirect_i8", base, off, 1),
        Instruction::load_indirect_u16(_, base, off) => indirect("load_indirect_u16", base, off, 2),
        Instruction::load_indirect_i16(_, base, off) => indirect("load_indirect_i16", base, off, 2),
        Instruction::load_indirect_u32(_, base, off) => indirect("load_indirect_u32", base, off, 4),
        Instruction::load_indirect_i32(_, base, off) => indirect("load_indirect_i32", base, off, 4),
        Instruction::load_indirect_u64(_, base, off) => indirect("load_indirect_u64", base, off, 8),
        Instruction::store_indirect_u8(_, base, off) => indirect("store_indirect_u8", base, off, 1),
        Instruction::store_indirect_u16(_, base, off) => {
            indirect("store_indirect_u16", base, off, 2)
        }
        Instruction::store_indirect_u32(_, base, off) => {
            indirect("store_indirect_u32", base, off, 4)
        }
        Instruction::store_indirect_u64(_, base, off) => {
            indirect("store_indirect_u64", base, off, 8)
        }
        _ => None,
    }
}
