//! Editor themes shared by syntax highlighting, the workbench, and minimap.
//! Apply `style_text_view` to the editor and use `colors` for workbench surfaces.
//! Theme selection is explicit; call `syntax::set_theme` when it changes.
use scarlet_ui::prelude::{Color, ColorPalette, ColorScheme, TextView};
use syntect::highlighting::{Color as SyntaxColor, StyleModifier, Theme as SyntaxTheme, ThemeItem};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Theme {
    #[default]
    Light,
    Dark,
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

    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub const fn toggle(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    pub const fn scheme(self) -> ColorScheme {
        match self {
            Self::Light => ColorScheme::Light,
            Self::Dark => ColorScheme::Dark,
        }
    }

    pub fn palette(self) -> ColorPalette {
        match self {
            Self::Light => ColorPalette::light(),
            Self::Dark => ColorPalette::dark(),
        }
    }

    pub fn colors(self) -> WorkbenchColors {
        let p = self.palette();
        let foreground = match self {
            Self::Light => Color::rgb(36, 41, 47),
            Self::Dark => Color::rgb(200, 204, 211),
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
        if self == Self::Dark {
            colors.background = Color::rgb(28, 30, 34);
            colors.surface = Color::rgb(33, 35, 39);
            colors.elevated = Color::rgb(40, 42, 46);
            colors.muted = Color::rgb(143, 149, 158);
            colors.border = Color::rgb(53, 56, 62);
            colors.current_line = Color::rgb(34, 37, 42);
            colors.selection = Color::rgba(63, 83, 115, 0.70);
            colors.minimap_viewport = Color::rgba(114, 125, 144, 0.15);
            colors.minimap_border = Color::rgba(114, 125, 144, 0.45);
        } else {
            colors.selection = Color::rgba(51, 114, 187, 0.24);
        }
        colors
    }

    /// Preserves document, selection, scroll bindings, and decorations.
    pub fn style_text_view(self, view: TextView) -> TextView {
        let c = self.colors();
        view.background(c.background)
            .text_color(c.foreground)
            .placeholder_color(c.muted)
            .selection_color(c.selection)
            .current_line_color(c.current_line)
            .border_color(c.border)
            .focused_border_color(c.accent)
    }

    /// Lexical token colors and the editor's neutral foreground share one model.
    pub(crate) fn syntect_theme(self) -> SyntaxTheme {
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
