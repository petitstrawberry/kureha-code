//! Editor themes shared by syntax highlighting, the workbench, and minimap.
//! Apply `style_text_view` to the editor and use `colors` for workbench surfaces.
//! Theme selection is explicit; call `syntax::set_theme` when it changes.
use scarlet_ui::prelude::{
    Color, ColorPalette, ColorScheme, ElevationRole, GlassMaterial, SurfaceStyle, TextView,
};
use syntect::highlighting::{Color as SyntaxColor, StyleModifier, Theme as SyntaxTheme, ThemeItem};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Theme {
    #[default]
    Light,
    Dark,
    Custom(std::sync::Arc<CustomTheme>),
}

/// Colors for editor and workbench chrome, derived from ScarletUI's palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkbenchColors {
    pub background: Color,
    pub surface: Color,
    pub elevated: Color,
    pub foreground: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub selection: Color,
    pub current_line: Color,
    pub minimap_viewport: Color,
    pub minimap_border: Color,
    pub error: Color,
    pub warning: Color,
}

impl Theme {
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    pub fn name(&self) -> &str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Custom(theme) => &theme.name,
        }
    }

    pub fn toggle(&self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
            Self::Custom(theme) => {
                if theme.dark {
                    Self::Light
                } else {
                    Self::Dark
                }
            }
        }
    }

    pub fn scheme(&self) -> ColorScheme {
        match self {
            Self::Light => ColorScheme::Light,
            Self::Dark => ColorScheme::Dark,
            Self::Custom(theme) => {
                if theme.dark {
                    ColorScheme::Dark
                } else {
                    ColorScheme::Light
                }
            }
        }
    }

    pub fn palette(&self) -> ColorPalette {
        match self {
            Self::Light => ColorPalette::light(),
            Self::Dark => ColorPalette::dark(),
            Self::Custom(theme) => {
                if theme.dark {
                    ColorPalette::dark()
                } else {
                    ColorPalette::light()
                }
            }
        }
    }

    pub fn colors(&self) -> WorkbenchColors {
        if let Self::Custom(theme) = self {
            return theme.colors;
        }
        let p = self.palette();
        let foreground = match self {
            Self::Light => Color::rgb(36, 41, 47),
            Self::Dark => Color::rgb(200, 204, 211),
            Self::Custom(_) => unreachable!(),
        };
        let accent = if self == &Self::Light {
            Color::rgb(51, 114, 187)
        } else {
            Color::rgb(138, 180, 230)
        };
        let mut colors = WorkbenchColors {
            background: p.background(),
            surface: p.surface_variant(),
            elevated: p.surface(),
            foreground,
            muted: p.text_secondary(),
            border: p.divider(),
            accent,
            selection: Color { a: 0.25, ..accent },
            current_line: p.background_secondary(),
            minimap_viewport: Color { a: 0.15, ..accent },
            minimap_border: Color { a: 0.70, ..accent },
            error: p.error(),
            warning: p.warning(),
        };
        if self == &Self::Dark {
            colors.background = Color::rgb(25, 28, 34);
            colors.surface = Color::rgb(33, 39, 49);
            colors.elevated = Color::rgb(40, 46, 56);
            colors.muted = Color::rgb(143, 149, 158);
            colors.border = Color::rgb(53, 56, 62);
            colors.current_line = Color::rgb(34, 37, 42);
            colors.selection = Color::rgba(63, 83, 115, 0.70);
            colors.minimap_viewport = Color::rgba(114, 125, 144, 0.15);
            colors.minimap_border = Color::rgba(114, 125, 144, 0.45);
        } else {
            colors.background = Color::rgb(252, 253, 255);
            colors.surface = Color::rgb(235, 240, 248);
            colors.elevated = Color::rgb(250, 252, 255);
            colors.selection = Color::rgba(51, 114, 187, 0.24);
        }
        colors
    }

    pub fn terminal_palette(&self) -> crate::terminal::TerminalPalette {
        if let Self::Custom(theme) = self {
            return theme.terminal;
        }
        let c = self.colors();
        let ansi = if self == &Self::Light {
            [
                0x363b44, 0xb63851, 0x3d7350, 0x876024, 0x326cad, 0x8851a1, 0x267885, 0xe0e5ed,
                0x667080, 0xc23a55, 0x347747, 0x936018, 0x286dbb, 0x9746a3, 0x167b8b, 0xf5f7fa,
            ]
        } else {
            [
                0x303640, 0xe78291, 0x9ac98a, 0xe3c784, 0x8ab4e6, 0xc6a0dc, 0x86c9c4, 0xd4d8df,
                0x737e91, 0xf397a4, 0xb1d8a3, 0xf0d9a5, 0xa9c8f0, 0xd8b6e9, 0xa5dcd7, 0xf1f3f5,
            ]
        }
        .map(|rgb| Color::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
        crate::terminal::TerminalPalette {
            background: c.background,
            foreground: c.foreground,
            cursor: c.accent.with_opacity(0.65),
            ansi,
        }
    }

    /// The exposed workspace behind the inset panes.
    pub fn workspace_background(&self) -> Color {
        match self {
            Self::Light => Color::rgb(222, 230, 241),
            Self::Dark => Color::rgb(19, 25, 36),
            Self::Custom(theme) => theme.workspace,
        }
    }

    /// Glass pane, with a rim and a restrained shadow above the workspace.
    pub fn chrome_style(&self) -> SurfaceStyle {
        SurfaceStyle::bubble_glass()
            .material(self.glass_material(0))
            .elevation(ElevationRole::Raised)
    }

    pub fn tab_style(&self) -> SurfaceStyle {
        SurfaceStyle::bubble_glass()
            .material(self.glass_material(1))
            .elevation(ElevationRole::Raised)
    }

    /// Denser glass keeps transient text readable over source code.
    pub fn popup_style(&self) -> SurfaceStyle {
        SurfaceStyle::bubble_glass()
            .material(self.glass_material(2))
            .elevation(ElevationRole::Floating)
    }

    pub fn input_material(&self) -> GlassMaterial {
        self.glass_material(3)
    }

    fn glass_material(&self, index: usize) -> GlassMaterial {
        let (opacity, blur) = match self {
            Self::Custom(theme) => (theme.opacity[index], theme.blur),
            _ => ([0.58, 0.78, 0.84, 0.78][index], 18.0),
        };
        GlassMaterial::new(blur, self.colors().elevated.with_opacity(opacity))
    }

    /// Editor colors may differ from the surrounding workbench.
    pub fn editor_colors(&self) -> WorkbenchColors {
        match self {
            Self::Custom(t) => t.editor_colors,
            _ => self.colors(),
        }
    }
    pub fn editor_font_size(&self) -> f32 {
        match self {
            Self::Custom(t) => t.font_size,
            _ => 14.0,
        }
    }
    pub fn editor_font(&self) -> Option<scarlet_ui::FontStack> {
        match self {
            Self::Custom(t) => t.font_stack.clone(),
            _ => None,
        }
    }
    /// Resolve the configured font off the UI thread before publishing this revision.
    pub fn prepare_font(mut self, directory: &std::path::Path) -> Result<Self, String> {
        if let Self::Custom(theme) = &mut self {
            let font = crate::fonts::configured_stack(
                theme.font_family.as_deref(),
                theme.font_file.as_deref(),
                directory,
            )?;
            std::sync::Arc::make_mut(theme).font_stack = font;
        }
        Ok(self)
    }
    pub fn with_code_theme(&self, editor: &Theme) -> Theme {
        let mut combined = custom::definition(self);
        combined.editor_colors = editor.editor_colors();
        combined.terminal = editor.terminal_palette();
        combined.syntax = editor.syntect_theme();
        Theme::Custom(std::sync::Arc::new(combined))
    }

    pub fn with_font_size(&self, size: f32) -> Theme {
        let mut combined = custom::definition(self);
        combined.font_size = size.clamp(8.0, 48.0);
        Theme::Custom(std::sync::Arc::new(combined))
    }

    /// Preserves document, selection, scroll bindings, and decorations.
    pub fn style_text_view(&self, view: TextView) -> TextView {
        let c = self.editor_colors();
        view.background(c.background)
            .text_color(c.foreground)
            .placeholder_color(c.muted)
            .selection_color(c.selection)
            .current_line_color(c.current_line)
            .border_color(c.border)
            .focused_border_color(c.accent)
    }

    /// Lexical token colors and the editor's neutral foreground share one model.
    pub(crate) fn syntect_theme(&self) -> SyntaxTheme {
        if let Self::Custom(theme) = self {
            return theme.syntax.clone();
        }
        let mut theme = SyntaxTheme::default();
        theme.settings.foreground = Some(syntax_color(self.colors().foreground));
        theme.settings.background = Some(syntax_color(self.colors().background));
        let colors = match self {
            Self::Light => [
                [93, 112, 96],
                [163, 21, 21],
                [0, 92, 197],
                [9, 115, 83],
                [119, 41, 163],
                [121, 94, 38],
                [38, 111, 137],
                [0, 92, 157],
            ],
            Self::Dark => [
                [135, 161, 125],
                [225, 164, 133],
                [112, 177, 241],
                [161, 208, 166],
                [207, 151, 220],
                [225, 211, 150],
                [99, 201, 184],
                [112, 177, 241],
            ],
            Self::Custom(_) => unreachable!(),
        };
        for (scopes, index) in [
            ("comment", 0),
            ("string", 1),
            ("constant, support.constant", 2),
            ("constant.numeric", 3),
            ("keyword, storage", 4),
            ("entity.name.function, support.function", 5),
            ("entity.name.type, support.type, entity.name.class", 6),
            ("entity.name.tag, entity.name.section", 7),
            ("markup.heading, markup.bold", 4),
            ("markup.inline.raw, markup.raw", 1),
        ] {
            let [r, g, b] = colors[index];
            theme.scopes.push(ThemeItem {
                scope: scopes.parse().expect("built-in syntax scope selector"),
                style: StyleModifier {
                    foreground: Some(SyntaxColor { r, g, b, a: 255 }),
                    ..StyleModifier::default()
                },
            });
        }
        theme
    }
}

fn syntax_color(c: Color) -> SyntaxColor {
    SyntaxColor {
        r: (c.r * 255.0).round() as u8,
        g: (c.g * 255.0).round() as u8,
        b: (c.b * 255.0).round() as u8,
        a: (c.a * 255.0).round() as u8,
    }
}

/// `top` drawn over the opaque `below`.
fn over(top: Color, below: Color) -> Color {
    let channel = |t: f32, b: f32| t * top.a + b * (1.0 - top.a);
    Color {
        r: channel(top.r, below.r),
        g: channel(top.g, below.g),
        b: channel(top.b, below.b),
        a: 1.0,
    }
}
/// WCAG contrast ratio of two opaque colors.
pub fn contrast(a: Color, b: Color) -> f32 {
    let luminance = |c: Color| {
        let channel = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

impl WorkbenchColors {
    /// Behind a selected row or the active tab: the accent, faintly, so it
    /// marks the row without washing out what is on it.
    pub fn highlight(&self) -> Color {
        self.accent.with_opacity(0.16)
    }
    /// Text of a selected row or the active tab, over the highlight on
    /// `under`: the accent, moved toward the foreground only as far as it
    /// takes to read as well as plain text does there (4.5:1 at most).
    pub fn emphasis(&self, under: Color) -> Color {
        let behind = over(self.highlight(), Color { a: 1.0, ..under });
        let target = contrast(Color { a: 1.0, ..self.foreground }, behind).min(4.5);
        (0..=20)
            .map(|step| {
                let t = step as f32 / 20.0;
                let mix = |a: f32, b: f32| a + (b - a) * t;
                Color {
                    r: mix(self.accent.r, self.foreground.r),
                    g: mix(self.accent.g, self.foreground.g),
                    b: mix(self.accent.b, self.foreground.b),
                    a: 1.0,
                }
            })
            .find(|color| contrast(*color, behind) >= target - 0.01)
            .unwrap_or(self.foreground)
    }
}

/// Whether `selection`, drawn over each of `under`, can be told apart from
/// it: far enough in color and in lightness. A selection the same as the
/// current line or a list's surface would vanish there.
pub fn selection_visible(selection: Color, under: &[Color]) -> bool {
    under.iter().all(|below| {
        let over = |top: f32, below: f32| top * selection.a + below * (1.0 - selection.a);
        let drawn = [
            over(selection.r, below.r),
            over(selection.g, below.g),
            over(selection.b, below.b),
        ];
        let base = [below.r, below.g, below.b];
        let distance = drawn
            .iter()
            .zip(base)
            .map(|(a, b)| ((a - b) * 255.0).powi(2))
            .sum::<f32>()
            .sqrt();
        let luminance = |c: [f32; 3]| {
            let channel = |v: f32| {
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
        };
        let (a, b) = (luminance(drawn), luminance(base));
        let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
        distance >= 30.0 && ratio >= 1.25
    })
}

/// Replaces a selection that would vanish over the background, the current
/// line or a surface with the accent, translucent enough to read through.
pub fn ensure_visible_selection(colors: &mut WorkbenchColors) {
    let under = [colors.background, colors.surface, colors.current_line];
    if selection_visible(colors.selection, &under) {
        return;
    }
    if let Some(alpha) = [0.35, 0.5, 0.65]
        .into_iter()
        .find(|alpha| selection_visible(Color { a: *alpha, ..colors.accent }, &under))
    {
        colors.selection = Color { a: alpha, ..colors.accent };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Selected rows and the active tab read in every bundled theme, and
    /// keep the accent's hue where the theme's accent already reads.
    #[test]
    fn emphasis_reads_on_the_highlight_in_every_theme() {
        let themes = Theme::ALL.into_iter().chain(
            PRESETS
                .iter()
                .map(|(name, _)| preset(name).unwrap()),
        );
        for theme in themes {
            let colors = theme.colors();
            let behind = over(colors.highlight(), Color { a: 1.0, ..colors.elevated });
            let text = colors.emphasis(colors.elevated);
            let plain = contrast(Color { a: 1.0, ..colors.foreground }, behind);
            assert!(
                contrast(text, behind) >= plain.min(4.5) - 0.01,
                "{}: as readable as plain text",
                theme.name()
            );
            if contrast(Color { a: 1.0, ..colors.accent }, behind) >= plain.min(4.5) {
                assert_eq!(text, Color { a: 1.0, ..colors.accent }, "{}", theme.name());
            }
        }
    }

    #[test]
    fn every_bundled_theme_has_a_selection_that_shows() {
        let mut themes = vec![Theme::Light, Theme::Dark];
        themes.extend(PRESETS.iter().filter_map(|(id, _)| preset(id)));
        for theme in themes {
            for colors in [theme.colors(), theme.editor_colors()] {
                assert!(
                    selection_visible(
                        colors.selection,
                        &[colors.background, colors.surface, colors.current_line]
                    ),
                    "{}: the selection blends in",
                    theme.name()
                );
            }
        }
    }

    #[test]
    fn a_custom_selection_that_blends_in_falls_back_to_the_accent() {
        let theme = parse_theme(
            "base='dark'\nname='Flat'\n[colors]\nbackground='#282a36'\nsurface='#44475a'\n\
             current_line='#44475a'\nselection='#44475a'\naccent='#bd93f9'\n",
        )
        .unwrap();
        let colors = theme.colors();
        assert_eq!((colors.selection.r, colors.selection.a < 1.0), (colors.accent.r, true));
        assert!(selection_visible(
            colors.selection,
            &[colors.background, colors.surface, colors.current_line]
        ));
    }

    fn luminance(c: Color) -> f32 {
        fn linear(v: f32) -> f32 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }

    #[test]
    fn themes_match_neutral_syntax_and_keep_readable_contrast() {
        for theme in Theme::ALL {
            let c = theme.colors();
            let syntax = theme.syntect_theme();
            assert_eq!(syntax.settings.foreground, Some(syntax_color(c.foreground)));
            assert_eq!(syntax.settings.background, Some(syntax_color(c.background)));
            for color in std::iter::once(c.foreground).chain(syntax.scopes.iter().map(|item| {
                let c = item.style.foreground.unwrap();
                Color::rgb(c.r, c.g, c.b)
            })) {
                let a = luminance(color);
                let b = luminance(c.background);
                assert!(
                    (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.0,
                    "{theme:?}: {color:?}"
                );
            }
        }
        assert_eq!(Theme::Dark.toggle(), Theme::Light);
        assert_ne!(Theme::Light.colors(), Theme::Dark.colors());
    }
}

mod custom;
pub use custom::{parse_theme, theme_path, CustomTheme, THEME_TEMPLATE};

mod presets;
pub use presets::{preset, PRESETS};
