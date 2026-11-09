# Kureha Code

A code editor for Scarlet and desktop platforms, with a pink Bubble Glass theme,
split editors, language-server support, live Markdown previews and a built-in
terminal.

![Markdown source and preview](docs/screenshots/markdown-preview.png)

## Getting started

Open a file with **File → Open** or browse a project with **Open Folder**.
The workspace starts empty; **New File** creates an untitled document.
Closing a document with unsaved changes offers Save, Discard and Cancel.

Each document keeps its undo history. Drag tabs to reorder them or move them
between editor groups. Drop a tab on a pane's edge to split that pane; drop on
the outer edge of the editor area to use that entire side of the workspace.
Splits can combine horizontal and vertical layouts. The same document can be
shown in several groups with independent selections and scroll positions.

Quick Open finds files by name or path. Start its query with `:` to jump to a
line, or `>` to search commands. Find/Replace supports case-sensitive, whole-word
and regular-expression searches; regex replacements support capture groups.
Project search includes unsaved text. Project replacements remain unsaved until
you save the affected files.

## Shortcuts

“Primary” means Command on macOS and Control elsewhere.

| Action | Shortcut |
| --- | --- |
| New / Open / Save | Primary N / O / S |
| Open Folder / Save As | Primary Shift O / S |
| Save All | Primary Alt S |
| Close Tab / Close Window | Primary W / Primary Shift W |
| Next / Previous Tab | Control Tab / Control Shift Tab |
| Quick Open | Primary P |
| Command Palette | Primary Shift P or F1 |
| Go to Line | Control G |
| Find / Search Project | Primary F / Primary Shift F |
| Next / Previous Match | F3 / Shift F3 |
| Split Right / Down | Primary \ / Primary Shift \ |
| Focus First / Second Group | Primary 1 / 2 |
| Back / Forward | macOS: Control - / Control Shift -; elsewhere: Alt Left / Right |
| Toggle Line Comment | Primary / |
| Indent / Outdent | Primary ] / [ |
| Move / Copy Line | Alt Up/Down / Alt Shift Up/Down |
| Format / References / Rename | Alt Shift F / Shift F12 / F2 |
| Open Markdown Preview | Primary Shift V |
| Select Color Theme | Primary K, then Primary T |
| Toggle Minimap | Primary K, then Primary M |
| Toggle Panel | Primary J or Control Backtick |
| New Terminal Session | Control Shift Backtick |

## Appearance

**View → Select Color Theme** changes the whole application. The default Kureha
palette pairs pale pink surfaces with rose accents. Other choices include Light,
Dark, Dracula, Monokai, Nord, Solarized Dark/Light, Gruvbox Dark and Tokyo Night.

**Select Code Color Theme** changes the editor and terminal together while
keeping the surrounding interface's colors. In either picker, arrow keys or
hovering preview a theme immediately; Enter or clicking keeps it, and Escape
restores the previous theme. Picker choices apply to the current session.

For persistent customization, choose **Customize Theme…** and save the generated
TOML file. Changes reload automatically. Invalid edits leave the last working
theme in place and show an error in the status bar. The default location is
`~/.config/kureha-code/theme.toml`, or `$XDG_CONFIG_HOME/kureha-code/theme.toml`.
`KUREHA_THEME` selects another file. **Reload Custom Theme** also reloads font files.

For example, keep the pink interface and use Tokyo Night for code and terminals:

```toml
name = "My workspace"
base = "kureha"

[code]
theme = "tokyo-night"
font_family = "Menlo"
font_size = 16.0
```

Use `font_file = "/path/to/font.ttf"` instead of `font_family`
to load a TTF/OTF; relative paths resolve beside the theme file and `~/` is
supported. Editor font sizes range from 8 to 48 logical pixels. The default
code font is bundled JetBrains Mono, with Japanese fallback fonts.

Optional `[terminal]` settings override its background, foreground, cursor and
16 ANSI colors. Existing output updates when the theme changes. Programs that
request explicit RGB or extended 256-color values retain those colors.
See the [complete configuration example](assets/theme.toml) and
[palette credits](assets/themes/README.md).

The minimap can be hidden from View. Drag its viewport indicator to scroll;
dragging continues outside the minimap until you release the pointer.
On macOS, drag the empty title strip to move the window.

## Markdown and images

**Open Markdown Preview** opens a preview tab in the current group.
**Open Markdown Preview to Side** opens it next to the source editor. Preview
tabs support the usual tab dragging, splitting and closing. Editing the source
updates the preview without saving; closing either tab leaves the other open.

The View menu and command palette offer:

- **Show Markdown Source** to return to the associated editor.
- **Toggle Markdown File Sync** to follow that editor's active Markdown file,
  or keep previewing the current file.
- **Toggle Markdown Scroll Sync** to enable or disable scrolling together.
- **Markdown: Refresh Preview** to reload images changed outside the editor.

File and scroll synchronization start enabled. Scroll positions are approximate
within long paragraphs and code blocks.

Previews support headings, quotes, nested lists, tasks, tables, code highlighting,
links and embedded images. Heading links jump within the preview; file links open
in the editor and web links open in your browser. Local images are supported on
all platforms; HTTP/HTTPS images are also supported on macOS, Windows and Linux.
Full HTML/CSS layout and preview text selection are not available yet.

PNG, JPEG, GIF, WebP, BMP and ICO files also open as image tabs with Fit, 100% and
zoom controls. Enlarged images scroll in both directions. Transparent images show
a checkerboard, and GIFs show their first frame. SVG files open as text.

## Terminal and panel

**Toggle Panel** shows or hides the panel independently of editor splits.
It opens below the editors. **+** starts a shell session, a session's **×** closes
that shell, and the far-right **×** hides the panel while keeping sessions running.
Drag a session to the workspace's right edge to dock the panel beside the editors,
or to the bottom edge to return it below.

New sessions start in the open project folder, or the current file's directory.
Sessions support resizing, scrollback and clipboard paste. **Terminal: Restart
Session** restarts the selected shell. Windows terminals are not supported yet;
terminal text selection and copying are also not available.

## Language support

Syntax colors are included for Rust, Python, JavaScript/TypeScript, JSON, TOML,
Markdown and other bundled languages. Language servers add completion, hover
documentation, diagnostics, Go to Definition, references, formatting and rename.
Rename currently supports changes within the current file only.

Install the servers you need and make them available on PATH:

| Language | Default server |
| --- | --- |
| Rust | `rust-analyzer` |
| Python | `pylsp` |
| JavaScript / TypeScript | `typescript-language-server --stdio` |
| JSON | `vscode-json-language-server --stdio` |
| C / C++ | `clangd` |

Files must be saved before language services can identify them. Configure custom
commands in `~/.config/kureha-code/lsp.toml` (or under `$XDG_CONFIG_HOME`):

```toml
[servers.rust]
command = "/absolute/path/to/rust-analyzer"
args = []
```

`KUREHA_LSP_CONFIG` selects another configuration file. `KUREHA_LSP_RUST` and
corresponding uppercase language IDs override a server's executable.

## Build and run

Install Nix with flakes enabled, then enter the development shell:

```sh
nix develop
cargo run -- path/to/source.rs
# Or open a project:
cargo run -- path/to/project-folder
```

The development shell supports Linux x86_64/AArch64 and macOS Apple Silicon.
With direnv installed, `direnv allow` loads it automatically. To run tests:

```sh
cargo test --lib --bin kureha-code
```

For Scarlet packaging, see [the bundle manifest](platforms/scarlet/bundle.toml).
Build and runtime verification is recorded in [validation notes](docs/validation.md).

Multi-cursor editing, code folding, project-wide rename, Git/diff tools, debugging,
extensions and restoring the workspace across launches are not available yet.
