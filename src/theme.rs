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
        let accent = p.primary();
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
    pub fn with_editor_theme(&self, editor: &Theme) -> Theme {
        let mut combined = custom::definition(self);
        combined.editor_colors = editor.editor_colors();
        combined.syntax = editor.syntect_theme();
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

#[cfg(test)]
mod tests {
    use super::*;

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
pub use custom::{CustomTheme, THEME_TEMPLATE, parse_theme, theme_path};

mod presets;
pub use presets::{PRESETS, preset};
