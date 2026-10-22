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
model into the macOS menu bar. Keyboard commands remain available and currently
use the framework's Control-based primary modifier. This is not complete macOS
menu/Command-key integration.

## Remaining scope

Syntax highlighting, language services, completion and extensions are absent.
Large-file rendering speed, IME composition, every platform, every close/save
error path, and tab-scroll retention under sustained editing are not claimed
verified. The editor engine integration does not by itself improve ScarletUI
TextDocument layout performance.
