# Kureha Code

A code editor using ScarletUI for its interface and `floem-editor-core` for
editing history and revisions. The same application selects SWS on Scarlet and
winit on other desktop platforms. WebView integration is deferred.

The workbench has a resizable explorer on the left, file tabs above the editor,
and a status bar below. Each document retains its own undo history, dirty state,
caret, selection and scroll position. New/Open add tabs without replacing
unsaved work; closing a dirty tab or the window offers Save, Discard and Cancel.
Cut/Copy/Paste use ScarletUI's asynchronous platform clipboard. Paste, Cut and
Replace All each have independent undo steps.

Open Folder loads a shallow tree asynchronously, expanding directories only on
demand. Listings are capped at 512 entries per directory, 4,096 cached entries,
32 levels and four concurrent reads; rows are virtualized. A limited listing
is labelled, and Open File can open files beyond the listing. Directory symlinks
are not traversed. Refresh reloads the current root.

Find/Replace is literal, case-sensitive, wraps in either direction, and works on
the active document. LF is used internally; CRLF input is saved as CRLF. Opened
files and Replace All results are limited to 16 MiB. Syntax highlighting,
language servers, completion and extensions are not implemented.

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
chooser on Scarlet and the system folder panel on macOS. Find/Replace uses local literal-search helpers;
TextView does not currently expose find helpers.

The File/Edit menus expose the commands. Primary shortcuts use ScarletUI's
Control modifier: N/O/S for New/Open/Save; Shift+O for Open Folder; Shift+S for
Save As; W for Close Tab; Shift+W for Close Window; Tab/Shift+Tab to switch tabs;
F/H for Find/Replace. F3 and Shift+F3 select the next/previous match. Standard
Z/Shift+Z, X/C/V and A handle Undo/Redo, clipboard and Select All in the editor.

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

For a local Scarlet checkout with the native SGFX environment prepared, add this
layer to the appropriate image in its `scarlet.local.toml`:

```toml
[[images.rootfs.layers]]
kind = "bundle"
path = "../../../kureha-code/platforms/scarlet/bundle.toml"
```

Then use the normal `cargo scarlet image` command. To package just the app from
that project's directory:

```sh
cargo scarlet app build --source ../../../kureha-code \
  --target aarch64-unknown-scarlet --release \
  --output ../../../kureha-code/dist/kureha-code.app
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
