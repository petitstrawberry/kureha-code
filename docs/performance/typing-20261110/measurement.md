# Native typing check, 2026-11-10

The final editor changes were exercised in a disposable macOS ARM64 release app
with `SCARLET_UI_WINIT_RENDERER=sgfx` and `SCARLET_UI_FRAME_LOG=1`. The fixture has
150 headings and 150 paragraphs (600 lines), with the paragraph text
`Paragraph with **bold** and some text.`

A temporary idle hook inserted `x` at byte 4 every 100 ms, steps 11–39, then exited
at step 55. One run showed only the source; another opened its live Markdown
preview beside it. Both runs were configured by macOS to 867×900 after requesting
1100×760. Source and preview runs used the same executable. The temporary hook
was removed afterwards and the ordinary release binary rebuilt.

Statistics include every presented frame from the first edit through the final
async updates, including the first clean-to-dirty transition. They include
highlight and preview completion frames, so they are not one frame per keystroke.
Startup and window configuration are excluded. No compilation or test suite ran
concurrently with the retained measurements.

| Frame work | Source only | Source and preview |
| --- | ---: | ---: |
| Edits | 29 | 29 |
| Presented frames | 57 | 61 |
| Median | 7.179 ms | 8.425 ms |
| p95 (nearest rank) | 16.676 ms | 14.593 ms |
| Maximum | 18.609 ms | 59.385 ms |
| CPU update/layout median | 2.756 ms | 2.628 ms |

`frame_us` covers dirty-tree updates and backend preparation/submission, not
input-to-display latency or GPU completion. The maximum preview frame shows
that this does not establish a worst-case latency bound. The edits were driven
through the app's document edit API in the native event loop, not OS keyboard
injection. A separate mounted renderer test sends real `TextInputCommit` events
and verifies document updates and local invalidation.

- [Source-only log](source.txt)
- [Preview-visible log](preview.txt)
- [Statistics](summary.json)

## Incremental syntax checks

A worker-local cache keeps line-relative token spans and parser/highlighter
checkpoints every 32 lines. After an edit, parsing resumes at the checkpoint
before the changed region. It reuses the unchanged suffix only when both parser
and highlighting state match the previous result. Checkpoints use Syntect's
[`state` / `from_state` APIs](https://docs.rs/syntect/5.3.0/syntect/easy/struct.HighlightLines.html).

The 1,000-line Rust regression reparses 32, 33 and 31 lines for a local replacement,
a Unicode line insertion and Undo respectively. Each result equals a full parse.
Removing multiline string/comment delimiters is tested in Rust, Python, TOML and
TypeScript: parsing continues past 64 lines when state has not converged, and the
final colors still match a full parse. Theme/path/grammar changes do not reuse
incompatible checkpoints. Worker caches retain at most 12 documents, with the
existing byte/line/token/time limits.

UI-side provisional spans are remapped through UTF-8-safe changed ranges, keeping
unchanged text colored while the worker runs. These are interim colors, not a
claim that a token after an edited delimiter is semantically final before parsing.
