# PolkaVM Recorder CTFS Audit — 2026-05-02

This audit checks `codetracer-polkavm-recorder` against the canonical CodeTracer
multi-stream CTFS schema and the section 5.6 audit checklist maintained in
`/tmp/isonim-migration.txt`. Prior audits set the canonical patterns: Ruby
(1.21, 1.22), Python (1.27), JavaScript (1.38), EVM (1.39), PHP (1.41),
Solana (1.44), Move (1.46), Cardano (1.48), Cairo (1.50), Flow / Cadence
(1.52) and Fuel / Sway (1.53). This is the **twelfth** recorder audited.

## Architecture

The PolkaVM recorder is a **single-process Rust crate** that embeds
`polkavm` 0.32 directly:

* `tracer.rs` (`PolkaVmTracer::trace_program`) parses a `.polkavm` blob
  via `ProgramBlob::parse`, builds a step-traced `Module` (with
  `set_step_tracing(true)`), instantiates it, and drives the step
  loop. Each `InterruptKind::Step` callback resolves source location
  via the per-blob DWARF/LineProgram `SourceMapper` and emits canonical
  CodeTracer events through the Rust-native `NimTraceWriter` (the
  `codetracer_trace_writer_nim` sibling-path crate).
* The general-purpose registers `A0`-`A5`, `S0`-`S1`, `T0`-`T2`,
  `SP`, and `RA` are emitted as `register_variable_with_full_value`
  variables every step, with names resolved through the
  `DwarfVariableInfo` table (so e.g. `A0` becomes `arg0` inside a
  function's argument scope).
* `host_functions.rs` enumerates the pallet-revive Ecalli host
  function table (29 entries: `seal_input`, `seal_return`,
  `seal_caller`, `seal_deposit_event`, `seal_debug_message`, ...).
* `ink_testing.rs` provides a simulated host for ink! contract tests
  (storage, input, return) that lets the recorder run ink! contracts
  end-to-end without a real Substrate runtime.
* `replay.rs` is a placeholder for on-chain (Substrate-RPC) replay.
  It currently fetches a contract code blob via a stubbed RPC client
  and routes through `recorder::record`; the real RPC integration is
  the M8 milestone and is not yet wired in.
* `solidity.rs` detects Solidity-via-Revive contract blobs and adapts
  the entry-point export name (`call` / `deploy`) and host-function
  display names to the `CALLDATALOAD` / `EXTCODECOPY` style EVM
  programmers expect.

The recorder is **not** an FFI consumer — every canonical entry point
(`register_call`, `register_step`, `register_special_event`, `arg`,
`register_thread_*`) is reachable. There are no `#[no_mangle]` stubs,
and `add_event` does not appear in the source.

## Summary

| # | Check | Status (pre-fix) | Status (post-fix) | Notes |
|---|---|---|---|---|
| a | CLI defaults to `TraceEventsFileFormat::Ctfs` | **GAP** | **OK** | Pre-fix the three subcommands `record`, `trace-ink`, `replay` each exposed an `OutputFormat { Binary, Json }` enum with `Binary` (legacy CBOR+Zstd) as the default. The canonical CTFS multi-stream container — required by the Nim `ct_reader_*` FFI and the db-backend's `CTFSTraceReader` — was not selectable at all. Post-fix the shared `OutputFormat` enum gains a `Ctfs` variant (listed first), with doc-comments on each option, plus an `impl From<OutputFormat> for TraceEventsFileFormat` so the three dispatch sites reduce to `let format: TraceEventsFileFormat = args.format.into();`. The `default_value` is now `"ctfs"` for all three subcommands. Same default-format fix as EVM (1.39), Solana (1.44), Move (1.46), Cardano (1.48), Cairo (1.50), Flow (1.52), Fuel (1.53). |
| b | `register_call` for each call | **PARTIAL** | **PARTIAL** | The recorder emits `register_call(fn_id, args)` for every Ecalli host-function invocation (the only call boundary visible at the PolkaVM level — see `tracer.rs::Ecalli` branch). User-program intra-`.polkavm` function calls (Rust `fn` boundaries inside the blob) are NOT emitted because PolkaVM-level step tracing exposes only the linear instruction stream and the standard `polkavm` 0.32 API does not surface RISC-V `JAL` / `JALR` / function-prologue boundaries to the embedder. Closing this requires either (1) DWARF subprogram-range tracking across `register_step`, or (2) a per-instruction call-site detector driven by the polkavm-linker's symbol table. Documented in **Open gaps** below. |
| c | Call args via `register_call_arg` / `arg()` | **GAP** | **OK (Ecalli)** / **GAP (intra-program, source-level)** | Pre-fix the recorder always passed `vec![]` to `register_call`. Post-fix the Ecalli branch stages `A0..A5` (the PolkaVM/RISC-V calling-convention argument registers) as canonical call args via `TraceWriter::arg(&format!("a{idx}"), value)` immediately before `register_call`. This both attaches them to the `CallRecord.args` slice (rendered in the calltrace pane's `.call-arg` rows — see codetracer 1.17) and registers them as step-local variables (so they appear in `ct/load-locals` for the host-function frame). Symbolic argument decoding (e.g. reading the SCALE-encoded ink! message selector + args at the pointer in `A0`) is the next-step open work for the M7/M8 milestones. Intra-program call args remain blocked by audit (b). |
| d | Write/WriteOther/Error/EvmEvent for IO and structured events via `register_special_event` | **GAP** | **OK** (debug/event/terminate/trap/segfault/oom routed; PolkaVM has no native stdout/stderr) | Pre-fix every host-function side effect was silently dropped, and the three runtime traps (`Trap`, `Segfault`, `NotEnoughGas`) emitted only `register_return(NONE_VALUE)` so the frontend's error channel never surfaced them. Post-fix the Ecalli dispatch table routes the structured host-side observable effects: `seal_debug_message` (idx 28) → `EventLogKind::Write` (the closest analogue to a stdout print; the pallet-revive runtime's debug buffer surfaces in node logs); `seal_deposit_event` (idx 4) → `EventLogKind::EvmEvent` (ink! Substrate event emission, semantically equivalent to EVM `LOG0`-`LOG4`); `seal_terminate` (idx 9) → `EventLogKind::TraceLogEvent` (informational; signals contract self-destruct). Trap / Segfault / NotEnoughGas all route through `EventLogKind::Error` with metadata strings `polkavm_trap` / `polkavm_segfault` / `polkavm_out_of_gas`. Unhandled host functions also surface as `Error` with metadata `unhandled_host_function`. PolkaVM has no native stdout/stderr — host functions are the only observable output channel — so explicit `Write` records for stdout are N/A outside `seal_debug_message`. Same routing pattern as EVM 1.39 LOG-opcode → EvmEvent and Fuel 1.53 Receipt → Receipt-kind-specific `EventLogKind`. |
| e | Thread events (Start / Exit / Switch) | OK (N/A) | OK (N/A) | PolkaVM is single-threaded by design — every `Module::instantiate` produces a single `RawInstance` running on a single host thread. Recorder correctly emits no thread events. |
| f | Step records for line navigation | OK | OK | `tracer.rs::run_step_loop` calls `register_step(path, line)` on every `InterruptKind::Step` whenever the source-mapped line changes for the current PC (line de-duped via `prev_line`). Source resolution falls back to `(blob_path, pc.0 + 1)` when DWARF info is absent. |
| g | Canonical CTFS schema match | **GAP** | **OK** | Verified post-fix via `tests/test_ctfs_audit.rs::ctfs_writer_produces_ct_container`: invoking `record(blob, out_dir, TraceEventsFileFormat::Ctfs)` produces a single `.ct` file starting with the canonical magic bytes `0xC0 0xDE 0x72 0xAC 0xE2` and materially populated (>64 bytes for the simple add-program blob, well past just the magic header). |
| h | Obsolete `add_event` calls | OK | OK | `grep -r 'add_event' src/` returns nothing. Recorder predates the 1.30 footgun and has always used dedicated `register_*` entry points. |
| i | `#[no_mangle]` stubs colliding with upstream Nim exports | OK | OK | `grep -r '#\[no_mangle\]' src/` returns nothing. Recorder uses the `codetracer_trace_writer_nim` Rust API directly (sibling-path dep), not the C FFI. |

## Concrete fixes applied

### 1. CLI now exposes and defaults to `Ctfs`

`src/main.rs`'s `OutputFormat` enum used to expose only `Binary` and
`Json`, with `Binary` as the default. There was no way to request the
canonical CTFS multi-stream container — `Binary` writes the legacy
CBOR+Zstd format that the Nim `ct_reader_*` FFI and the db-backend's
`CTFSTraceReader` cannot consume directly.

Post-fix: `OutputFormat` gains a `Ctfs` variant (listed first), with
doc-comments explaining each option, and a freshly added
`impl From<OutputFormat> for TraceEventsFileFormat` makes the three
dispatch sites uniform:

```rust
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Canonical CodeTracer multi-stream container (recommended).
    Ctfs,
    /// Legacy CBOR + Zstd binary format.
    Binary,
    /// Human-readable JSON (slower; useful for debugging).
    Json,
}

impl From<OutputFormat> for TraceEventsFileFormat {
    fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Ctfs => TraceEventsFileFormat::Ctfs,
            OutputFormat::Binary => TraceEventsFileFormat::Binary,
            OutputFormat::Json => TraceEventsFileFormat::Json,
        }
    }
}
```

`RecordArgs.format`, `TraceInkArgs.format`, and `ReplayArgs.format`
all now `default_value = "ctfs"`, and the three dispatch sites in
`record`, `trace_ink`, and `replay` collapse to
`let format: TraceEventsFileFormat = args.format.into();`. An
`OutputFormat::as_str` helper is wired in for future
`trace_metadata.json` `format` field emission (analogous to the Fuel
1.53 audit pattern); marked `#[allow(dead_code)]` until the metadata
emission path is wired.

### 2. Ecalli branch now stages canonical call args

The Ecalli dispatch in `tracer.rs::run_step_loop` previously called
`register_call(fn_id, vec![])` so the calltrace pane showed every
host-function invocation with empty arguments. Post-fix it stages
`A0..A5` (the PolkaVM/RISC-V calling-convention argument registers)
through `TraceWriter::arg("a0", ...)`, ..., `TraceWriter::arg("a5",
...)` immediately before `register_call`. This:

1. Attaches them to the `CallRecord.args` slice that the IsoNim
   calltrace view renders as `.call-arg` rows (post-1.17).
2. Registers them as step-local variables so they show up in the
   `ct/load-locals` response for the host-function frame.

The values are Int-typed against the same `u64` `reg_type_id` that
the per-step register dump uses, so the renderer displays them
consistently. Symbolic decoding (e.g. reading SCALE-encoded ink!
message data from the pointer in `A0`) is open work — see
**Open gaps** below.

### 3. Host-function side effects route through `register_special_event`

The Ecalli dispatch was previously a `register_call` / handler /
`register_return` triple with no surfacing of the host function's
own side effects. Post-fix the dispatch table routes the three
observable host-function families onto the structured event log:

* `seal_debug_message` (idx 28) → `EventLogKind::Write`,
  metadata `"seal_debug_message"`, content
  `"msg_ptr=0x… msg_len=…"`. Pallet-revive's debug buffer is the
  closest analogue PolkaVM has to a stdout print.
* `seal_deposit_event` (idx 4) → `EventLogKind::EvmEvent`,
  metadata `"ink_deposit_event"`, content
  `"topics_ptr=0x… topics_len=… data_ptr=0x… data_len=…"`. ink!
  contracts emit Substrate events through this host function;
  the routing matches EVM 1.39 LOG-opcode routing and Cairo 1.50
  StarknetEvent routing.
* `seal_terminate` (idx 9) → `EventLogKind::TraceLogEvent`,
  metadata `"ink_terminate"`. Informational; signals contract
  self-destruct (semantically distinct from a runtime error).

The metadata slot carries the host-function name so the frontend
can group events; the content slot carries the calling-convention
register values (pointers + lengths). Full memory introspection
(reading the actual bytes at `msg_ptr` for `msg_len` bytes, or the
topic / data buffers at `seal_deposit_event`'s pointer arguments)
is the next-step open work — see **Open gaps**.

### 4. Runtime traps route through the error channel

Pre-fix `InterruptKind::Trap`, `Segfault(_)`, and `NotEnoughGas`
each emitted only `register_return(NONE_VALUE)` and broke out of
the step loop, so the frontend's error channel never surfaced
them. Post-fix each variant additionally calls
`register_special_event(EventLogKind::Error, "polkavm_<kind>",
"step=… pc=…")` before the return. Mirrors the Cairo 1.50
CairoPanic and Fuel 1.53 Panic / Revert routing:

```rust
InterruptKind::Trap => {
    TraceWriter::register_special_event(
        &mut *self.writer,
        EventLogKind::Error,
        "polkavm_trap",
        &format!("step={step_count} pc={:?}", instance.program_counter()),
    );
    TraceWriter::register_return(&mut *self.writer, NONE_VALUE);
    break;
}
```

Unhandled host functions (idx without an entry in
`host_functions::resolve_host_function`) also surface as
`EventLogKind::Error` with metadata `"unhandled_host_function"`.

## Tests added

`tests/test_ctfs_audit.rs` (3 new cases):

* `ctfs_writer_produces_ct_container` — runs the simple add-program
  blob (assembled programmatically via `polkavm_common::writer`)
  through `recorder::record` with `TraceEventsFileFormat::Ctfs` and
  asserts the resulting `.ct` file starts with the canonical CTFS
  magic bytes (`0xC0 0xDE 0x72 0xAC 0xE2`) and is materially
  populated (>64 bytes).
* `ctfs_format_advertised_in_record_help` — CLI smoke test that
  `record --help` advertises `ctfs` as a `--format` value with
  `[default: ctfs]`. Uses `CARGO_BIN_EXE_codetracer-polkavm-recorder`
  to locate the just-built binary (same idiom as Flow 1.52, Fuel
  1.53). Catches accidental defaults regressions.
* `ecalli_special_event_does_not_empty_trace` — structural smoke
  test for the Ecalli `register_special_event` routing introduced
  in this audit. Builds a program with `ecalli(28)`
  (`seal_debug_message`) and asserts the resulting `.ct` container
  starts with the CTFS magic and is materially populated. Pre-fix
  this routing did not exist; post-fix it must not regress the
  size or magic of the canonical container.

Read-side end-to-end content assertions on the embedded event records
(e.g. that `register_special_event(EventLogKind::Write,
"seal_debug_message", …)` actually appears in the event-log of the
`.ct` container) need the `codetracer_trace_reader_nim` dev-dep added
and a small reader-walk helper. Tracked as an open follow-up below
(also open for Cairo, Cardano, Flow, and Fuel).

## Verification

```
cd /home/zahary/metacraft/codetracer-polkavm-recorder
AH_TEST_RESOURCE_GUARD=1 cargo test --release
```

* `lib` unit tests: 47 / 47 passing
* `test_cli` (existing): 5 / 5 passing
* `test_tracer` (existing): 15 / 15 passing (1 ignored —
  `export_fixture` requires `POLKAVM_FIXTURE_OUTPUT_DIR`)
* `test_ctfs_audit` (new): 3 / 3 passing

Total: 70 / 70 active passing in audit-touched suites, 0 regressions.
`cargo build --release` clean.
`cargo clippy --release --lib` produces 0 warnings on audit-touched
code.

### Targeted Playwright sweep

A program-specific spec exists at
`src/tests/gui/tests/program_specific_tests/polkavm_example.spec.ts`.
Per its file-level docstring, every live test is gated on both the
`codetracer-polkavm-recorder` binary AND the `polkatool` PolkaVM
toolchain being available on PATH. The recorder is built by this
audit, but `polkatool` (which compiles `.rs` source down to a
`.polkavm` blob) is not present in the sandbox dev shell, so all
runtime tests skip cleanly. The structural language-detection tests
at the bottom of the spec run unconditionally and were unaffected
by this audit.

```
cd /home/zahary/metacraft/codetracer
just test-gui tests/program_specific_tests/polkavm_example.spec.ts
```

Pre-fix: structural tests pass; live tests skipped (no polkatool).
Post-fix: identical — environment gating is unchanged.

## Open gaps (not blocking, documented for follow-up)

### Intra-program function-call boundaries (audit b / c, recorder-side)

PolkaVM-level step tracing surfaces only the linear instruction
stream — `polkavm` 0.32's `RawInstance::run` callback delivers
`InterruptKind::Step` for each retired instruction but does NOT
expose RISC-V `JAL` / `JALR` / function-prologue boundaries to the
embedder. The current recorder emits `register_call` only at Ecalli
boundaries, so a Rust program with N user-function calls shows up
in the calltrace pane as a flat single-frame dump from the entry
point's perspective.

Closing this needs either of:

1. **DWARF subprogram-range tracking across `register_step`.** The
   blob's DWARF `.debug_info` already carries
   `DW_TAG_subprogram` entries with `DW_AT_low_pc` /
   `DW_AT_high_pc`. Walking that table once at startup and tracking
   PC-vs-current-subprogram transitions in `run_step_loop` would
   let the recorder emit `register_call(fn_id, ...)` /
   `register_return` at each user-function boundary. This is the
   most general approach and works for any DWARF-emitting toolchain
   (Rust, Solidity-via-Revive, future C/C++ blobs).

2. **polkavm-linker symbol-table-driven call detector.** The
   `polkavm-linker` crate already carries a symbol table mapping
   PC ranges to function names. Same mechanism, less DWARF
   plumbing — but only works for blobs produced by polkavm-linker,
   not for hand-assembled or third-party blobs.

Until either lands the recorder still produces correct
instruction-level traces (every step records source line +
register file) — the gap is purely the absence of intra-program
function-call boundaries in the calltrace pane. Mirrors the
Sway-source-level open item from Fuel 1.53.

### Symbolic ink! call-arg decoding (audit c, source-level)

The Ecalli call-arg staging emits the raw register values
`A0..A5` as `Int` arguments. For most pallet-revive host
functions the calling convention is "pointer + length", so the
true symbolic args (e.g. the SCALE-encoded `topics` and `data`
buffers passed to `seal_deposit_event`, or the UTF-8 string passed
to `seal_debug_message`) live in guest memory. Closing this needs
per-host-function memory readers in `tracer.rs::Ecalli`, plus an
ink!-metadata-driven decoder for the Substrate event payloads —
analogous to the EVM 1.39 "call args from JumpType-driven jump
analysis" item and the Fuel 1.53 cross-contract symbolic call-args
item.

Concrete fix shape:

```rust
// Inside the Ecalli branch, after `register_call` and before the
// handler runs, for known host functions read the buffer:
4 => {
    let topics_ptr = instance.reg(Reg::A0) as u32;
    let topics_len = instance.reg(Reg::A1) as u32;
    let data_ptr   = instance.reg(Reg::A2) as u32;
    let data_len   = instance.reg(Reg::A3) as u32;
    let topics = instance.read_memory(topics_ptr, topics_len)
                         .unwrap_or_default();
    let data   = instance.read_memory(data_ptr,   data_len)
                         .unwrap_or_default();
    // SCALE-decode topics + data through the contract's ink!
    // metadata (ink_metadata::Layout), if registered.
    TraceWriter::register_special_event(
        &mut *self.writer,
        EventLogKind::EvmEvent,
        "ink_deposit_event",
        &format!("topics=0x{} data=0x{}", hex(&topics), hex(&data)),
    );
}
```

The `instance.read_memory` API already exists (used by
`InkHostHandler` in `ink_testing.rs`), so this is purely a
recorder-side enhancement.

### Replay-side tracing (audit f, cross-cutting)

`replay.rs::replay_contract_call` is currently a placeholder. Its
`fetch_contract_code` stub always returns an error (only the
local-file fallback works), and the actual Substrate-RPC
integration (M8 milestone) is not yet wired. Once the RPC client
is implemented, the replay path should feed the fetched blob
through `recorder::record` exactly the way the test path does
today — and the fix shape is already in place via this audit
(default `ctfs` format, structured event routing, call-arg
staging). Same shape of gap as Cairo 1.50 and Fuel 1.53.

### Per-contract ABI registration (audit b / c enabling)

ink! contracts ship a `metadata.json` with full ABI info
(`messages[].args[].name`, `messages[].args[].type`,
`events[].args[].type`, ...). The recorder already parses this
JSON in `ink_testing.rs::parse_ink_metadata` for the `trace-ink`
subcommand, but the parsed `InkMessage.args` names are NOT plumbed
into the Ecalli `arg()` staging path. Wiring an
`InkContractInfo`-aware path through `tracer.rs` would let
`seal_input` (idx 0) stage args by symbolic name (e.g.
`init_value` instead of `a0`) once the SCALE-decoded calldata is
available. Parallel to the Fuel 1.53 per-contract ABI registration
item.

### Multi-stream IO event collapse (cross-cutting)

Same writer-side issue documented in 1.39 (EVM), 1.41 (PHP), 1.44
(Solana), 1.46 (Move), 1.48 (Cardano), 1.50 (Cairo), 1.52 (Flow)
and 1.53 (Fuel): the multi-stream IO event writer's
`toIOEventKind` collapses 13 `EventLogKind`s onto 4
`IOEventKind` buckets, losing the original kind byte and the
metadata string. PolkaVM's `seal_debug_message` lands in `stdout`
(via `Write`) and `seal_deposit_event` lands in `stderr` (via
`EvmEvent`), but the frontend cannot distinguish them from each
other or from EVM `LOG`s without reaching the embedded raw event
stream. Out of scope for any single recorder audit; flagged as a
writer-side fix in `codetracer_trace_writer_ffi.nim`'s
`toIOEventKind`.

### Read-side end-to-end content assertions

The audit tests assert the `.ct` file starts with the CTFS magic
and is materially populated. Verifying that the embedded event
stream contains the expected `register_call` /
`register_special_event` records (e.g.
`EventLogKind::EvmEvent` with `"ink_deposit_event"` metadata when
a contract runs `seal_deposit_event`) requires the
`codetracer_trace_reader_nim` dep added as a `[dev-dependencies]`
entry plus a small reader-walk helper. Tracked here for the next
pass (also open for Cairo, Cardano, Flow, and Fuel).

### Solidity-via-Revive blob coverage

`solidity.rs` already detects Solidity-via-Revive contract blobs
and adapts entry-point detection (`call` / `deploy` exports
instead of `main`) and the host-function display-name table
(EVM-style names like `getCallDataLoad` for the analogous Ecalli
indices). The audit's structured event routing (`seal_*` index
table in `tracer.rs`) currently keys on the Substrate /
pallet-revive index numbers and so applies uniformly to both Rust
and Solidity blobs — no Solidity-specific divergence is required.
Verified by `test_polkavm_function_entry_exit` and the existing
`solidity::tests` suite (10 passing).

## After this audit

Section 5.6's recorder list shows `codetracer-polkavm-recorder` as
audited (gaps closed for default-Ctfs CLI + Ecalli call-arg staging
+ host-function side effect routing through
`register_special_event` for `seal_debug_message` / `seal_deposit_event`
/ `seal_terminate` / Trap / Segfault / NotEnoughGas /
unhandled-host-function; intra-program function-call boundaries +
symbolic ink! call-arg decoding + replay-path tracing open as
DWARF-integration / metadata-decoding / RPC-integration follow-ups).
Audited recorder count: 11 → 12.

---

## Convention compliance follow-up — 2026-05-08

Mirrors the cairo / cardano / circom / flow / fuel / leo / miden / move
follow-ups: the recorder is now CTFS-only at the CLI surface, with the
canonical `CODETRACER_<NAME>_RECORDER_OUT_DIR` /
`CODETRACER_<NAME>_RECORDER_DISABLED` env-var contract from
`Recorder-CLI-Conventions.md` §4 / §5.

### CLI changes

* `--format` / `-f` removed from all three subcommands (`record`,
  `trace-ink`, `replay`).  Clap now rejects the flag at every level —
  exercised by `tests/test_cli.rs::test_format_flag_rejected_by_clap`.
* `OutputFormat` enum (and its `From<OutputFormat> for
  TraceEventsFileFormat` / `as_str` impls) deleted from `src/main.rs`.
* `RecordArgs.out_dir` / `TraceInkArgs.out_dir` / `ReplayArgs.out_dir`
  changed from `PathBuf` (with `default_value = "./ct-traces/"`) to
  `Option<PathBuf>`.  A new `resolve_out_dir` helper resolves
  `--out-dir` → `CODETRACER_POLKAVM_RECORDER_OUT_DIR` →
  `./ct-traces/` in priority order.
* New `recording_disabled()` helper reads
  `CODETRACER_POLKAVM_RECORDER_DISABLED` (`1` / `true`); each
  subcommand short-circuits with a "skipping trace recording" note when
  it is set.
* `--help` text now points users at `ct print` from
  `codetracer-trace-format-nim` for human-readable conversion.

### Library / tracer changes

* `src/recorder.rs::record(blob_path, out_dir)` no longer takes a
  `format` parameter; the writer is pinned to `TraceEventsFileFormat::Ctfs`.
* `src/tracer.rs::PolkaVmTracer::trace_program(blob_path, blob_bytes,
  out_dir)` no longer takes a `format` parameter; same pin via the new
  module-level `CTFS_FORMAT` constant.  The `events_filename`
  match-on-`format` collapsed to the unconditional `trace.bin`.
* `src/replay.rs::replay_contract_call(config, out_dir)` no longer takes
  a `format` parameter; the route through `recorder::record` is
  CTFS-only.  Inline `replay_contract_call` test rewritten to drop the
  `TraceEventsFileFormat::Json` argument.

### Tests

* `tests/test_cli.rs` extended with the six standard convention tests:
  - `test_recorded_trace_via_ct_print_json` — records a programmatic
    add-program blob through `recorder::record`, pipes the produced
    `.ct` file through `ct-print --json` from
    `codetracer-trace-format-nim`, and asserts on **structural
    anchors** (the fixture path `simple.polkavm` and at least one of
    the resolved register names `arg0` / `arg1` / `S0` / `S1` / `T0` /
    `SP` / `RA`).  Integer values are not asserted because the PolkaVM
    recorder's variable payload (`ValueRecord::Int { i, type_id }`
    over a `u64` register-type id) doesn't round-trip through
    `ct print --json` today (same pre-existing limitation as cardano /
    circom / flow / fuel / leo / miden / move).
  - `test_env_out_dir_used_when_flag_omitted` — sets
    `CODETRACER_POLKAVM_RECORDER_OUT_DIR=<tmp>` without `--out-dir`
    and asserts the env-supplied dir receives the `.ct` bundle.
  - `test_env_disabled_skips_recording` — sets
    `CODETRACER_POLKAVM_RECORDER_DISABLED=1` and asserts the recorder
    exits 0 with no trace artefacts written.
  - `test_format_flag_rejected_by_clap` — asserts clap rejects
    `--format json` at all three subcommand levels (`record` /
    `trace-ink` / `replay`).
  - `test_no_format_flag_in_help` — asserts `--help` (top-level + each
    subcommand) does not advertise `--format` or `CODETRACER_FORMAT`.
  - `test_help_mentions_ct_print` — asserts top-level `--help`
    mentions `ct print` so users discover the canonical conversion
    tool.
* `tests/test_ctfs_audit.rs::ctfs_format_advertised_in_record_help`
  **deleted**.  It asserted on the OLD `--format` contract (the flag
  must be advertised with `[default: ctfs]`), which is incompatible
  with the post-2026-05-08 contract (`--format` must not exist).  The
  three replacement assertions live in `tests/test_cli.rs`
  (`test_no_format_flag_in_help` / `test_format_flag_rejected_by_clap`
  / `test_help_mentions_ct_print`) and the equivalent `--help`
  greps live in `tests/verify-cli-convention-no-silent-skip.sh`.
* `tests/test_tracer.rs::test_polkavm_cli_record_with_blob` rewritten
  to invoke `CARGO_BIN_EXE_codetracer-polkavm-recorder` directly
  (instead of `cargo run -- ... --format json`) so the test exercises
  the binary that callers ship.

### New artefacts

* `Justfile` — standard `build` / `test` / `lint` / `verify-cli-convention`
  / `format` recipes.  `lint` and `test` both run
  `tests/verify-cli-convention-no-silent-skip.sh`.
* `tests/verify-cli-convention-no-silent-skip.sh` — shell-side
  verification that `--format` is absent from `--help` at all four
  levels (top + record + trace-ink + replay), `--out-dir` /
  `--version` / `ct print` are present where the convention requires
  them, and the two env vars are referenced in `src/`.  Wired into
  `just lint` and `just test`.

### Drive-by fixes

* `tests/test_tracer.rs` — pre-existing `unnecessary_map_or` warnings
  rewritten to `is_some_and`; pre-existing `unreachable_code` /
  unused `load_trace_metadata` helper deleted; `&out_dir` borrow
  warning removed.  Keeps `cargo clippy --locked --all-targets -- -D
  warnings` clean so `just lint` passes.
* `cargo fmt` applied across the crate (the toolchain has drifted
  since the 2026-05-02 audit commit; otherwise `just lint` would fail
  on `cargo fmt --check`).

### Verification

```
export LIBRARY_PATH=/nix/store/<…>-zstd-<…>/lib   # local libzstd workaround
cd /home/zahary/metacraft/codetracer-polkavm-recorder
cargo test --locked              # 47 lib + 11 cli + 2 audit + 15 tracer = 75 active passing
cargo clippy --locked --all-targets -- -D warnings   # clean
bash tests/verify-cli-convention-no-silent-skip.sh   # 18 ok lines, 0 fails
```

`tests/test_cli.rs::test_recorded_trace_via_ct_print_json` runs end-to-end
(does not skip) inside the metacraft workspace where
`../codetracer-trace-format-nim/ct-print` exists.

### Recorder-CLI-Conventions.md

The Implementation Status table now lists Polkavm as `✓ Compliant
(CTFS-only)` with the standard env-var notes.
