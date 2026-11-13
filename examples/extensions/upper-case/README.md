# Upper Case example extension

Upper-cases the current selection. It is a complete extension written against
the raw ABI in [`docs/extensions.md`](../../../docs/extensions.md).

To install it, run this from the repository and restart Kureha Code:

```sh
nix develop --command python3 scripts/build-extension.py --install examples/extensions/upper-case
```

It builds the module and puts `extension.toml`, `extension.wasm` and this
README in `~/.config/kureha-code/extensions/example.upper-case/` (or under
`KUREHA_EXTENSIONS_DIR`). Run it again after changing the code; `--out <folder>`
puts the folder somewhere else.

Restart Kureha Code and run **Upper Case: Upper-case Selection** from the
command palette.
