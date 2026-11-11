# vivid_local_control

Native owner-authenticated local streams used by `vivid_sdk::local_control`.
The implementation was migrated from Vivido's `src/polling/transport.rs` under
its Apache-2.0 license, preserving Unix credentials and Windows pipe ownership.

The crate has an independent native build with no GUI/runtime dependencies.
It provides bounded accepts, read/write timeouts, clone cancellation and stream
pairs. Applications own their framing, request limits, lifecycle and endpoint
cleanup. See the SDK's [local-control guide](../docs/LOCAL-CONTROL.md).

Run `cargo fmt --all --check`, `cargo test --all-targets` and strict Clippy from
this directory. Windows-specific code and tests can also be checked with
`--target x86_64-pc-windows-msvc`; execution on native Windows is a separate gate.
