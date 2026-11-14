use super::*;
use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Immutable theme revisions keep in-flight syntax jobs tied to their palette.
#[derive(Clone, Debug)]
pub struct CustomTheme {
    pub(super) name: String,
    pub(super) dark: bool,
    pub(super) colors: WorkbenchColors,
    pub(super) terminal: crate::terminal::TerminalPalette,
    pub(super) workspace: Color,
    pub(super) opacity: [f32; 4],
    pub(super) blur: f32,
    pub(super) syntax: SyntaxTheme,
    pub(super) editor_colors: WorkbenchColors,
    pub(super) font_size: f32,
    pub(super) font_stack: Option<scarlet_ui::FontStack>,
    pub font_family: Option<String>,
    pub font_file: Option<String>,
    revision: u64,
}
impl PartialEq for CustomTheme {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
}
impl Eq for CustomTheme {}
impl Hash for CustomTheme {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.revision.hash(state);
    }
}
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

pub fn theme_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KUREHA_THEME").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    Some(root.join("kureha-code/theme.toml"))
}

pub const THEME_TEMPLATE: &str = include_str!("../../assets/theme.toml");

fn color(value: &toml::Value, key: &str) -> Result<Color, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{key}: expected a hex color string"))?;
    let hex = text.strip_prefix('#').unwrap_or("");
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{key}: use #RRGGBB or #RRGGBBAA"));
    }
    let channel = |offset| u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap();
    Ok(Color::rgba(
        channel(0),
        channel(2),
        channel(4),
        if hex.len() == 8 {
            channel(6) as f32 / 255.0
        } else {
            1.0
        },
    ))
}
fn table<'a>(
    value: &'a toml::Value,
    key: &str,
) -> Result<&'a toml::map::Map<String, toml::Value>, String> {
    value
        .as_table()
        .ok_or_else(|| format!("{key}: expected a table"))
}
fn number(value: &toml::Value, key: &str, max: f64) -> Result<f32, String> {
    let value = value
        .as_float()
        .or_else(|| value.as_integer().map(|v| v as f64))
        .ok_or_else(|| format!("{key}: expected a number"))?;
    if !value.is_finite() || !(0.0..=max).contains(&value) {
        return Err(format!("{key}: expected 0..{max}"));
    }
    Ok(value as f32)
}

pub(super) fn definition(base: &Theme) -> CustomTheme {
    let mut value = match base {
        Theme::Custom(theme) => (**theme).clone(),
        _ => CustomTheme {
            name: base.name().into(),
            dark: base == &Theme::Dark,
            colors: base.colors(),
            terminal: base.terminal_palette(),
            editor_colors: base.colors(),
            workspace: base.workspace_background(),
            opacity: [0.58, 0.78, 0.84, 0.78],
            blur: 18.0,
            syntax: base.syntect_theme(),
            font_size: 14.0,
            font_stack: None,
            font_family: None,
            font_file: None,
            revision: 0,
        },
    };
    value.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    value
}

pub fn parse_theme(source: &str) -> Result<Theme, String> {
    if source.len() > 64 * 1024 {
        return Err("Theme exceeds 64 KiB".into());
    }
    let value: toml::Value = source
        .parse()
        .map_err(|e| format!("Invalid theme TOML: {e}"))?;
    let root = table(&value, "theme")?;
    for key in root.keys() {
        if ![
            "name",
            "base",
            "colors",
            "glass",
            "syntax",
            "syntax_rules",
            "code",
            "terminal",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("Unknown theme key: {key}"));
        }
    }
    let base = match root
        .get("base")
        .and_then(toml::Value::as_str)
        .unwrap_or("dark")
    {
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        name => super::preset(name).ok_or_else(|| format!("Unknown base theme: {name}"))?,
    };
    if root.get("base").is_some_and(|v| !v.is_str()) {
        return Err("base must be light or dark".into());
    }
    let name = match root.get("name") {
        None => "Custom",
        Some(v) => v
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 80)
            .ok_or("name must be 1..80 bytes of text")?,
    };
    let mut theme = definition(&base);
    theme.name = name.into();
    if let Some(value) = root.get("colors") {
        for (key, value) in table(value, "colors")? {
            match key.as_str() {
                "selected_background" => {
                    theme.colors.selected_background = Some(color(value, key)?);
                    continue;
                }
                "selected_foreground" => {
                    theme.colors.selected_foreground = Some(color(value, key)?);
                    continue;
                }
                _ => {}
            }
            let target = match key.as_str() {
                "background" => &mut theme.colors.background,
                "surface" => &mut theme.colors.surface,
                "elevated" => &mut theme.colors.elevated,
                "foreground" => &mut theme.colors.foreground,
                "muted" => &mut theme.colors.muted,
                "border" => &mut theme.colors.border,
                "accent" => &mut theme.colors.accent,
                "selection" => &mut theme.colors.selection,
                "current_line" => &mut theme.colors.current_line,
                "minimap_viewport" => &mut theme.colors.minimap_viewport,
                "minimap_border" => &mut theme.colors.minimap_border,
                "error" => &mut theme.colors.error,
                "warning" => &mut theme.colors.warning,
                "workspace" => &mut theme.workspace,
                _ => return Err(format!("Unknown colors key: {key}")),
            };
            *target = color(value, key)?;
        }
    }
    // Workbench overrides also update terminal defaults; explicit terminal
    // settings inherited from a base survive when those colors are untouched.
    if let Some(colors) = root.get("colors").and_then(toml::Value::as_table) {
        if colors.contains_key("background") {
            theme.terminal.background = theme.colors.background;
        }
        if colors.contains_key("foreground") {
            theme.terminal.foreground = theme.colors.foreground;
        }
        if colors.contains_key("accent") {
            theme.terminal.cursor = theme.colors.accent.with_opacity(0.65);
        }
    }
    if let Some(value) = root.get("glass") {
        for (key, value) in table(value, "glass")? {
            match key.as_str() {
                "pane_opacity" => theme.opacity[0] = number(value, key, 1.0)?,
                "tab_opacity" => theme.opacity[1] = number(value, key, 1.0)?,
                "popup_opacity" => theme.opacity[2] = number(value, key, 1.0)?,
                "input_opacity" => theme.opacity[3] = number(value, key, 1.0)?,
                "blur_radius" => theme.blur = number(value, key, 40.0)?,
                _ => return Err(format!("Unknown glass key: {key}")),
            }
        }
    }
    theme.editor_colors = theme.colors;
    if let Some(value) = root.get("code") {
        let editor = table(value, "code")?;
        if let Some(value) = editor.get("theme") {
            let name = value.as_str().ok_or("code.theme must be a preset name")?;
            let selected = match name {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                name => super::preset(name).ok_or_else(|| format!("Unknown code theme: {name}"))?,
            };
            theme.editor_colors = selected.editor_colors();
            theme.terminal = selected.terminal_palette();
            theme.syntax = selected.syntect_theme();
        }
        for (key, value) in editor {
            match key.as_str() {
                "theme" => {}
                "font_size" => {
                    theme.font_size = number(value, key, 48.0)?;
                    if theme.font_size < 8.0 {
                        return Err("code.font_size must be 8..48".into());
                    }
                }
                "font_family" => {
                    theme.font_family = Some(
                        value
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("font_family must be text")?
                            .into(),
                    )
                }
                "font_file" => {
                    theme.font_file = Some(
                        value
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or("font_file must be a path")?
                            .into(),
                    )
                }
                "background" => theme.editor_colors.background = color(value, key)?,
                "foreground" => theme.editor_colors.foreground = color(value, key)?,
                "selection" => theme.editor_colors.selection = color(value, key)?,
                "current_line" => theme.editor_colors.current_line = color(value, key)?,
                _ => return Err(format!("Unknown code key: {key}")),
            }
        }
        if editor.contains_key("background") {
            theme.terminal.background = theme.editor_colors.background;
        }
        if editor.contains_key("foreground") {
            theme.terminal.foreground = theme.editor_colors.foreground;
        }
        if theme.font_file.is_some() && theme.font_family.is_some() {
            return Err("Choose font_family or font_file, not both".into());
        }
    }
    if let Some(value) = root.get("terminal") {
        const ANSI: [&str; 16] = [
            "black",
            "red",
            "green",
            "yellow",
            "blue",
            "magenta",
            "cyan",
            "white",
            "bright_black",
            "bright_red",
            "bright_green",
            "bright_yellow",
            "bright_blue",
            "bright_magenta",
            "bright_cyan",
            "bright_white",
        ];
        for (key, value) in table(value, "terminal")? {
            let target = match key.as_str() {
                "background" => &mut theme.terminal.background,
                "foreground" => &mut theme.terminal.foreground,
                "cursor" => &mut theme.terminal.cursor,
                key => {
                    let index = ANSI
                        .iter()
                        .position(|name| *name == key)
                        .ok_or_else(|| format!("Unknown terminal key: {key}"))?;
                    &mut theme.terminal.ansi[index]
                }
            };
            *target = color(value, key)?;
        }
    }
    theme.syntax.settings.foreground = Some(syntax_color(theme.editor_colors.foreground));
    theme.syntax.settings.background = Some(syntax_color(theme.editor_colors.background));
    if let Some(value) = root.get("syntax") {
        for (key, value) in table(value, "syntax")? {
            let (index, related) = match key.as_str() {
                "comment" => (0, None),
                "string" => (1, Some(9)),
                "constant" => (2, None),
                "number" => (3, None),
                "keyword" => (4, Some(8)),
                "function" => (5, None),
                "type" => (6, None),
                "tag" => (7, None),
                _ => return Err(format!("Unknown syntax key: {key}")),
            };
            let c = Some(syntax_color(color(value, key)?));
            theme.syntax.scopes[index].style.foreground = c;
            if let Some(index) = related {
                theme.syntax.scopes[index].style.foreground = c;
            }
        }
    }
    if let Some(rules) = root.get("syntax_rules") {
        for rule in rules
            .as_array()
            .ok_or("syntax_rules must be an array of tables")?
        {
            let rule = table(rule, "syntax_rules")?;
            if rule.keys().any(|k| k != "scope" && k != "color") {
                return Err("syntax_rules supports scope and color".into());
            }
            let scope: syntect::highlighting::ScopeSelectors = rule
                .get("scope")
                .and_then(toml::Value::as_str)
                .ok_or("syntax rule needs scope")?
                .parse()
                .map_err(|e| format!("Invalid syntax scope: {e}"))?;
            let foreground = syntax_color(color(
                rule.get("color").ok_or("syntax rule needs color")?,
                "syntax rule color",
            )?);
            // Syntect resolves equally specific selectors in first-match order.
            // Replace inherited copies while retaining the fixed base slots used
            // by [syntax], so explicit rules can override preset bracket colors.
            for item in &mut theme.syntax.scopes {
                item.scope
                    .selectors
                    .retain(|selector| !scope.selectors.contains(selector));
            }
            theme.syntax.scopes.push(ThemeItem {
                scope,
                style: StyleModifier {
                    foreground: Some(foreground),
                    ..StyleModifier::default()
                },
            });
        }
    }
    super::ensure_visible_selection(&mut theme.colors);
    super::ensure_visible_selection(&mut theme.editor_colors);
    Ok(Theme::Custom(Arc::new(theme)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_palette_and_typography_do_not_change_workbench_colors() {
        let theme = parse_theme("base='light'\n[code]\ntheme='tokyo-night'\nfont_size=18\nfont_family='Menlo'\nforeground='#eeeeee'").unwrap();
        assert_eq!(theme.colors(), Theme::Light.colors());
        assert_eq!(theme.editor_colors().background, Color::rgb(26, 27, 38));
        assert_eq!(theme.editor_colors().foreground, Color::rgb(238, 238, 238));
        assert_eq!(theme.editor_font_size(), 18.0);
        for (id, _) in super::super::PRESETS {
            assert!(super::super::preset(id).is_some());
        }
        assert!(parse_theme("[code]\nfont_size=0").is_err());
    }

    #[test]
    fn code_palette_pairs_editor_and_terminal_with_optional_terminal_overrides() {
        let ui = super::super::preset("kureha").unwrap();
        for (id, _) in super::super::PRESETS {
            let code = super::super::preset(id).unwrap();
            let combined = ui.with_code_theme(&code);
            assert_eq!(combined.colors(), ui.colors());
            assert_eq!(combined.editor_colors(), code.editor_colors());
            assert_eq!(combined.terminal_palette(), code.terminal_palette());
            assert_eq!(code.terminal_palette().background, code.colors().background);
        }
        let theme = parse_theme("base='kureha'\n[code]\ntheme='monokai'\nbackground='#272722'\n[terminal]\nred='#aabbcc'\ncursor='#dd558880'").unwrap();
        assert_eq!(theme.colors(), ui.colors());
        assert_eq!(
            theme.terminal_palette().background,
            theme.editor_colors().background
        );
        assert_eq!(
            theme.terminal_palette().foreground,
            theme.editor_colors().foreground
        );
        assert_eq!(theme.terminal_palette().ansi[1], Color::rgb(170, 187, 204));
        assert_eq!(
            theme.terminal_palette().cursor,
            Color::rgba(221, 85, 136, 128.0 / 255.0)
        );
        assert!(parse_theme("[terminal]\nred='#xyz'").is_err());
        assert!(parse_theme("[terminal]\npink='#aabbcc'").is_err());
    }

    #[test]
    fn overrides_preserve_base_and_have_independent_revision_identity() {
        let t = parse_theme("base='light'\nname='Plum'\n[colors]\nbackground='#20152c'\nselection='#aabbcc80'\n[syntax]\nkeyword='#ff00aa'\n[glass]\npopup_opacity=0.95").unwrap();
        assert_eq!(t.name(), "Plum");
        assert_eq!(t.colors().background, Color::rgb(32, 21, 44));
        assert_eq!(t.colors().foreground, Theme::Light.colors().foreground);
        assert_eq!(t.syntect_theme().scopes[4].style.foreground.unwrap().r, 255);
        assert_ne!(
            parse_theme("base='dark'").unwrap(),
            parse_theme("base='dark'").unwrap()
        );
        assert!(parse_theme(THEME_TEMPLATE).is_ok());
    }
    #[test]
    fn invalid_settings_are_rejected_instead_of_silently_ignored() {
        for source in [
            "base=2",
            "base='blue'",
            "nope=1",
            "[colors]\nbackground='#xx0000'",
            "[colors]\nbackground='#１２３'",
            "[glass]\npopup_opacity=nan",
            "[glass]\npane_opacity=1.1",
            "[syntax]\nunknown='#000000'",
            "[[syntax_rules]]\nscope=1",
        ] {
            assert!(parse_theme(source).is_err(), "{source}");
        }
    }
}
