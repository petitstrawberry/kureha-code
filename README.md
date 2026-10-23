# Kureha Code

A code editor using ScarletUI for its interface and `floem-editor-core` for
editing history and revisions. The same application selects SWS on Scarlet and
winit on other desktop platforms. WebView integration is deferred.

The workbench has a resizable explorer on the left, file tabs above the editor,
and a status bar below. Each document retains its own undo history, dirty state,
caret, selection and scroll position. New/Open add tabs without replacing
unsaved work; closing a dirty tab or the window offers Save, Discard and Cancel.
Cut/Copy/Paste use ScarletUI's asynchronous platform clipboard. Paste, Cut and
Replace All each have independent undo steps. View can split the editor right or
down into two resizable groups with separate tab lists. A document opened in both
groups shares text and history while keeping separate selections and viewports.
Closing a group keeps its unsaved documents in the remaining group.

Open Folder loads a shallow tree asynchronously, expanding directories only on
demand. Listings are capped at 512 entries per directory, 4,096 cached entries,
32 levels and four concurrent reads; rows are virtualized. A limited listing
is labelled, and Open File can open files beyond the listing. Directory symlinks
are not traversed. Refresh reloads the current root.

Find/Replace is an overlay at the editor’s upper right: opening it preserves the
document viewport and scroll position. Search offers case, whole-word and regular
expression modes, wraps in either direction, and works on the active document.
Regular-expression replacements support capture groups. LF is used internally; CRLF input is saved as CRLF. Opened
files and Replace All results are limited to 16 MiB. The editor provides syntax colors, file-kind icons, language-server diagnostics, completion, definition navigation and hover information. Extensions are not implemented.

## Desktop development

```sh
cargo run -- path/to/source.rs
cargo run -- path/to/project-folder
cargo test --lib --bin kureha-code
```

ScarletUI owns rendering, windows, menus, input methods, file dialogs and
clipboard access. This repository does not use a separate UI toolkit or a
platform-specific replacement editor. Platform portability beyond builds
actually recorded in `docs/validation.md` remains unverified.

Open Folder uses ScarletUI’s common file-dialog API: the existing Files folder
chooser on Scarlet and the system folder panel on macOS. Find/Replace uses shared bounded search helpers;
TextView does not currently expose find helpers.

The File/Edit/View/Code menus expose the commands on Scarlet and in the native
macOS menu bar. Primary shortcuts use Command
on macOS and Control on other platforms: N/O/S for New/Open/Save; Shift+O for Open Folder; Shift+S for
Save As; W for Close Tab; Shift+W for Close Window; Control+Tab/Shift+Tab to switch tabs;
F for Find/Replace (also Control+H off macOS). F3/Shift+F3 and primary G/Shift+G
select the next/previous match. Standard
Z/Shift+Z, X/C/V and A handle Undo/Redo, clipboard and Select All in the editor.

| Action | Shortcut (primary is Command on macOS, Control elsewhere) |
| --- | --- |
| Quick Open / Command Palette | Primary P / Primary Shift P (or F1) |
| Go to line and optional column | Control G |
| Search project | Primary Shift F |
| Save all dirty documents | Primary Alt S |
| Split right / down | Primary \\ / Primary Shift \\ |
| Focus first / second group | Primary 1 / Primary 2 |
| Toggle line comments | Primary / |
| Indent / outdent | Primary ] / Primary [, or Tab / Shift Tab on selected lines |
| Move / copy line up or down | Alt Up/Down / Alt Shift Up/Down |
| Format / references / rename | Alt Shift F / Shift F12 / F2 |

Quick Open fuzzy-matches names and paths in a background index; `:line:column`
switches to line navigation and `>command` switches to the command palette.
Navigation overlays leave the editor's height unchanged. The bounded index skips
build/vendor directories and symlinks, supports an exclusion-only subset of root
ignore files, and reports incomplete scans; it is not a full Git ignore engine.

Project search includes unsaved buffers and shows file/line previews in the
sidebar. It is cancellable and bounded to 1 MiB per file, 64 MiB total and 2,000
matches. Replace All first checks that every source still matches its snapshot,
then opens changed files as unsaved buffers with one undo transaction per file.
It does not write files automatically. Incomplete results and replacement sets
over 128 files cannot be applied. Save All processes documents sequentially and
stops on cancellation or conflict. A file changed externally since opening or
saving is not silently overwritten; the changed buffer remains available.

Editor text uses a 14 px `TextView.font_stack(...)`: JetBrains Mono Nerd Font
Mono Regular, then Mplus 1 and Noto Sans CJK JP. UI chrome keeps ScarletUI's
default font. Host executables embed the fonts from `assets/fonts` with their
licenses, so moving the executable does not break font loading. Scarlet loads
the matching files from `/share/fonts` once per process.

## Scarlet application

`app.toml` is the single source for the app name, stable ID and build recipe.
`build.rs` supplies that identity to both desktop and Scarlet windows. Following
Cadence's app packaging model, `platforms/scarlet/bundle.toml` installs a finished
`/applications/kureha-code.app` through the SDK's `kind = "app"` layer.

The Cargo recipe is at the repository root because the SDK requires its build
sources to remain within the recipe directory. No copy of the build recipe is
shipped inside the finished app.

Scarlet includes this app through the remote bundle at a fixed, published commit:

```toml
[[layers]]
kind = "bundle"
source = { git = "https://github.com/petitstrawberry/kureha-code.git", rev = "<published-commit>" }
bundle = "platforms/scarlet/bundle.toml"
```

The distribution's `bundles/desktop/bundle.toml` contains the real pinned revision. No sibling checkout or local bundle override is required. To package a development checkout independently, run the SDK from a configured Scarlet project:

```sh
cargo scarlet app build --source /absolute/path/to/kureha-code \
  --target aarch64-unknown-scarlet --release \
  --output /absolute/path/to/new/kureha-code.app
```

The output must be a new directory. The SDK validates native ELF architecture,
loader and descriptor paths before publishing the `.app`. SGFX comes from the
Scarlet project, not a copied private dependency cache.

## Editing model

`src/editor.rs` adapts ScarletUI edit deltas to the independent Floem/Lapce editor
core. The engine owns undo groups and saved revisions; updates to the ScarletUI
document replace only the range reported by its rope delta. The full Floem UI,
Lapce application and Linux compatibility layers are not dependencies.

`src/files.rs` stages and synchronizes file contents before rename. Native
exclusive creation uses `scarlet-fs`; other platforms use the standard library.
Opening and saving run off the UI thread. Current ScarletUI TextView still
builds layout from its own document, so this integration does not yet establish
large-file rendering performance.

Save completion carries a stable document ID and revision, and cannot mark a
different tab or a newer revision clean. A saving tab is temporarily read-only;
other tabs remain editable. Clipboard mutations are discarded after tab,
document, selection or file-operation changes, including switching away and
back. Menu callbacks queue commands for the UI thread.

## Code features

Explorer and tabs use 16 px file-kind icons, including language accents and separate folder, package/configuration, document, image and archive shapes. The packaged app has its own leaf/code icon in `assets/kureha.svg` and `assets/kureha.png`.

Syntax parsing uses Syntect's pure Rust regex backend on one background worker. Rust, Python, JavaScript/TypeScript, JSON, TOML, Markdown and the included language grammars receive colors, including multiline strings and comments. Results are keyed by stable tab ID, engine revision and path; pending revisions coalesce and stale results are discarded. Documents above 1 MiB, 20,000 lines or a 16 KiB line fall back to plain text with a visible reason; parsing also has span/time bounds. Editing remains available.

The Code menu exposes Completion (Control+Space or primary I), Go to Definition
(F12 or primary-click), Hover (primary K then primary I), Problems, and Restart
Language Server. Formatting applies server edits as one undo transaction;
references can navigate to unopened files. F2 opens an inline rename input.
Rename currently accepts only edits to the current file: cross-file renames and
resource operations are rejected as a whole, with an explanation. Unsupported
server capabilities disable the corresponding request. Typing an identifier or member accessor schedules completion;
hovering a symbol schedules a server hover request. Completion and hover appear
in bounded editor overlays. Up/Down select a candidate and Enter/Tab accepts it
as one undo step, including compatible additional text edits. Snippet-form completions are not advertised or inserted as raw template text. Problems select their document range; error/warning underlines track the current revision. Escape closes the language panel.

Language servers are separate executables, not bundled or downloaded by Kureha. Defaults are `rust-analyzer`, `pylsp`, `typescript-language-server --stdio`, `vscode-json-language-server --stdio`, and `clangd`. They must be runnable on the platform hosting the editor. A host language server executable is not a Scarlet executable. Missing servers and initialization failures are reported in the status bar; the editor continues to work.

Configure commands and arguments in `$XDG_CONFIG_HOME/kureha-code/lsp.toml` (normally `~/.config/kureha-code/lsp.toml`):

```toml
[servers.rust]
command = "/absolute/path/to/rust-analyzer"
args = []

[servers.python]
command = "pylsp"
args = []
```

`KUREHA_LSP_CONFIG` selects another user configuration file. `KUREHA_LSP_RUST` and corresponding uppercase language IDs override just the executable. Project folders do not supply executable commands. Files must be saved to a path before language services can identify them. Communication uses bounded JSON-RPC over stdio with UTF-16 positions; edits and tab switches invalidate stale completion/navigation responses.


## Themes, minimap and terminal

View offers Light/Dark, minimap visibility and the integrated terminal. The editor
surface is flat and borderless; themes include syntax colors and subdued selection
and separators. Primary K then primary T switches theme; primary K then primary M
toggles the minimap. The overview has fixed two-pixel row spacing, clips and scrolls
with the document, and uses a translucent neutral viewport. Source and paint work
are bounded; very large documents use at most 768 sampled rows.

Primary J or Control+backtick toggles the terminal. It runs a real PTY shell in the
explorer root (otherwise the file’s directory), with ANSI colors, resizing,
scrollback and asynchronous output. macOS/Unix uses openpty; Scarlet uses /dev/ptmx
and /bin/sh. Clipboard paste is session-bound. Windows ConPTY is not implemented;
opening the terminal there reports unsupported. Terminal text selection/copy and
shell integration beyond the PTY are not implemented.

This is not yet VS Code feature parity. Arbitrarily nested editor groups,
multi-cursor editing, code folding, workspace-wide symbol rename, Git/diff tools,
debugging, extension hosting, and persistent workspace restoration remain open.
