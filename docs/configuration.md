# Configuration

Open **Kureha Code → Settings…** (`⌘,` on macOS, `Ctrl+,` elsewhere).
Changes made in Settings are saved automatically.

## Files

Files live in `~/.config/kureha-code/`, or `$XDG_CONFIG_HOME/kureha-code/`
when `XDG_CONFIG_HOME` is set.

| File | Purpose | Open from Settings |
| --- | --- | --- |
| `settings.toml` | Editor preferences and extension permissions | **Open Settings File…** |
| `theme.toml` | Custom UI, syntax, font and terminal colors | **Edit Custom Theme…** |
| `lsp.toml` | Language server commands and arguments | — |

Settings and theme file changes apply automatically. Invalid edits keep the last
working configuration. Omitted settings use the defaults below.

## Editor settings

Keys belong at the top level of `settings.toml` unless a table is shown.

| Key | Default | Description |
| --- | --- | --- |
| `color_theme` | Unset: use `theme.toml`, otherwise **Kureha Light** | UI theme name. Selecting a UI theme in Settings resets the separate code theme. |
| `code_theme` | Unset: follow the UI/custom theme | Independent editor and terminal theme. |
| `font_size` | Unset: use the theme's size (**14** by default) | Code font size, from 8 to 48. |
| `hide_minimap` | `false` | Hide the editor minimap. |
| `word_wrap` | `true` | Wrap long lines to the editor width. |
| `wrap_at_words` | `true` | Prefer word boundaries when wrapping; ignored when `word_wrap` is off. |
| `indent_depth_background` | `false` | Subtle colored bands and separators in leading whitespace. |
| `active_block_guide` | `false` | Connect enclosing brackets with a guide in their syntax color. |
| `inline_type_hints` | `false` | Show inferred types beside code when the language server provides them. Hints are excluded from saved and copied text. |
| `insert_spaces` | `true` | Convert Tab input to spaces; does not rewrite existing text. |
| `tab_size` | `4` | Indentation width, from 1 to 8. Backspace in leading whitespace removes one indentation step. |
| `format_on_save` | `false` | Format the document before saving. |
| `[formatter] <language>` | Unset: first available extension formatter, otherwise language server | Choose `"lsp"` or an installed formatter extension ID for a language. |

For example, enable inline types and use two spaces per indentation step:

```toml
inline_type_hints = true
insert_spaces = true
tab_size = 2
```

Clean open files automatically reload external disk changes. Unsaved edits are
preserved, and saving checks for conflicting disk changes; this behavior has no
settings toggle.

## Themes and fonts

**Kureha Light** is the default; **Kureha Dark** is its dark counterpart.
**Sakura** preserves the original pink palette. **Classic Light** and
**Classic Dark** remain available, including their older `light` / `dark` names.
Other bundled themes include Dracula, Monokai, Nord, Solarized Light/Dark,
Gruvbox Dark and Tokyo Night.

For custom colors, start with the commented [theme example](../assets/theme.toml).
The default code font is bundled JetBrains Mono. Set `[code] font_family` to an
installed family or `font_file` to a TTF/OTF file in `theme.toml` to change it.
A `font_size` in `settings.toml` takes precedence over `[code] font_size`.
Omit `color_theme` from `settings.toml` to use your custom `theme.toml`.

Syntax highlighting is refined with semantic colors when supported by the
language server. The editor and minimap share syntax colors and custom overrides;
Rust also colors nested brackets.

## Language servers

Install language servers separately. Built-in commands are:

| Language ID | Default command | Arguments |
| --- | --- | --- |
| `rust` | `rust-analyzer` | None |
| `python` | `pylsp` | None |
| `javascript`, `javascriptreact`, `typescript`, `typescriptreact` | `typescript-language-server` | `--stdio` |
| `json` | `vscode-json-language-server` | `--stdio` |
| `c`, `cpp` | `clangd` | None |

Override a command in `lsp.toml`:

```toml
[servers.rust]
command = "/path/to/rust-analyzer"
args = []
```

Command precedence is: `KUREHA_LSP_<LANGUAGE>` environment variable, `lsp.toml`,
an enabled extension's server, then the built-in command. For example,
`KUREHA_LSP_RUST` overrides Rust's command with no arguments.
`KUREHA_LSP_CONFIG` overrides the path to `lsp.toml`.

## Extensions

These keys also belong in `settings.toml`. Permissions can be managed in Settings;
see [extension support](extensions.md) for details.

| Key | Default | Description |
| --- | --- | --- |
| `[extensions] disabled` | `[]` | IDs of disabled extensions. Their existing permissions are retained. |
| `[extensions] allow_environment` | `[]` | Extension IDs allowed to change the environment of programs Kureha starts. |
| `[extensions.allow_process] <extension-id>` | `[]` per extension | Program names the extension may run, such as `"ruff"`. Use bare names, not paths or command lines. |

## Environment overrides

| Variable | Default | Purpose |
| --- | --- | --- |
| `XDG_CONFIG_HOME` | `~/.config` | Base configuration directory. |
| `KUREHA_THEME` | `<config directory>/kureha-code/theme.toml` | Custom theme file path; `settings.toml` is read from the same directory. |
| `KUREHA_LSP_CONFIG` | `<config directory>/kureha-code/lsp.toml` | Language server configuration file. |
| `KUREHA_LSP_<LANGUAGE>` | Unset | Override one language server command; uppercase the language ID. |
