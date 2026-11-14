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
Stop example.format-tools Running ruff" withdraws it.

The tools also read the project's configuration. Prettier in particular loads
JavaScript configuration files and plugins (`prettier.config.js`, say), so
formatting a file runs code from that project. Allow Prettier only if you trust
the projects you open.

Programs are found in the absolute entries of the editor's `PATH`; relative
entries such as `node_modules/.bin` are skipped. An absolute entry that points
into a project (an activated virtualenv, say) is used like any other.

Installing Format Tools makes it the formatter for these languages. To keep a
language server's formatting for one of them, set it in the settings file:

```toml
[formatter]
python = "lsp"
```

To install it, run this from the repository and restart Kureha Code:

```sh
nix develop --command python3 scripts/build-extension.py --install examples/extensions/format-tools
```

It builds the module and puts `extension.toml`, `extension.wasm` and this
README in `~/.config/kureha-code/extensions/format-tools.kx/` (or under
`KUREHA_EXTENSIONS_DIR`). Run it again after changing the code; `--out <folder>`
puts the folder somewhere else.
