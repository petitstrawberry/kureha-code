# Validation — 2026-10-23

This records observed behavior, separately from build and unit-test results.

## Builds and automated checks

- macOS ARM64: `cargo test --lib --bin kureha-code` passed 26 tests (11 library,
  15 application). Coverage includes editing deltas and undo groups, independent
  tabs, stale save/clipboard completion, bounded explorer listings, literal
  search/replace, UTF-8 and CRLF, and atomic file replacement.
- Scarlet AArch64: the SDK built a native `.app` using the existing
  `aarch64-limine-full` project's SGFX configuration. Run `cargo scarlet app build`
  from that project directory; plain Cargo without its native loader configuration
  does not link the SGFX dynamic backend.
- No Windows or Linux runtime validation was performed.

## Scarlet native interaction

The actual AArch64 kernel, SWS, Scarlet shell, Files picker service and packaged
Kureha executable ran under QEMU TCG at 1280 × 800 with a 2D virtio GPU. This is
an isolated initramfs smoke environment, not a full distribution boot or GPU
performance benchmark. All documents were disposable generated fixtures.

Observed with ScarletUI revision 30945ce:

- Expanded the project explorer and opened Rust and Japanese text in separate tabs.
- Copied Japanese text through the session clipboard, pasted into a new tab,
  undid the complete paste, and redid it.
- Used the native Files Save dialog to create `kureha.txt`.
- Found `Find`, replaced it with `Edit`, saved, closed the tab, and reopened the
  saved file. The replacement and Japanese text were both retained.
- Open Folder launched Files in folder-selection mode. Selecting `/tmp` changed
  the explorer root while the already-open documents remained available.
- Closing a dirty tab opened a Save / Discard / Cancel confirmation. Cancel
  preserved the tab and edits. The initial run exposed a window-raising issue;
  confirmation creation now explicitly calls the platform focus API. A second
  native run confirmed that the dialog appears in front, Cancel retains edits,
  menu Undo clears the dirty state, and Close Tab then closes without a prompt.

Screenshots, input traces, binary hashes, boot commands and logs are preserved in
`../Scarlet/artifacts/desktop-quality-20261023/`. In particular,
`kureha-native-workbench/` contains the folder chooser, clipboard/undo/redo,
find/replace and save/reopen evidence; `native-popup-focus/` contains the final
window title, Refresh icon, raised confirmation, File-menu Open Folder and
menu Undo followed by clean tab closing. The final dependency is ScarletUI
7adae8c, which additionally synchronizes declarative menu state after rebuilding.
Its 32 focused runtime tests passed; Kureha’s 26 tests passed again at that pin.
A final native run at 7adae8c opened the shared Open Folder picker from the
Explorer button, selected `/root`, and opened Rust and Japanese text in separate
tabs. The screenshots are in `native-submenus/` (including
`kureha-native-open-folder-api.png` and `kureha-final-workbench.png`).

## macOS interaction and limits

The workbench launched on macOS and Open File selected a real fixture using
NSOpenPanel. Further GUI checks stopped when the Mac was locked. The updated
Open Folder mapping was compile-checked and policy-tested, but its final AppKit
panel has not been manually verified.

At this earlier checkpoint, ScarletUI's winit backend did not install the app's File/Edit menu
model into the macOS menu bar. Keyboard commands remain available. This early observation predates the Command
modifier support verified below. Native menu integration was added in the later workbench checkpoint below.

## Remaining scope

Extensions are not implemented. Syntax and language-service validation added later on the same date is recorded below.
Large-file rendering speed, IME composition, every platform, every close/save
error path, and tab-scroll retention under sustained editing are not claimed
verified. The editor engine integration does not by itself improve ScarletUI
TextDocument layout performance.

## Syntax, icons and language services — later validation

ScarletUI 9ddf4be adds text style spans and diagnostic underlines without changing
font metrics. Kureha now uses background syntax parsing, per-language file icons
and a dedicated packaged application icon. Actual AArch64 screenshots of Rust
coloring and the folder explorer are preserved in
`../Scarlet/artifacts/kureha-code-20261023/native-qa/`.

The final host suite passed 55 tests; two optional real-server tests are excluded
from that default run. The application integration test was also run explicitly
against rust-analyzer and passed definition navigation, accepting a completion
with an undoable edit, and pull diagnostics. macOS GUI observation confirmed
colored source, file icons and diagnostic underlines. A transient cancellation
observed while indexing no longer replaces the user's status with protocol data;
a regression test covers this behavior.

The LSP client supports stdio servers configured by the user. No language server
is bundled with Scarlet. The native build and syntax rendering were verified;
running rust-analyzer inside Scarlet was not verified and requires a compatible
server executable. The successful rust-analyzer integration test ran on macOS.
Primary-key behavior and the later native menu integration are recorded below.


## Workbench interaction refinement

ScarletUI 8fb4c671 preserves committed TextView scroll offsets across intermediate
parent measurements and suppresses pointer-hover callbacks during selection drags.
Its text-view suite passed 96 tests (four optional benchmarks ignored), with a
separate hover-drag regression also passing. The temporary-viewport regression
failed before the fix by changing vertical scroll from 933 to zero.

The published dependency is used directly by Kureha, without local Cargo patches.
The host suite passed 56 library and 26 application tests (one optional real-server
test in each binary ignored). The real rust-analyzer application test also passed
explicitly, including definition, accepting completion, and pull diagnostics.

A mounted workbench regression scrolls a 250-line document, drags a multiline
selection while updating the position status and rebuilding the tree, then opens
and closes Find. Document identity, rectangle, scroll offset and selection anchor
remain stable. On macOS, a separate disposable fixture was scrolled to line 106;
forward and reverse multiline drags retained that top line and text positions.
Find appeared at the upper right without changing editor height or scroll.
Screenshots are in Scarlet/artifacts/kureha-code-20261023.

Earlier macOS interaction in this refinement also verified Command+A, F12 to a
real Rust definition, a rust-analyzer hover popover, theme switching, and an
interactive PTY `pwd` returning the fixture’s source directory. These observations
do not establish Windows/Linux runtime support or a native Scarlet language server.


The Scarlet AArch64 `.app` was rebuilt with the same published UI pin. A native
QEMU run scrolled to line 132, selected lines 138–146, and opened the Find overlay;
the first visible line and editor height remained unchanged. The integrated PTY
launched `/bin/sh`, accepted commands, and `/bin/ls` listed the fixture’s root
folder. A service launch without PATH exposed failed command-name lookup; the
native backend now supplies `/bin:/usr/bin` only when PATH is missing or empty.
The native language status correctly reports that rust-analyzer is absent.
Evidence is in `native-refinement-final/`, separate from the macOS screenshots.

A focus regression additionally confirms that clicking the document after using
the terminal returns Find shortcuts to the editor. Closing the Find overlay
returns Command+A to the document instead of a removed search field.

A fresh native run of the PATH fallback accepted plain `ls` from the integrated
terminal and displayed the fixture directory, without setting PATH in the service
or shell. The final application suite passed 26 tests and the library suite 56.

## Two editor groups and workspace commands — 2026-10-23

The workbench now provides Quick Open, Command Palette, line/column navigation,
workspace search and buffered replacement, code-edit commands, two editor groups,
Save All, formatting, references and current-file symbol rename. Kureha references
published ScarletUI 01bc36e6326b19f118cbf4dc1e56f3bcb6786631 directly. That version
adds AppKit menus and a themed placeholder API; no sibling Cargo patch is used.

The host suites passed 93 library tests and 60 application tests. Three optional
real-server tests are excluded from that default run. The two application tests
were also run explicitly against rust-analyzer: definition, completion, diagnostics,
formatting and single-file rename with one-step undo, unopened reference navigation,
and atomic rejection of cross-file rename all passed. Code paths for unavailable
capabilities and stale responses are covered separately.

Review found and corrected split-view lifecycle errors: closing background tabs
must preserve each group's active document; a delayed file read must target its
originating group without taking focus; disjoint edits and undo/redo must transform
the other view's selection through the engine's real delta. Regressions cover
paste, multiline indentation, undo/redo and closing either group's last tab.
A save-conflict regression also covers canonical path aliases on macOS.

Actual macOS interaction verified the native View menu, Split Editor Down, Quick
Open input with Command+A, and independent group views. Further macOS operations
stopped when the screen locked. This is not a claim that every latest operation
was checked on macOS. The native menu backend's separate focus/shortcut/Quit-veto
probe is recorded in ScarletUI/artifacts/native-menu-20261023/verification.txt.

Actual Scarlet AArch64 QEMU interaction verified Quick Open, right split, independent
line navigation (left at line 1, right at line 145), theme selection, workspace
search (113 matches in three fixture files), navigation to an unopened result,
returning Select All to the editor, and buffered replacement across three files.
A stale search snapshot was rejected without applying changes. These runs used
real fixture files and the packaged `.app`, not a host preview. The native status
correctly reports the missing rust-analyzer executable; native LSP execution is
not claimed. Native evidence and build/test logs are in
Scarlet/artifacts/kureha-workbench-20261023/.

Remaining product scope includes more than two/nested groups, multi-cursor editing,
code folding, cross-file rename, Git/diff and debugging integration, extension
hosting and workspace restoration. The current result is not full VS Code parity.

The reviewed native package also saved all three changed files through File →
Save All, and saved an undo through Control+Alt+S while the editor was focused.
A subsequent native check exposed focus remaining on the disabled workspace
Replace All button. The fix returns focus after replacement and explicit editor
or tab activation; a mounted interaction regression clicks Replace All and then
verifies that Save All reaches the filesystem through its keyboard shortcut.

Final native focus verification at 547bbbb confirmed replacement followed
immediately by Control+Alt+S, Command Palette theme changes, both split directions,
and closing/reopening the saved README with its replaced contents intact. Evidence
is in Scarlet/artifacts/kureha-workbench-20261023/native-focus-final/.
Rapid typing after Quick Open exposed one remaining queued-input race: the first
character could reach the previous editor before the overlay mounted. An editor
capture guard now routes that queued text into the navigation query. The mounted
regression fails before this fix and passes after it, without changing the document.


## Terminal cursor and grid rendering — 2026-10-24

The terminal now rebuilds its own cursor/focus view when those states change,
without waiting for an unrelated workbench update. Pending-wrap cursor positions
are clamped to the visible grid and wide continuations use the leading cell.
Dim ANSI text is preserved. Cell dimensions and PTY sizing use the same editor
font metrics. The published ScarletUI revision `fd587a29` honors the explicit
font stack in retained painting as well as CPU rendering and uses the font line
height for vertical placement.

Thirteen terminal tests passed, including real PTY input/output, resize, exit,
cancellation, and a mounted rendering-pipeline regression for cursor-only
movement/hide/show and focus changes. The TextGrid regression compares CPU and
retained font rendering, including bold cells.

Actual macOS PTY interaction used `terminal_probe.py` in
`../Scarlet/artifacts/kureha-terminal-20261024/`. It displayed narrow/wide Latin
characters, Japanese, ANSI colors/bold/dim and box-drawing lines, then changed
only ANSI cursor position/visibility while leaving the text unchanged. The
corresponding `terminal-rendering.png` and `cursor-{moved,hidden,shown}.png`
captures confirm the immediate visible updates. These are host UI observations,
not a Scarlet-native UI performance measurement. TextGrid still represents a
cell with one scalar, so combining-mark/emoji shaping is not claimed complete.

## Flat explorer and command-palette rows — 2026-10-24

Explorer and navigation candidates use text/icon rows, with selection and hover
color spanning the entire row. There are no nested filename/candidate Buttons.
Pointer release must complete inside the row; keyboard activation and focus
survive a workbench rebuild. Input state is retained by stable row key and
pruned when the corresponding model changes.

The binary test suite passed 62 tests (2 existing ignored cases), including
full-width label/detail/blank-area activation, outside-release cancellation,
retained keyboard focus, navigation and editor-focus regressions. Actual macOS
interaction opened `backend.rs` from the explorer row's trailing blank area and
activated Quick Open from the right edge of its command-palette row. Arrow-key
selection used the same full-row highlight. Captures are `sidebar-flat.png` and
`palette-flat.png` in `../Scarlet/artifacts/kureha-terminal-20261024/`.

This Finder-launched host verification instance could not locate rust-analyzer
on its inherited PATH. That is distinct from the AArch64 full distribution's
`/bin/rust-analyzer`; host LSP success is not claimed for this GUI run.

## Stable line-number gutter — 2026-10-24

ScarletUI `4cbad1e9` reserves four widest-digit cells before editing begins.
The text origin stays fixed through 9/10, 99/100 and 999/1000 lines and while
deleting back across those boundaries. Documents over 9999 lines can expand
the gutter to fit their line numbers. Both Kureha editor groups use this layout.

The TextView suite passed 93 tests (4 existing ignored cases), including a
regression that failed at 10 lines before the fix and now checks text/caret
coordinates, viewport width and hit testing up to 9999 lines, plus expansion
at 10000. Kureha's binary suite passed 62 tests (2 existing ignored cases).
Actual macOS editing from 9 to 10 lines preserved the text's horizontal origin;
captures are `9-lines.png` and `10-lines.png` in
`../Scarlet/artifacts/kureha-gutter-20261024/`. The fixture was opened via the
startup file argument after native file-dialog automation did not complete.
This observation is host UI verification, not a Scarlet-native UI run.

## Custom themes and hover responsiveness — 2026-11-09

The theme settings worker loads TOML palettes and editor fonts outside the UI
thread. Six bundled palette variants supplement Light/Dark; editor colors and
font settings can differ from the surrounding workbench. Invalid and atomic
saves, independent editor overrides and stale syntax-job rejection are covered.
Settings examples and palette attribution are in `assets/theme.toml` and
`assets/themes/README.md`.

The initial hung-process sample was dominated by layout text measurement and
parsed font-table cloning. Hover wrapping now searches UTF-8 boundaries, viewport
and body share measured layouts, and code blocks use one asynchronous highlight
request per hover (avoiding eviction churn after eight code blocks). ScarletUI
PR #89 shares font faces and adds owned/installed-family font stacks.

Validation on macOS AArch64:

- `cargo test --lib --bin kureha-code`: 176 passed, 4 optional/manual cases ignored.
- Pinned dependency `cargo check` and `cargo build --release --locked --offline` passed.
  Both full-workbench hover tests also passed in release mode: initial layout
  plus Escape took about 269 ms; six CPU-painted scroll frames took about 390 ms.
- ScarletUI graphics tests: 20 passed, including owned font lifetime, fallback
  and cache-identity coverage.
- A 28-code-block hover test finishes highlighting and reuses measured layouts.
  Full-workbench tests exercise Escape and scroll down/up inside the hover,
  checking hover content movement and subsequent dismissal with painting enabled.
- The manual preview generator produced 17 CPU-rendered fixtures: ten existing
  Light/Dark scenes, six bundled palettes and a light workbench with a dark
  editor and 18 px font. Representative fixtures are `screenshots/theme-dracula.png`
  and `screenshots/theme-editor-only.png`.
- Native winit/WGPU verification loaded Menlo at 18 px only in the editor,
  opened rust-analyzer documentation for `std::fmt::Debug`, scrolled down through
  multiple pages and back up, dismissed with Escape and moved the editor caret.

The user also reported a hang specifically while scrolling inside hover text.
A later sample of the user's process was idle; it did not capture that hang.
The native reproduction above remained responsive. The CPU debug integration
fixture spends roughly 1.1 seconds per painted scroll frame, mostly in backdrop
painting; this is not a native GPU frame-time measurement. These checks do not
establish that every reported hang has been eliminated. Scarlet-native runtime
verification is still outstanding.

## Hover-visible document scrolling — 2026-11-09

The clarified trigger is scrolling the editor document while a hover is visible.
This differs from scrolling inside the hover. A full-workbench regression
reproduced lost wheel input: the hit target below the popup was its full-viewport
`OnEventRenderObject`, not the document TextView. The popup's surrounding ZStack
also claimed the transparent area after that event wrapper was moved.

The keep-open event handler now wraps the actual panel inside Anchored, and
mutually exclusive information/rename panels use Either rather than a full-size
ZStack. Pointer input outside the panel reaches TextView. A document wheel event
also dismisses the old hover, clears its delayed target and cancels outstanding
hover requests, including explicit Show Hover requests without a pointer
generation. Diagnostics and other panels are left intact. Scrolling inside the
hover still scrolls its contents and keeps it open.

Validation: all 178 Kureha tests passed (four optional/manual cases ignored),
including hit-target selection, document scroll and selection preservation,
continued scrolling after a rebuild, late manual-hover response rejection and
hover-internal scroll retention. The new document-scroll regression failed
before the fix because TextView never received the event. Locked/offline release
build passed. Native winit/WGPU launched, but CUA could not acquire its window
(`cgWindowNotFound`); this follow-up's native interaction is not claimed verified.
No new ScarletUI change was needed for this fix.

## Navigation, file icons and minimap dragging — 2026-11-09

The tab strip now exposes Back/Forward. Explicit tab, definition, project-search
and line navigation remember the originating document/group, selection and
scroll position. History is session-only, bounded to 128 entries, prunes closed
documents and discards the forward branch on a new jump. Restoring an edited
buffer clamps saved byte positions to UTF-8 boundaries. Project Replace All's
internal tab activations do not add navigation entries.

File/folder icons are 18 px, folders have stronger filled/bold silhouettes, and
Rust/Python/JavaScript/TypeScript/Go/HTML/CSS use distinct language marks.
Colors adjust against the theme's normal and selected backgrounds. All bundled
palettes pass the icon contrast checks. ScarletUI #90 exposes seven already
bundled Tabler glyphs; Cargo pins its published commit
`4a24445bb01e1e6c6e2ee1439e6c05f3ad6a7ed5` without a local path patch.

Minimap drag state now belongs to its retained renderer. Grabbing the viewport
keeps the grab offset, leaving the bounds does not cancel dragging, and moving
uses the full scrollable range even for sampled long files. Release/cancellation
ends the drag, including outside the minimap. Rebuilding the workbench retains
the gesture; switching to another document clears it.

Validation on macOS AArch64:

- All 183 Kureha tests passed; four optional/manual tests ignored. History tests
  cover painted viewport restoration, back/forward branching, split groups,
  closed documents and changed UTF-8 text. Project-search navigation also checks
  back/forward restoration.
- The mounted minimap regression covers 100-line and 4,000-line documents,
  off-center grabs, rebuilds during dragging, horizontal escape, both scroll
  limits and release outside. The long-file case passed separately after the
  full suite.
- All seven ScarletUI icon tests passed, including every selected glyph's
  rasterization and filled-outline fallback. Locked/offline release build passed.
- Eighteen CPU-painted fixtures rendered successfully; Light and Monokai images
  were inspected and preserved in `screenshots/navigation-light.png` and
  `screenshots/navigation-monokai.png`.
- Native winit/WGPU launched and rendered the new tab controls and Rust mark.
  CUA's drag command timed out; a subsequent process sample was idle in the
  AppKit event loop. Native drag/back-button interaction is not claimed verified.
  Scarlet-native runtime testing remains outstanding.

The minimap source marks subsequently changed from 80% to 40% opacity (also
respecting any alpha in the syntax color). The viewport indicator remains at
15% neutral fill. All 12 minimap tests and all 18 rendered fixtures passed again;
Light/Monokai screenshots were refreshed after visual review.


## Image previews, tab dragging and WebView design — 2026-11-09

Raster images open in read-only tabs using bounded worker decoding. Fit and zoom
share an inset, clipped surface; a compact toolbar remains inside a 180px pane.
The checkerboard and enlarged image cannot paint across the pane rim. Image tabs
keep dimensions, format and size in the status line and cannot be saved as text.

Tab dragging reorders the strip, splits an unsplit editor at any edge, and moves
between the existing two panes. Empty panes collapse. Document identity, dirty
buffers and Undo remain shared; selection, scroll and image zoom follow a move.
Hit regions are collected from the live element tree before pointer dispatch,
including incrementally laid-out and scrolled children. They do not depend on
paint origins: native retained GPU layers can paint in local coordinates.

Validation on macOS AArch64 with the published ScarletUI pin:

- Locked/offline full suite: 108 library and 80 application tests passed;
  five optional/manual tests ignored. Release build passed.
- Mounted pointer tests cover splitting, moving, rebuilding during a gesture,
  outside release and Escape. A local-origin paint regression protects native
  GPU hit regions. Model tests cover dirty buffers, Undo, selection, scroll and
  empty-pane promotion.
- Worker tests verify image alpha, read-only editing/saving, file byte retention,
  pane moves and zoom cleanup. Pixel tests at 180px and 260px widths cover Fit,
  100% and 800% zoom, toolbar bounds and the unpainted outer inset.
- The refreshed CPU fixture was inspected and saved as
  `screenshots/image-preview.png`.
- Native winit/WGPU interaction confirmed image Fit/100%, dragging the image tab
  to the bottom edge to split, moving it back to the upper pane, and automatic
  collapse of the empty lower pane. The clipped preview was visually inspected.
  Scarlet-native and other desktop backends remain untested.

`webview.md` is a design, not an implemented HTML/Markdown preview. It describes
extracting Myrica's reusable session/engine layer and supporting both frame and
platform-native child-view presentation. No Myrica working files were changed.


## Stable Quick Open and live theme selection — 2026-11-09

Quick Open previously cleared its rows on every query edit before its worker
finished. This collapsed the popup between keystrokes; commands were filtered
synchronously and did not show the intermediate empty frame. File queries now
retain the previous rows and selection until the latest generation publishes.
Enter/click acceptance still requires the displayed query to match the current
query. Root/mode changes and terminal errors clear irrelevant rows, and unchanged
loading hints no longer trigger repeated state notifications.

The command palette now exposes Select Color Theme and Select Editor Color
Theme as parent actions, with searchable candidate lists beneath them. Keyboard
selection and pointer hover preview immediately. Enter/click commits; Escape,
returning to Commands or switching navigation restores the original palette.
A settings reload supersedes an active preview. Editor-only choices preserve
workbench colors and custom fonts. View menu and Primary+K Primary+T open the
same picker. Previewing does not write custom settings.

The startup default is now Kureha: pale pink glass, rose selection/accent and a
near-white editor with coordinated syntax colors. Light retains the previous
blue palette. Existing custom theme files still override the default; only the
bundled template for newly created settings changed.

Validation on macOS AArch64:

- 192 tests passed (108 library, 84 application); five optional/manual tests
  ignored. Locked/offline release build passed.
- The deterministic pending-worker regression failed before the Quick Open fix
  and passed afterward. It covers repeated typing, retained rows and text-field
  identity, stale acceptance rejection and eventual newest-query publication.
- Theme tests cover nested entry, live filtering, commit/cancel, parent return,
  settings replacement, editor-only colors/fonts and pointer preview without
  losing query focus. Bundled-palette icon contrast tests include Kureha.
- Twenty-two CPU fixtures rendered successfully. Kureha workbench and chooser
  were inspected and preserved in `screenshots/kureha-theme.png` and
  `screenshots/theme-picker.png`.
- Native macOS WGPU confirmed Cmd+Shift+P → Select Color Theme, keyboard preview
  of Light/Dark, Escape restoration to Kureha, and continued Cmd+P typing with
  updated results and stable populated-panel height. Other platforms are not
  verified by this pass.


## Utility panel, nested splits and macOS chrome — 2026-11-09

The utility panel is separate from editor documents and the split tree. Its
View menu command is Toggle Panel. Cmd+J/Control+backtick toggles visibility regardless
of keyboard focus; the panel's rightmost × hides it without terminating shells.
Each session's × closes just that PTY. The panel strip contains session names,
+ and ×, with no heading, history buttons, path/status footer or empty footer row.
Normal editor strips have no terminal-add button. Multiple sessions retain their
own output, and hidden/background sessions continue polling. Paste requests are
bound to session ID, generation and focus epoch.

The panel starts below the entire editor tree. Dragging a session to the right
or bottom docking edge relocates the whole utility panel; file tabs do not enter
it. The editor tree separately supports nested horizontal/vertical splits and
three columns. A tab dropped in the outermost 16 logical pixels of the editor
area targets the entire tree, while inner edge zones target the individual pane.
Both targets have distinct area previews. Palette commands select all four outer
edges explicitly. Stable pane identities prevent delayed file opens from entering
a newly created replacement pane. Closing a pane retains documents and the
neighbour's existing active tab and viewport.

Validation:

- Host suite: 108 library + 91 application tests passed; seven manual/optional
  tests are excluded from the default run. Release build passed with locked,
  offline dependencies in the Nix development shell.
- Real PTY regression verifies two independent shell variables, hidden output,
  hide/show and selection clipboard invalidation, and closing only the target PTY.
- Mounted pointer/layout regressions cover panel docking independent of nested
  editors, all four whole-editor drop edges versus local edges, minimum-size
  nested layouts, and preserving selection/scroll/text/undo when moving tabs.
- Five CPU-rendered layout fixtures passed. Reviewed bottom/right docking,
  hidden/restored panel, and a full-height right editor beside a vertical stack.
  Representative images are panel-bottom.png, panel-right.png and
  editor-outer-right.png in docs/screenshots.
- Native macOS release review confirmed the compact panel, its visibility toggle,
  separate editor tabs without +, and simultaneous horizontal/vertical editor
  splits above the panel. The native accessibility tree retained close,
  minimize and fullscreen controls with the unified titlebar.

ScarletUI is pinned to published commit 1e0c15585dda178b8e70594ed850d9148da2918d
([upstream PR #91](https://github.com/petitstrawberry/scarlet-ui/pull/91)). Its
SYSTEM_OVERLAY mode preserves native macOS controls over full-size content;
20 core window tests and 38 winit tests passed. Other hosts use normal decoration.
The separately reported VT100 panic was explicitly excluded from this task.

## Built-in Markdown preview — 2026-11-09

Markdown now has a native ScarletUI preview alongside the existing image viewer.
Toggle Markdown Preview (Primary+Shift+V), Open Markdown Preview to Side, and
Markdown: Refresh Preview are available through the menu or command palette.
Source and preview share a document, with mode owned by the pane. Markdown
parsing/highlighting and image I/O run on separate workers; stale text revisions
and image results from a previous file path are rejected. Layout is retained for
unchanged content/theme/font/width. Hidden previews stop scheduling work.

Images are embedded with preserved aspect ratio and alpha. Relative local paths,
URI-escaped Unicode names, cache reuse/invalidation, HTTP image responses, and
failure placeholders are covered. PNG/JPEG/GIF-first-frame/WebP/BMP/ICO are
supported; SVG and HTML/CSS layout remain outside this implementation. HTTP/HTTPS
images are supported on the desktop host builds; Scarlet currently uses local
images. Inline emphasis uses the theme accent; preview text selection is not yet
implemented. Limits and refresh behavior are documented in README.md.

Validation on macOS through the locked Nix development environment:

- `cargo test --locked --offline -- --test-threads=1`: 113 library and 98 binary
  tests passed; eight optional/manual tests were ignored.
- `cargo build --release --locked --offline`: passed.
- A deliberately blocked local HTTP image response did not prevent a newer
  unsaved text revision from reaching the preview before the response was released.
- Source/preview separation, image reuse after edits, Undo, theme changes, pane
  moves, closing documents, rapid revisions, and hidden/reopened views passed.
- Actual renderer tests covered Unicode, long unbroken text, tables and embedded
  images at 180/320/840px widths, retained geometry, wheel scrolling and heading
  link targets. An initially quadratic long-word wrapping path was replaced with
  bounded glyph-advance measurement; the 20,000-character regression passed.
- The manual `markdown_preview_screenshot` fixture rendered source and preview at
  1440×1060. The resulting [screenshot](screenshots/markdown-preview.png) was
  visually reviewed for headings, nested content, code colors, tables and the
  embedded image. This was a ScarletUI CPU renderer review, not a native window
  interaction pass.


## Independent previews and bounded painting — 2026-11-09

Historical checkpoint; the dedicated-pane UI and native measurement limitation
below are superseded by the ordinary-tab checkpoint that follows.

The workspace now starts with no document. Closing the last document leaves an
empty editor; creating an untitled document is explicit. Markdown previews are
independent panes opened beside their associated editor, rather than a tab mode.
They follow unsaved text and optionally follow the active Markdown file and
scroll position. Closing a preview preserves its source. File opens, tab moves,
and editor-group closure cannot turn a preview into an ordinary editor tab.

Scroll synchronization uses source-line/block geometry in both directions, even
without clicking to transfer focus. A queued reveal is clamped when a document
shrinks, and programmatic movement is not echoed back. Within a block, source
positions are interpolated rather than mapped character by character.

ScarletUI is pinned to published revision 88d8873b217460d381995e625d2003fb87c5fc1e
([PR #92](https://github.com/petitstrawberry/scarlet-ui/pull/92)). Custom render
objects receive viewport changes and can invalidate their own paint without
requesting layout on every scroll. The upstream core suite passed 703 tests,
with five optional tests ignored; a mounted regression verifies new scrolled
pixels without another content layout.

Kureha emits paint commands only for visible Markdown runs, panels and images.
Unchanged paragraphs keep their measured runs after edits and image completion.
Two retained width layouts avoid remeasuring text when split containers probe a
provisional width. Glyph advances are shared across paragraphs during layout;
decoded image buffers remain shared with the backend.

Validation in the locked Nix environment on macOS ARM64:

- Full suite: 113 library + 102 application tests passed; eight ignored.
  PTY validation used `SHELL=/bin/sh` for a predictable interactive shell.
- After the final geometry/measurement changes, all ten focused Markdown tests
  passed again (one screenshot fixture ignored).
- Mounted regressions cover bidirectional wheel synchronization, no feedback
  loop, disabled synchronization, shrinking documents with a queued reveal,
  following/pinning files, source activation from a preview, closing either
  document or preview, and an entirely empty workspace.
- A 1,000-paragraph regression generates fewer than 200 paint commands for a
  400px viewport at three scroll positions, over 50 times fewer than painting
  the full fixture. Scrolling preserves geometry, unchanged paragraphs preserve
  their measured buffers, and provisional width changes reuse the prior layout.
  These are command-count/cache checks, not frame-rate measurements.
- The release build passed. A separate native process was launched with
  `SCARLET_UI_WINIT_RENDERER=sgfx`, which disables CPU fallback. It reported
  `[ScarletUI] platform-winit renderer=sgfx backend=wgpu`.
- Native input/scrolling could not be verified: computer-use window lookup
  returned `cgWindowNotFound`. GPU initialization is verified; GPU frame rates
  and native preview interactions are not claimed measured.
- The updated [two-pane screenshot](screenshots/markdown-preview.png) was rendered
  by ScarletUI's CPU validation pipeline and visually reviewed, including the
  independent preview controls and embedded image. It is not a GPU screenshot.

No new Scarlet, Windows or Linux runtime verification was performed.


## Ordinary preview tabs, source scrolling and code themes — 2026-11-09

Markdown previews now use the ordinary editor tab strip and split tree. A preview
has its own tab ID and refers to the source document, including its unsaved text
and Undo history. It can be reordered, split, moved between groups and closed like
other tabs. Closing either source or preview keeps the other alive; closing the
last view of a dirty document offers the usual save confirmation. The dedicated
preview header/panel has been removed. File following and optional bidirectional
scroll sync are controlled from View or the command palette.

The remaining source-scroll slowdown was traced to workbench invalidation on
both requesting and acknowledging the preview's scroll position. Requests now
update a State-bound range on ScrollView, and acknowledgement updates only local
bookkeeping. No workbench rebuild is needed for either step. ScarletUI revision
`34c2c08057f7be7579b9975c927e3aa13f9cfa3d` adds that binding and subscription support
for containers that own view state ([PR #92](https://github.com/petitstrawberry/scarlet-ui/pull/92)).

A native macOS release process forced to sgfx/wgpu measured the final 29 source
scroll updates in a 600-line Markdown fixture. Median frame work fell from
43.378 ms to 3.357 ms; median CPU dirty-tree/layout work fell from 40.435 ms to
0.049 ms. macOS configured slightly different window widths (736px before,
720px after, both 900px high), so these are diagnostic measurements, not a
controlled same-viewport benchmark or an FPS claim. See the
[method and raw logs](performance/markdown-scroll-20261109/measurement.md).
The driver changed source scroll bindings through the real scene loop; mounted
renderer tests separately exercise actual wheel events.

Terminal defaults, cursor and 16 ANSI slots now follow the selected palette,
including already-rendered output and hidden sessions. **Select Code Color Theme**
changes the editor and terminal together while retaining the workbench palette.
The custom settings section is `[code]`; there is no `[editor]` compatibility
alias. `[terminal]` permits explicit overrides. RGB and extended indexed colors
requested by terminal applications keep their original values. The terminal
retains semantic color indices so changing a theme cannot accidentally recolor
an explicit RGB value that happened to equal an old palette entry.

Validation in the locked Nix environment on macOS ARM64:

- Full suite: 115 library + 103 application tests passed, 10 optional/manual
  tests ignored. PTY tests used `SHELL=/bin/sh`.
- After removing the temporary configuration alias and renaming the picker,
  the focused theme tests passed again.
- Upstream ScarletUI core: 704 passed, five ignored. Bound reveal changes move
  retained content without re-laying out children; manual wheel scrolling still
  works after a reveal, and unchanged state schedules no extra frame.
- Mounted preview tests cover independent tab identity, dirty document lifetime,
  moving/splitting, following/pinning files, bidirectional sync, disabled sync,
  shrinking documents, no workbench invalidation and no Markdown relayout on
  source scrolling. Thirty idle cycles produce no layout or presentation.
- Terminal tests cover immediate recoloring without fresh PTY output, pending
  output across a theme change, cursor/focus repaint, unchanged session generation,
  inverse/bold/underline preservation, explicit RGB, extended indexed colors,
  and returning to the original palette without stale pixels.
- [Markdown tabs](screenshots/markdown-preview.png) and
  [terminal palettes](screenshots/terminal-themes.png) were rendered with the CPU
  validation pipeline and visually reviewed. They are not native GPU captures.

No new Scarlet, Windows or Linux runtime verification was performed.


## Incremental syntax and local typing updates — 2026-11-10

Every text revision previously cleared syntax colors while a whole-document
parse ran. The edit callback and parser completions also rebuilt the workbench.
Syntax now retains remapped colors on unchanged UTF-8 text while a worker reparses
from saved line checkpoints until its state converges. Editor and Markdown
components subscribe to their own document/decoration updates, so ordinary input
and async completions do not rebuild workbench chrome. A clean-to-dirty transition
still updates the tab's dirty indicator.

Validation: 118 library + 104 application tests passed, 10 optional/manual tests
ignored. Tests compare incremental and full colors through local edits, inserted
Unicode lines, Undo, and removed multiline delimiters across four languages.
A mounted `TextInputCommit` regression preserves preview layout until its model
actually changes, reuses unchanged paragraph measurements, and checks that input,
syntax publication and preview publication leave the chrome revision unchanged.

The ordinary release build passed after removing the temporary native probe.
Native sgfx/wgpu typing checks covered source-only and preview-visible windows.
The [method, timings and logs](performance/typing-20261110/measurement.md) record
both the warm medians and the remaining maximum-frame spike; no claim of measured
keyboard-to-display latency or universal worst-case performance is made.


## Terminal Tab input and editor groups — 2026-11-10

Tab and Shift+Tab now reach the terminal input queue through ScarletUI's
opt-in `Focusable::tab_input` policy. The bottom panel remains fixed; individual
sessions can move into existing editor tab strips, local splits or outer splits,
and back into the panel without restarting their PTYs.

- Kureha: 119 library + 106 application tests passed; 10 optional/manual tests ignored.
- ScarletUI core at `8e8f4a3e`: 705 passed; five ignored.
- Mounted event tests verify Tab/Shift+Tab focus and PTY queue bytes, plus input
  before a panel-to-editor drag, after that drag and after returning to the panel.
- Real PTY tests preserve shell variables across moves, keep hidden sessions alive,
  isolate terminal closes and restore the correct focus after panel/palette use.
  Splitting an editor terminal starts an independent shell; closing its group
  moves the tabs to the neighboring group without ending the sessions.
- [Editor terminal alongside the bottom panel](screenshots/terminal-editor-tabs.png)
  was rendered and visually reviewed with ScarletUI's CPU validation pipeline.
  Panel-hidden and restored layouts were also rendered; this is not a native GPU capture.
- The locked, offline macOS release build passed. No new Linux, Windows or Scarlet runtime check was performed.


## Terminal IME — 2026-11-10

Terminal focus now exposes a text-input context and caret rectangle through
ScarletUI. Composition renders locally with a selection and underline; only
committed UTF-8 reaches the PTY. Candidate positioning follows the cursor and
viewport, including long compositions. Blur, cancellation and session close
clear the composition without sending it to the shell.

- 120 library + 106 application tests passed; 10 optional/manual tests ignored.
- Mounted tests verify input-context registration, Japanese preedit rendering,
  suppression of conversion keys, exact committed bytes, cancellation, focus
  changes, viewport clamping and absence of idle frame invalidation.
- The locked, offline macOS release build passed.
- The user confirmed that terminal IME works with the updated build.


## Clipboard and context menus — 2026-11-10

Kureha uses ScarletUI's native clipboard providers and standard text-widget
context menus. Code-view commands delegate to the existing application edit
transactions through `TextView::on_edit_action`, preserving Undo and delayed-cut
validation. Search fields use ScarletUI's standard editing path. Terminal views
in either location expose Paste and Copy Visible Output through the shared menu
API; copied text comes from the parser so wide and combining characters survive.

- Kureha: 120 library + 109 application tests passed; 10 optional/manual tests ignored.
- ScarletUI at `2b5d33ad`: 724 core tests passed; five ignored.
- Mounted right-click tests cover the clicked split, selection preservation,
  delayed cuts, failed writes, Undo, and search-field pastes leaving code untouched.
- Real PTY tests copy Japanese and combining text from the clicked terminal
  and paste into a different panel session while the editor terminal has focus.
- No new native OS clipboard or Linux/Windows runtime validation was performed.
- The locked, offline macOS release build passed.


## Explorer scroll and context-menu stability — 2026-11-10

- Updated both platform pins to ScarletUI `0419dbec` (PR #96, based on #95).
  Bounded HStack/VStack layout defers explicit fill children until sibling space
  has been allocated, preventing provisional viewport sizes from clamping scroll.
- The mounted Explorer regression now resizes the window, scrolls near the end
  of a shorter listing, rebuilds the workbench, clicks a visible row, and waits
  for the file to open. It checks row hit coordinates, stable scroll position,
  retained sidebar width and divider release/blur behavior. The previous pin
  failed with an offset change from -245 to -225; the new pin passes.
- Context menus consume wheel packets without closing, and survive repeated
  notifications of the same window size. Changed size, focus loss, Escape and
  outside clicks still dismiss them. Core tests cover zero/nonzero wheel packets,
  all trackpad phases, resize, redraw and existing menu activation behavior.
- Kureha: 120 library + 109 application tests passed; 10 optional/manual tests ignored.
- ScarletUI: 727 core tests passed, five ignored; all eight list tests passed
  after adding the horizontal-layout/real-resize regression (728 unique passing tests).
- These are mounted pipeline tests. Native macOS trackpad replay was not verified.
- The locked, offline macOS release build passed.


## Captured text selection and newline highlights — 2026-11-10

- Both platform pins use ScarletUI `dd466ced` (PR #97). Selection drags keep
  their original TextView outside the viewport and use time-based edge scrolling
  while the pointer is held still. Release, cancellation, blur and document edges
  stop motion. No text-layout recomputation or extra rendering backend is added.
- The mounted Kureha regression crosses the sidebar and neighboring editor,
  extends the original selection, autoscrolls and releases outside. The other
  document and focused group remain unchanged. It fails on the previous pin at
  the left-edge selection assertion and passes with the new dependency.
- ScarletUI's full core suite passed: 730 tests, five ignored. All three focused
  selection tests passed after adding EOF/cancellation coverage (731 unique
  passing tests), including soft wrap, grapheme boundaries and layout reuse.
- Selected LF/CRLF endings receive a space-width highlight, including blank
  lines. Soft-wrap boundaries and EOF do not receive newline markers.
- [Kureha selection](screenshots/selection-kureha.png) and
  [Monokai selection](screenshots/selection-monokai.png) were rendered with the
  mounted CPU pipeline and visually reviewed. These are not native GPU captures;
  native OS pointer capture was not re-verified.
- Kureha: 120 library + 110 application tests passed; 11 optional/manual tests ignored.
  The selection-highlight preview also passed when run explicitly.

The locked, offline macOS release build passed.


## Appcoat packaging (2026-11-10)

- Appcoat is pinned at `3ddff8882449d82ce4c8829d538b0157b5ff0096` in the development
  flake. App identity now comes from `appcoat.toml`; mismatched Cargo/package
  versions fail the build. The Scarlet bundle consumes a prebuilt native app.
- `python3 scripts/package.py` built the release executable and packaged an
  icon-bearing macOS app. `plutil -lint` and `codesign --verify --deep --strict`
  passed. Native application launch displayed the Kureha workbench.
- The Nix toolchain's Apple libiconv reference is explicitly mapped to
  `/usr/lib/libiconv.2.dylib`. Both declare ABI compatibility version 7, and the
  host SDK provides its symbols. This avoids bundling a library whose conversion
  tables and plugins still refer to its Nix prefix. Final Mach-O dependencies
  contain only OS paths, with no Nix store load commands.
- All 230 editor tests passed (11 intentionally ignored), plus 4 packaging tests
  covering Cargo JSON discovery, failed builds, paths with spaces, Unicode argv,
  metadata/icons, existing-output protection and target validation.
- Appcoat's own checks passed 15 Rust tests and 7 integration tests, including
  transitive dylib relocation after deleting original inputs and signature checks.
- Linux runtime and Scarlet boot were not exercised on this Mac. An attempted
  AArch64 Scarlet build reached linking but the existing sgfx loader dependencies
  require unresolved `dlopen`/`dlsym`/`dlerror` symbols. No broken package was
  published; the documented Scarlet path accepts a separately built artifact.
- Developer ID signing, notarization and macOS document-open event handling are
  outside this integration. The generated app uses local ad-hoc signing.

## Settings and acknowledgements — 2026-11-10

Settings is an ordinary workbench tab with the existing tab identity and drag/split
paths. It owns no text document and closing it does not create an Untitled file.
The existing ScarletUI `MenuItemModel::app()` adds Settings to the macOS application menu. Acknowledgements is a section
inside Settings, without a separate application-menu item. Open Settings File remains in Settings and the
command palette. Settings sections use ScarletUI `TabView`; acknowledgements
uses an inline searchable `ListView` beside the notice text.

Confirmed color/code themes, code font size and minimap visibility are saved in
`settings.toml`, next to `theme.toml`. Writes run on the theme worker, preserve TOML
comments, follow existing symlinks and replace files atomically. Invalid settings
are retained with an error. Shutdown drains queued saves. Live theme preview and
cancel do not write settings; delayed save acknowledgements wait for a preview to
close and cannot overwrite a newer choice.

Generate the checked-in offline license catalogue after updating dependencies:

```sh
nix develop --command python3 scripts/generate-acknowledgements.py
python3 -m unittest discover -s tests -p 'test_acknowledgements.py'
```

`cargo-about` is supplied by the pinned flake. It resolves dependency licenses with
`generate --locked --fail --format json`; `about.toml` records accepted licenses
and checksum-verified repository LICENSE clarifications for Scarlet workspace
crates whose manifests omit license metadata. The small adapter only reshapes
that report and adds the bundled font/theme notices. No custom license discovery
or download logic remains. The catalogue is embedded in the executable and copied
into packaged resources, so reading it does not require a network connection.

Regressions cover settings-tab reuse/moves/splits/closing without documents,
read-only notices (including IME), theme preview/confirmation, persistence across
restart, malformed input, atomic external edits, symlinks and shutdown flushing.
CPU previews of Kureha/Monokai settings and notices are in `artifacts/settings/`.

The final suite passed 242 Rust tests (12 optional tests ignored), four packaging
tests and the catalogue check. macOS SGFX/WGPU validation used a disposable config:
Settings opened in the main window from the application menu, Monokai selection
was saved, and a fresh packaged launch restored it. Cmd+, opened Settings from an
empty workbench and Cmd+W closed only that tab. Acknowledgements search and Enter
selection displayed the matching notice. The workbench now retains a focusable
ancestor so shortcuts work even when no text editor is focused. Windows/Linux/
Scarlet runtime behavior of this settings change was not exercised.

The TabView/inline-list follow-up passed 12 focused settings tests and renderer
previews in Kureha/Monokai. Native macOS interaction verified section switching,
inline search with Enter to select a notice, and the simplified application menu.

The bundled-asset follow-up adds a dedicated Gruvbox notice with pinned upstream
MIT declarations and author attribution. Upstream supplies no standalone LICENSE
or copyright notice; the notice explicitly identifies the standard permission and
warranty terms supplied from SPDX rather than presenting them as an upstream file.
All six Python tests passed, including complete theme/font notice coverage, Tabler
copyright attribution, and notice/catalogue inclusion in an Appcoat package. The
release macOS package passed signature verification; its notices match the source
files and the executable embeds the Gruvbox attribution and provenance.


## Shared tab pane framing — 2026-11-10

ScarletUI `TabPane::new(bar, content)` now owns the editor groups and panel frame.
It composes the shared Surface renderer with separately clipped bar/content slots,
then paints the foreground border after both. Kureha supplies theme colors and
slots rather than assembling the outer radius, clipping and border itself.
The settings TabView has 16 logical pixels of space above it.

ScarletUI's 23 TabView/TabPane tests and API doctest passed. The new pixel regression
covers opaque slot backgrounds, continuous outer edges, rounded clipping and
stable child identities through resize/rebuild. Kureha's 122 binary tests passed
(10 optional tests ignored), including tab splitting/moving, terminal/panel and
settings behavior. Light/dark settings previews are in `artifacts/settings-tab-pane/`.

The packaged macOS app was also checked with the native SGFX/WGPU backend:
Appearance/Acknowledgements switched within the shared frame, the top spacing was
visible, and the application menu contained Settings without an Acknowledgements
item. All six Python catalogue/packaging tests and package signature verification
passed. The packaged build is `dist/tab-pane/Kureha Code.app`.


## Control label baselines — 2026-11-10

The ScarletUI pin includes the shared Button/TabView/Select baseline calculation
from floating-point ascent and descent, following Qt's single-line layout model.
CPU and SGFX snap the translated baseline once; text damage bounds, tile origins
and retained replay anchors use the same calculation. Glyph ink does not influence
label placement, and no per-label pixel offset is applied.

The dependency passed 736 core tests (5 optional tests ignored) and 136 SGFX
renderer tests. Kureha's six settings tests (including the manual light/dark
preview fixture) passed against the final revision. Preview images are in
`artifacts/settings-baseline-final/`; the final package is
`dist/baseline-final/Kureha Code.app`.

The final native macOS SGFX/WGPU package was inspected on Appearance and
Acknowledgements, including notice scrolling and repaint. All six Python checks
and package signature verification passed. The test app used a disposable config.

## Acknowledgements row alignment — 2026-11-10

The row's `Text.padding(6).frame(INFINITY, 32)` placed a 16px measured line at
y=6; `frame` constrains size but does not align its child. Its line center was
y=14 inside a row centered at y=16. The label now uses leading horizontal and
center vertical alignment, with horizontal padding only. No font-specific or
glyph-ink offset is used, and the row height and font size remain unchanged.

`license_labels_center_the_measured_line_including_mixed_japanese` checks the
actual element layout for Latin, Japanese, mixed text, accents and descenders.
`license_alignment_guides` renders those same Text objects before/after at 1x
and 2x. Blue marks the row center, green the measured Text box, and red the
logical baseline (the renderer snaps it to the physical pixel grid). Coordinates
are saved alongside the PNGs in `artifacts/settings-alignment/alignment.csv`.
The corrected first row has top/bottom y=8/24 and baseline y=20.568 within the
32px row; the previous values were y=6/22 and y=18.568. All eight settings tests,
including mixed-text guides and light/dark page renders, passed.
The release build `dist/alignment/Kureha Code.app` passed signature verification;
the corrected list placement was also inspected in the native macOS SGFX/WGPU
window. Mixed-script guide images use the CPU backend with the same Text layout.

The ScarletUI pin also includes cached Rustybuzz shaping and fractional glyph
placement from petitstrawberry/scarlet-ui#99. That revision passed 740 core and
136 SGFX tests; Kureha passed 122 binary and 120 library tests before the row
alignment change. The typography work does not resolve the remaining difference
between the glass rim behind the tab strip and behind opaque pane content.

```sh
KUREHA_SETTINGS_PREVIEW=artifacts/settings-alignment \
  cargo test --bin kureha-code settings_ui::tests -- --include-ignored
```
