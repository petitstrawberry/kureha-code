# Text Tools example extension

Everyday text commands for Kureha Code, written with the minimal SDK in
[`../sdk`](../sdk).

| Commands | Works on |
| --- | --- |
| Sort Lines, Remove Duplicate Lines, Reverse Lines, Trim Trailing Whitespace | Selected lines (grown to whole lines), or the whole document |
| Minify JSON | Selection or whole document. Key order and number text are kept |
| Format Selection as JSON | Selection only, for JSON inside other files (Markdown, logs, tests). JSON files use Code: Format Document |
| Count Lines, Words and Characters | Selection or whole document. Shown in the status bar |
| Convert to UPPER / lower / Title / snake_case / CONSTANT_CASE / camelCase / PascalCase / kebab-case | Selection only |
| Base64 Encode / Decode, URL Encode / Decode | Selection only. Decoding refuses bytes that are not UTF-8 |

Text Tools is also a **JSON formatter**: Code: Format Document (Alt+Shift+F)
and format-on-save use it for `.json` files, with the editor's indentation.
To keep the JSON language server instead, add `[formatter]` / `json = "lsp"`
to the settings file.

Shortcuts (while the editor has focus): Alt+Shift+S sort lines,
Alt+Shift+D remove duplicate lines, Alt+Shift+U upper case,
Alt+Shift+L lower case, Alt+Shift+C count.

Line commands keep CRLF line endings and the document's final newline. Input
is limited to 192 KiB so every command stays inside the host's time budget.

```sh
nix develop --command cargo build --release --target wasm32-unknown-unknown \
  --manifest-path examples/extensions/text-tools/Cargo.toml
mkdir -p ~/.config/kureha-code/extensions
ln -s "$PWD/examples/extensions/text-tools" ~/.config/kureha-code/extensions/text-tools
```

Restart Kureha Code and type `Text Tools` in the command palette.
Run the transformation tests natively with
`cargo test --manifest-path examples/extensions/text-tools/Cargo.toml`.
