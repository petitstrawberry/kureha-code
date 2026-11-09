# Kureha Code

A native, cross-platform code editor written in pure Rust with ScarletUI.
It runs without an embedded web browser.

<!-- ![Markdown source and preview](docs/screenshots/markdown-preview.png) -->

## Build and run

With Nix and flakes enabled:

```sh
nix develop
cargo run -- path/to/file-or-directory
```

The development shell supports Linux x86_64/AArch64 and macOS Apple Silicon.
With direnv, run `direnv allow` to load it automatically.

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

- [Scarlet bundle](platforms/scarlet/bundle.toml)
- [Build and runtime validation](docs/validation.md)
