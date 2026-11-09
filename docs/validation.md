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
