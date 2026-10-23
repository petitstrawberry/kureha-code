# Native LSP acceptance

This standalone binary compiles Kureha's actual `src/lsp.rs` with its existing
`serde_json` dependency, without linking the GUI. It launches a real rust-analyzer
through `Client::spawn`; it contains no substitute server or canned responses.

Build from a Scarlet Rust development shell, using a separate target directory:

```sh
cd /Users/petitstrawberry/Development/Rust/Scarlet
nix develop --command sh -c 'cd /Users/petitstrawberry/Development/Rust/kureha-code && cargo build --locked --offline --manifest-path tools/native-lsp-probe/Cargo.toml --target-dir target/native-lsp-probe-scarlet --target aarch64-unknown-scarlet'
```

The executable is
`target/native-lsp-probe-scarlet/aarch64-unknown-scarlet/debug/kureha-native-lsp-probe`
under the Kureha checkout. It uses the native `/bin/scarlet-ld` interpreter.
This helper does not launch a VM. Stage it in a disposable native acceptance image.

Once staged as `/bin/kureha-native-lsp-probe`, launch directly from init or a
noninteractive shell, with `CARGO` and `RUSTC` absent:

```sh
PATH=/bin:/usr/bin /bin/kureha-native-lsp-probe --server /bin/rust-analyzer --timeout-secs 300
```

Do not source `/etc/profile.sh` for this acceptance run: its extended PATH would
hide GUI tool discovery failures. Kureha preserves the explicit init PATH and,
only for Scarlet server names `rust-analyzer` or `/bin/rust-analyzer`, supplies
child `CARGO` and `RUSTC` from `/opt/scarlet/toolchains/rust/current/bin` if those
variables are absent. Explicit values (including empty ones) are preserved.
Other server paths/wrappers receive no tool overrides. Missing or empty PATH gets
`/opt/scarlet/toolchains/rust/current/bin:/bin:/usr/bin` for the child only.
The parent process environment is never modified.

Host verification, in the same shell, uses its own target directory:

```sh
cd /Users/petitstrawberry/Development/Rust/kureha-code
cargo build --locked --offline --manifest-path tools/native-lsp-probe/Cargo.toml --target-dir target/native-lsp-probe
target/native-lsp-probe/debug/kureha-native-lsp-probe --server /absolute/path/to/rust-analyzer --timeout-secs 120
```

The server path defaults to `rust-analyzer`. Cargo, rustc, and matching rust-src
must be discoverable by the server. Host environment behavior is unchanged.
There is no sysroot bypass. The probe disables proc macros and build scripts,
and turns off check-on-save. Both the probe and Scarlet Rust GUI sessions
disable initial cache priming and proc macros. Formatting is outside this
probe's acceptance scope.

The scratch workspace and its isolated Cargo home are created under `/tmp`, whose
tmpfs supports advisory locks even in the diskless acceptance image. The fixture
has no registry dependencies. Standard-library metadata still needs its real
registry dependency sources; supply those with the installed rust-src package.
Its `Cargo.toml` declares a nonstandard `fixture/entry.rs` binary target and a
local `native_values` Cargo path dependency. An unopened `numbers.rs` module
reexports the dependency's constants. Only `entry.rs` receives didOpen.
Success requires all of these observed server results:

- Initialize advertises UTF-16 (or the protocol default), hover, definition,
  completion, open/close and text synchronization.
- Before didOpen, server status becomes healthy and quiescent, and
  `rust-analyzer/analyzerStatus` reports a loaded workspace rooted at the scratch
  `Cargo.toml`. The package count includes sysroot packages and is not fixed.
- Hover of the inferred local `answer` reports exactly `let answer: u32`.
- Definition of `VALUE` crosses the Cargo dependency to the unopened
  `values/src/lib.rs`, range `(0,10)..(0,15)` in zero-based LSP positions.
- Completion includes label `answer`, kind `6`, detail `u32`, and an exact
  `answer` replacement at `(3,16)..(3,22)`.
- Definition of `std::string::String` reaches the real
  `library/alloc/src/string.rs`, at the actual `pub struct String` declaration.
  This fails when usable standard-library sources are absent.
- didChange version 2 changes `numbers::VALUE` to `numbers::OTHER` without saving.
  Hover must become `let answer: u64`; definition must reach `(1,10)..(1,15)` in
  the unopened dependency; completion detail must become `u64` with the same
  exact edit. The file on disk must still contain version 1.
- didClose, shutdown returning null, exit, and a successful child exit status.

Requests share the overall deadline (default 180s, configurable from 1 to 3600s). A watchdog allows five additional seconds for
cleanup before forcing failure, including hung spawn/drop. Normal completion
and errors remove the scratch workspace; a hard watchdog exit can leave the
logged scratch directory and child behind for the VM owner to clean up.
Stages, observed values and failures go to stderr. Failures exit 1 and print
`SCARLET_NATIVE_LSP_FAILED`; success exits 0 and prints
`SCARLET_NATIVE_LSP_OK target_os=scarlet` (or the host OS). `--help` exits 0
without a success marker. Require both the exit status and the marker.

Verified on the host with rust-analyzer 1.94.0-nightly: the complete cross-Cargo
acceptance sequence passes, including real std sources and exit status 0.
The AArch64 Scarlet executable cross-builds successfully. Guest runtime
acceptance is recorded separately from these build and host results. Host evidence is in
`artifacts/native-lsp/host.log` in the Kureha checkout.
