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

ScarletUI's winit backend currently does not install the app's File/Edit menu
model into the macOS menu bar. Keyboard commands remain available. This early observation predates the Command
modifier support verified below; the macOS menu-bar integration remains absent.

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
The macOS menu-bar limitation above remains; primary-key behavior was updated as
recorded below.


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
