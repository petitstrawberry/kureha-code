# Editor fonts

These unmodified fonts and their accompanying licenses were copied from the
Scarlet desktop bundle's `bundles/desktop/fs/share/fonts` directory.

- `JetBrainsMonoNerdFontMono-Regular.ttf`: Nerd Fonts JetBrains Mono 3.4.0;
  see `JETBRAINS_MONO_NERD_FONT_LICENSE.txt` (MIT and SIL Open Font License 1.1).
- `Mplus1-Regular.ttf`: see `MPLUS_FONTS_OFL.txt` (SIL Open Font License 1.1).
- `NotoSansCJKjp-Regular.otf`: see `NOTO_SANS_CJK_OFL.txt` (SIL Open Font License 1.1).

Host builds embed these assets for portable editor typography. Scarlet builds
load the desktop's `/share/fonts` copies. `src/fonts.rs` constructs only the
editor's font stack; it does not change ScarletUI's global fonts.
