# Format Tools example extension

Code: Format Document (Alt+Shift+F) and format-on-save through formatters you
already have installed. Written with the minimal SDK in [`../sdk`](../sdk).

| Languages | Program |
| --- | --- |
| Python | `ruff format` |
| JavaScript, TypeScript (and JSX/TSX) | `prettier` |
| C, C++ | `clang-format` |

Each tool reads the document on standard input, runs in the document's folder
so it finds the project's configuration, and its output replaces the document.
A failing tool (a syntax error, say) leaves the document as it was and shows
the tool's first error line.

The extension declares these programs in `[capabilities] process`, but nothing
runs until you allow it: run "Extensions: Allow example.format-tools to Run
ruff" (and so on) from the command palette. A program you allow can run with any
arguments and with your permissions, so allow only tools you trust. "Extensions:
Stop example.format-tools Running ruff" withdraws it. Programs are found on the
editor's `PATH`, never inside the workspace.

Installing Format Tools makes it the formatter for these languages. To keep a
language server's formatting for one of them, set it in the settings file:

```toml
[formatter]
python = "lsp"
```

```sh
nix develop --command cargo build --release --target wasm32-unknown-unknown \
  --manifest-path examples/extensions/format-tools/Cargo.toml
mkdir -p ~/.config/kureha-code/extensions
ln -s "$PWD/examples/extensions/format-tools" ~/.config/kureha-code/extensions/format-tools
```
