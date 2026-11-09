//! Recognizable language marks with contrast-aware 18 px workbench icons.
//! Language colors are accents; the shape still distinguishes non-code files.
use scarlet_ui::icon::{IconFill, IconStyle, IconWeight};
use scarlet_ui::prelude::*;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct FileIcon {
    pub icon: Icon,
    pub color: Color,
}
impl FileIcon {
    pub fn view(self) -> IconView {
        IconView::new(self.icon)
            .size(IconSize::Pixels(18))
            .style(self.style())
            .color(self.color)
    }
    pub fn style(self) -> IconStyle {
        let folder = matches!(self.icon, Icon::Folder | Icon::FolderOpen);
        IconStyle::default()
            .weight(if folder {
                IconWeight::Bold
            } else {
                IconWeight::Normal
            })
            .fill(if folder {
                IconFill::Filled
            } else {
                IconFill::Outline
            })
    }
    pub fn themed(mut self, theme: &crate::theme::Theme, selected: bool) -> Self {
        let colors = theme.colors();
        let background = if selected {
            mix(colors.elevated, colors.selection, colors.selection.a)
        } else {
            colors.elevated
        };
        let endpoint = if contrast(Color::WHITE, background) > contrast(Color::BLACK, background) {
            Color::WHITE
        } else {
            Color::BLACK
        };
        let original = self.color;
        for step in 0..=20 {
            self.color = mix(original, endpoint, step as f32 / 20.0);
            if contrast(self.color, background) >= 4.5 {
                break;
            }
        }
        self
    }
}

fn mix(from: Color, to: Color, fraction: f32) -> Color {
    let channel = |a: f32, b: f32| a + (b - a) * fraction;
    Color {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 1.0,
    }
}
fn luminance(color: Color) -> f32 {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}
fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub fn for_path(path: Option<&Path>) -> FileIcon {
    let name = path
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let extension = path
        .and_then(Path::extension)
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (icon, color) = match (name, extension.as_str()) {
        ("Cargo.toml" | "Cargo.lock", _) => (Icon::Package, Color::rgb(157, 74, 40)),
        ("package.json" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock", _) => {
            (Icon::Package, Color::rgb(58, 125, 68))
        }
        ("Dockerfile" | "Containerfile", _) => (Icon::Package, Color::rgb(30, 112, 167)),
        ("Makefile" | "Justfile" | "justfile", _) => (Icon::Terminal, Color::rgb(104, 91, 137)),
        (".gitignore" | ".gitattributes" | ".gitmodules" | ".editorconfig", _) => {
            (Icon::Settings, Color::rgb(107, 111, 122))
        }
        (_, "rs") => (Icon::BrandRust, Color::rgb(157, 74, 40)),
        (_, "py" | "pyi" | "pyw") => (Icon::BrandPython, Color::rgb(43, 104, 155)),
        (_, "js" | "jsx" | "mjs" | "cjs") => (Icon::BrandJavascript, Color::rgb(159, 118, 13)),
        (_, "ts" | "tsx" | "mts" | "cts") => (Icon::BrandTypescript, Color::rgb(36, 105, 174)),
        (_, "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "ini" | "conf") => {
            (Icon::Settings, Color::rgb(123, 94, 151))
        }
        (_, "md" | "mdx" | "markdown" | "rst") => (Icon::FileDescription, Color::rgb(53, 113, 153)),
        (_, "go") => (Icon::BrandGolang, Color::rgb(20, 142, 169)),
        (_, "html" | "htm") => (Icon::BrandHtml5, Color::rgb(171, 77, 47)),
        (_, "xml" | "svg" | "vue" | "svelte") => (Icon::Code, Color::rgb(171, 77, 47)),
        (_, "css" | "scss" | "sass" | "less") => (Icon::BrandCss3, Color::rgb(117, 76, 162)),
        (_, "sh" | "bash" | "zsh" | "fish" | "ps1") => (Icon::Terminal, Color::rgb(56, 119, 74)),
        (
            _,
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "m" | "mm" | "java" | "kt" | "swift" | "rb"
            | "lua" | "php" | "nix" | "zig" | "ex" | "exs",
        ) => (Icon::FileCode, Color::rgb(67, 104, 136)),
        (_, "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "tiff") => {
            (Icon::Photo, Color::rgb(134, 79, 143))
        }
        (_, "mp3" | "wav" | "flac" | "ogg" | "m4a") => (Icon::FileMusic, Color::rgb(146, 73, 115)),
        (_, "mp4" | "webm" | "mov" | "mkv") => (Icon::Video, Color::rgb(146, 73, 115)),
        (_, "zip" | "gz" | "xz" | "tar" | "zst" | "7z") => {
            (Icon::FileZip, Color::rgb(144, 105, 54))
        }
        (_, "sql" | "db" | "sqlite" | "sqlite3") => (Icon::Database, Color::rgb(51, 122, 122)),
        (_, "txt" | "log" | "csv" | "tsv") => {
            (Icon::FileText, ColorPalette::default().text_secondary())
        }
        _ => (Icon::File, ColorPalette::default().text_secondary()),
    };
    FileIcon { icon, color }
}

pub fn folder(expanded: bool) -> FileIcon {
    FileIcon {
        icon: if expanded {
            Icon::FolderOpen
        } else {
            Icon::Folder
        },
        color: Color::rgb(159, 119, 46),
    }
}

/// Dedicated Kureha mark: a folded crimson leaf with code brackets. Kept local
/// to the app; no global ScarletUI icon enum entry is needed. The parent may
/// export this SVG for desktop/app-bundle metadata without a new dependency.
pub const APP_ICON_SVG: &str = include_str!("../assets/kureha.svg");

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_and_folder_marks_stay_readable_in_all_presets_and_selections() {
        let themes = crate::theme::Theme::ALL.into_iter().chain(
            crate::theme::PRESETS
                .iter()
                .map(|(name, _)| crate::theme::preset(name).unwrap()),
        );
        for theme in themes {
            for selected in [false, true] {
                let colors = theme.colors();
                let bg = if selected {
                    mix(colors.elevated, colors.selection, colors.selection.a)
                } else {
                    colors.elevated
                };
                for icon in [
                    for_path(None),
                    for_path(Some(Path::new("lib.rs"))),
                    for_path(Some(Path::new("app.py"))),
                    for_path(Some(Path::new("app.ts"))),
                    for_path(Some(Path::new("app.js"))),
                    folder(false),
                    folder(true),
                ] {
                    assert!(
                        contrast(icon.themed(&theme, selected).color, bg) >= 4.5,
                        "{} {selected}",
                        theme.name()
                    );
                }
            }
        }
    }
    #[test]
    fn file_kinds_use_consistent_shapes_and_language_accents() {
        assert_eq!(for_path(None).icon, Icon::File);
        assert_eq!(for_path(Some(Path::new("lib.RS"))).icon, Icon::BrandRust);
        assert_ne!(
            for_path(Some(Path::new("lib.rs"))).color,
            for_path(Some(Path::new("lib.py"))).color
        );
        assert_eq!(for_path(Some(Path::new("Cargo.lock"))).icon, Icon::Package);
        assert_eq!(for_path(Some(Path::new("photo.png"))).icon, Icon::Photo);
        assert_eq!(folder(true).icon, Icon::FolderOpen);
        assert_eq!(folder(false).icon, Icon::Folder);
        assert_eq!(
            for_path(Some(Path::new("日本語.md"))).icon,
            Icon::FileDescription
        );
    }
}
