# Upper Case example extension

Upper-cases the current selection. It is a complete extension written against
the raw ABI in [`docs/extensions.md`](../../../docs/extensions.md).

```sh
nix develop --command cargo build --release --target wasm32-unknown-unknown \
  --manifest-path examples/extensions/upper-case/Cargo.toml
mkdir -p ~/.config/kureha-code/extensions
ln -s "$PWD/examples/extensions/upper-case" ~/.config/kureha-code/extensions/upper-case
```

Restart Kureha Code and run **Upper Case: Upper-case Selection** from the
command palette.
