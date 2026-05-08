//! Tracer implementation for PolkaVM.
//!
//! Steps through a PolkaVM program using step tracing and emits
//! CodeTracer trace events (steps, calls, returns, variables).

use std::path::Path;

use codetracer_trace_types::{EventLogKind, Line, NONE_VALUE, TypeKind, ValueRecord};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Context, Result, eyre};
use polkavm::{Config, Engine, InterruptKind, Module, ModuleConfig, ProgramBlob, Reg};

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

        // -- 3. Create engine and module with step tracing -------------------------------
        let engine_config = Config::from_env()
            .map_err(|e| eyre!("failed to parse PolkaVM config from environment: {e}"))?;
        let engine = Engine::new(&engine_config)
            .map_err(|e| eyre!("failed to create PolkaVM engine: {e}"))?;

        let mut module_config = ModuleConfig::new();
        module_config.set_step_tracing(true);

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
            host_handler: Box::new(NoOpHostFunctionHandler),
            is_solidity,
        };

        // -- 6. Initialise output files --------------------------------------------------
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("cannot create output dir: {}", out_dir.display()))?;

        // CTFS-only writer — events stream lives in `trace.bin`.
        let events_path = out_dir.join("trace.bin");
        let metadata_path = out_dir.join("trace_metadata.json");
        let paths_path = out_dir.join("trace_paths.json");

        TraceWriter::begin_writing_trace_events(&mut *tracer.writer, &events_path)
            .map_err(|e| eyre!("{e}"))?;
        TraceWriter::begin_writing_trace_metadata(&mut *tracer.writer, &metadata_path)
            .map_err(|e| eyre!("{e}"))?;
        TraceWriter::begin_writing_trace_paths(&mut *tracer.writer, &paths_path)
            .map_err(|e| eyre!("{e}"))?;

        // -- 7. Start the trace ----------------------------------------------------------
        TraceWriter::start(&mut *tracer.writer, blob_path, Line(1));

        // Register the "u32/u64" type for register values.
        let reg_type_id = TraceWriter::ensure_type_id(&mut *tracer.writer, TypeKind::Int, "u64");
        tracer.reg_type_id = Some(reg_type_id);

        // -- 8. Instantiate and run with step tracing ------------------------------------
        let mut instance = module
            .instantiate()
            .map_err(|e| eyre!("failed to instantiate module: {e}"))?;

        instance.prepare_call_typed(entry_point, ());

        tracer.run_step_loop(&mut instance, &source_mapper, &variable_info, blob_path)?;

        // -- 9. Finish writing -----------------------------------------------------------
        TraceWriter::finish_writing_trace_events(&mut *tracer.writer).map_err(|e| eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_metadata(&mut *tracer.writer)
            .map_err(|e| eyre!("{e}"))?;
        TraceWriter::finish_writing_trace_paths(&mut *tracer.writer).map_err(|e| eyre!("{e}"))?;
        tracer.writer.close().map_err(|e| eyre!("{e}"))?;

        Ok(())
    }

    /// Run the step loop, processing each instruction and emitting trace events.
    fn run_step_loop(
        &mut self,
        instance: &mut polkavm::RawInstance,
        source_mapper: &SourceMapper,
        variable_info: &DwarfVariableInfo,
        blob_path: &Path,
    ) -> Result<()> {
        let reg_type_id = self.reg_type_id.unwrap();
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
                    self.emit_register_values(instance, reg_type_id, pc, variable_info);
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
                    TraceWriter::register_special_event(
                        &mut *self.writer,
                        EventLogKind::Error,
                        "polkavm_trap",
                        &format!("step={step_count} pc={:?}", instance.program_counter()),
                    );
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
                        9 => {
                            // seal_terminate(beneficiary_ptr)
                            TraceWriter::register_special_event(
                                &mut *self.writer,
                                EventLogKind::TraceLogEvent,
                                "ink_terminate",
                                &format!("beneficiary_ptr={:#x}", instance.reg(Reg::A0)),
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
                InterruptKind::Segfault(_segfault) => {
                    eprintln!(
                        "Segfault at step {} (pc: {:?})",
                        step_count,
                        instance.program_counter()
                    );
                    // Route the trap onto the structured error channel so
                    // the frontend surfaces it as a runtime failure rather
                    // than dropping it silently (mirrors Cairo 1.50
                    // CairoPanic and Fuel 1.53 Panic/Revert routing).
                    TraceWriter::register_special_event(
                        &mut *self.writer,
                        EventLogKind::Error,
                        "polkavm_segfault",
                        &format!("step={step_count} pc={:?}", instance.program_counter()),
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
    fn emit_register_values(
        &mut self,
        instance: &polkavm::RawInstance,
        reg_type_id: codetracer_trace_types::TypeId,
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
    }
}
