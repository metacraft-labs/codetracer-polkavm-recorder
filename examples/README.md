## codetracer-polkavm-recorder examples

Small PolkaVM programs you can record and replay with CodeTracer to
exercise the recorder end-to-end. They are deliberately tiny so the
trace stays readable and the focus stays on the workflow rather than
the workload.

### Prerequisites

* `ct` (the CodeTracer CLI) on your `PATH`.
* The recorder built locally:

  ```bash
  cargo build --release
  ```

* A PolkaVM program blob (a `.polkavm` file). If you don't have one
  yet, the recommended starting point is to build one from this
  repository — the host example in this directory does it without
  requiring a RISC-V cross-compilation toolchain:

  ```bash
  cargo run --example build_flow_test_blob
  ```

  This writes `test-programs/rust/flow_test.polkavm` by driving
  PolkaVM's `ProgramBlobBuilder` directly. Reach for that API (or this
  example) before wrestling with a cross-compiler — it is by far the
  smoothest path for newcomers.

### Two-step workflow: record, then replay

Produce a `.ct` trace bundle, then open it in the CodeTracer GUI:

```bash
ct record path/to/program.polkavm
# Writes a `.ct` bundle under the default trace directory.

ct replay -t path/to/trace.ct
# Opens the recorded trace in the CodeTracer GUI.
```

Use this when you want to inspect a trace later, share it, or replay
the same execution multiple times without re-recording.

### One-step workflow: record and open

```bash
ct run path/to/program.polkavm
```

`ct run` records the execution and immediately opens the resulting
trace in the GUI. Reach for it during iterative debugging when you do
not need to keep the trace around.

### Walkthrough: `flow_test.polkavm`

1. Build the blob:

   ```bash
   cargo run --example build_flow_test_blob
   ```

   The program computes `((10 + 32) * 2) + 10 == 94` with each
   intermediate value bound to a named local (`a`, `b`, `sum_val`,
   `doubled`, `final_result`).

2. Record and open in one shot:

   ```bash
   ct run test-programs/rust/flow_test.polkavm
   ```

3. In the GUI, step through `compute()`. Because the recorder resolves
   DWARF line *and column* information, step-over advances to the
   next statement on multi-statement lines rather than re-stopping on
   the same line — you can land precisely on each assignment in turn.
   This is the "column-aware step-over" behaviour wired up in
   `tracer.rs`; the dedicated regression test in
   `tests/column_aware_step_over.rs` asserts that consecutive
   statements on the same source line receive distinct column values.

4. Inspect the locals at each stop: you should see `a = 10`,
   `b = 32`, `sum_val = 42`, `doubled = 84`, and finally
   `final_result = 94`.

If you prefer the two-step flow for the same program:

```bash
ct record test-programs/rust/flow_test.polkavm
ct replay -t <printed-trace-path>
```

### Adding more examples

Drop additional blob builders next to `build_flow_test_blob.rs`. Keep
them small and self-contained — the goal is to give a new contributor
a runnable trace in under a minute.
