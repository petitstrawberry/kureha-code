//! File-kind icons share the workbench's Tabler outlines and 16 px sizing.
//! Language colors are accents; the shape still distinguishes non-code files.
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
            .size(IconSize::Small)
            .color(self.color)
    }
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
        (_, "rs") => (Icon::FileCode, Color::rgb(157, 74, 40)),
        (_, "py" | "pyi" | "pyw") => (Icon::FileCode, Color::rgb(43, 104, 155)),
        (_, "js" | "jsx" | "mjs" | "cjs") => (Icon::FileCode, Color::rgb(159, 118, 13)),
        (_, "ts" | "tsx" | "mts" | "cts") => (Icon::FileCode, Color::rgb(36, 105, 174)),
        (_, "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "ini" | "conf") => {
            (Icon::Settings, Color::rgb(123, 94, 151))
        }
        (_, "md" | "mdx" | "markdown" | "rst") => (Icon::FileDescription, Color::rgb(53, 113, 153)),
        (_, "html" | "htm" | "xml" | "svg" | "vue" | "svelte") => {
            (Icon::Code, Color::rgb(171, 77, 47))
        }
        (_, "css" | "scss" | "sass" | "less") => (Icon::Palette, Color::rgb(117, 76, 162)),
        (_, "sh" | "bash" | "zsh" | "fish" | "ps1") => (Icon::Terminal, Color::rgb(56, 119, 74)),
        (
            _,
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "m" | "mm" | "go" | "java" | "kt" | "swift"
            | "rb" | "lua" | "php" | "nix" | "zig" | "ex" | "exs",
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
    fn file_kinds_use_consistent_shapes_and_language_accents() {
        assert_eq!(for_path(None).icon, Icon::File);
        assert_eq!(for_path(Some(Path::new("lib.RS"))).icon, Icon::FileCode);
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
