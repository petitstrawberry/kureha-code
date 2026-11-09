use super::Theme;
use std::sync::OnceLock;

pub const PRESETS: &[(&str, &str)] = &[
    ("dracula", "Dracula"),
    ("nord", "Nord"),
    ("solarized-dark", "Solarized Dark"),
    ("solarized-light", "Solarized Light"),
    ("gruvbox-dark", "Gruvbox Dark"),
    ("tokyo-night", "Tokyo Night"),
];

pub fn preset(name: &str) -> Option<Theme> {
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    let themes = THEMES.get_or_init(|| {
        [
            include_str!("../../assets/themes/dracula.toml"),
            include_str!("../../assets/themes/nord.toml"),
            include_str!("../../assets/themes/solarized-dark.toml"),
            include_str!("../../assets/themes/solarized-light.toml"),
            include_str!("../../assets/themes/gruvbox-dark.toml"),
            include_str!("../../assets/themes/tokyo-night.toml"),
        ]
        .into_iter()
        .map(|s| super::parse_theme(s).expect("bundled theme"))
        .collect()
    });
    PRESETS
        .iter()
        .position(|(id, label)| name.eq_ignore_ascii_case(id) || name.eq_ignore_ascii_case(label))
        .map(|i| themes[i].clone())
}
