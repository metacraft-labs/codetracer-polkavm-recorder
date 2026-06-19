## codetracer-polkavm-recorder

A recorder of PolkaVM program executions that produces [CodeTracer](https://github.com/metacraft-labs/CodeTracer) traces.

> [!WARNING]
> Currently it is in a very early phase: we're welcoming contribution and discussion!

### Overview

codetracer-polkavm-recorder loads PolkaVM program blobs (ink! smart contracts, Solidity compiled via Revive), executes them with step-level tracing, and resolves DWARF debug information to recover variable names. It emits structured trace files compatible with CodeTracer.

### Building

```bash
cargo build
```

### Usage

Record a trace from a PolkaVM program blob:

```bash
codetracer-polkavm-recorder record <blob-file> --out-dir <dir>
# Produces a CTFS multi-stream `.ct` bundle plus trace_metadata.json /
# trace_paths.json in <dir>.
```

Trace an ink! contract via the ink! testing framework:

```bash
codetracer-polkavm-recorder trace-ink --contract <blob> --message <name> --out-dir <dir>
```

Replay an on-chain contract execution:

```bash
codetracer-polkavm-recorder replay --address <addr> --selector <name> --out-dir <dir>
```

The recorder always writes traces in the canonical CodeTracer CTFS
multi-stream format (see
[`Recorder-CLI-Conventions.md`](https://github.com/metacraft-labs/codetracer-specs)
§4). To convert a recorded `.ct` bundle to JSON or text for
inspection, use `ct print` (shipped with
[`codetracer-trace-format-nim`](https://github.com/metacraft-labs/codetracer-trace-format-nim)).

However, you probably want to use it in combination with CodeTracer, which would be released soon.

### Examples

See [`examples/`](examples/README.md) for small PolkaVM programs you
can record and replay with `ct` to exercise the recorder end-to-end.

### Architecture

The recorder is organized into the following modules:

* `recorder.rs` — top-level recording orchestration and trace file output
* `tracer.rs` — step-level PolkaVM execution tracing
* `dwarf_variables.rs` — DWARF debug info parsing to recover variable names
* `host_functions.rs` — ecalli dispatch (host function call handling)
* `ink_testing.rs` — ink! contract testing integration
* `solidity.rs` — Revive-compiled Solidity detection and source mapping
* `replay.rs` — on-chain contract replay

### Testing

Test programs live in `test-programs/assembly/` and `test-programs/rust/`. Run the test suite with:

```bash
cargo test
```

### Environment variables

* `CODETRACER_POLKAVM_RECORDER_OUT_DIR` — fallback for `--out-dir` when the flag is omitted (convention: `Recorder-CLI-Conventions.md` §5)
* `CODETRACER_POLKAVM_RECORDER_DISABLED` — set to `1` or `true` to skip recording entirely (the recorder still validates inputs and exits cleanly)
* `CODETRACER_POLKAVM_RECORDER_LOG_LEVEL` — recorder log verbosity (advisory)
* `POLKAVM_SANDBOXING_ENABLED=false` — disable PolkaVM sandboxing for CI or environments without user namespaces
* `RUST_LOG` — controls log verbosity (standard `env_logger` syntax, e.g. `RUST_LOG=debug`)

### Contributing

We'd be very happy if the community finds this useful, and if anyone wants to:

* Use and test the PolkaVM/ink! support or CodeTracer.
* Provide feedback and discuss alternative implementation ideas: in the issue tracker, or in our [discord](https://discord.gg/qSDCAFMP).
* Contribute code to enhance the PolkaVM support of CodeTracer.
* Provide [sponsorship](https://opencollective.com/codetracer), so we can hire dedicated full-time maintainers for this project.

### Legal info

LICENSE: MIT

Copyright (c) 2025 Metacraft Labs Ltd
