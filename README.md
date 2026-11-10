# Kureha Code

A native, cross-platform code editor written in pure Rust with ScarletUI.

![Markdown source and preview](docs/screenshots/markdown-preview.png)

## Build and run

With Nix and flakes enabled:

```sh
nix develop
cargo run -- path/to/file-or-directory
```

The development shell supports Linux x86_64/AArch64 and macOS Apple Silicon.
With direnv, run `direnv allow` to load it automatically.

## Package

Inside `nix develop`, build a release package with [Appcoat](https://github.com/petitstrawberry/appcoat):

```sh
python3 scripts/package.py
```

On macOS, open `dist/macos/Kureha Code.app` or copy it to Applications.
On Linux, the result is `dist/linux/kureha-code`; run `bin/kureha-code` inside it.
For desktop launchers, add that directory's `bin` to `PATH` and `share` to
`XDG_DATA_DIRS`. Linux runtime libraries are provided by the development shell;
the package does not bundle them. macOS packages are not distribution-signed or notarized.

Use `--artifact /path/to/kureha-code` to package an existing binary, or
`--output /path/to/new-package` to choose another destination. Existing packages
are never overwritten.

For Scarlet, package an executable built for your image
before using the [Scarlet bundle](platforms/scarlet/bundle.toml):

```sh
python3 scripts/package.py --platform scarlet --target aarch64-unknown-scarlet \
  --artifact /path/to/scarlet/kureha-code
```

`riscv64gc-unknown-scarlet` is also accepted. The bundle consumes the resulting
`dist/scarlet/kureha-code.app`; build it for the architecture of your image.

## Configuration

**View → Customize Theme…** opens `~/.config/kureha-code/theme.toml`.
See the [configuration example](assets/theme.toml) for available settings.

Language servers must be installed separately. Override their commands in
`~/.config/kureha-code/lsp.toml`:

```toml
[servers.rust]
command = "/path/to/rust-analyzer"
args = []
```

Both configuration paths respect `$XDG_CONFIG_HOME`.

## Development

Run tests inside the development shell:

```sh
cargo test --lib --bin kureha-code
```
