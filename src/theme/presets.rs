use super::Theme;
use std::sync::OnceLock;

pub const PRESETS: &[(&str, &str)] = &[
    ("kureha-light", "Kureha Light"),
    ("kureha-dark", "Kureha Dark"),
    ("sakura", "Sakura"),
    ("dracula", "Dracula"),
    ("monokai", "Monokai"),
    ("nord", "Nord"),
    ("solarized-dark", "Solarized Dark"),
    ("solarized-light", "Solarized Light"),
    ("gruvbox-dark", "Gruvbox Dark"),
    ("tokyo-night", "Tokyo Night"),
];

pub fn preset(name: &str) -> Option<Theme> {
    let normalized = name.to_ascii_lowercase();
    let name = match normalized.as_str() {
        "kureha" => "kureha-light",
        "classic light" | "classic-light" => return Some(Theme::Light),
        "classic dark" | "classic-dark" => return Some(Theme::Dark),
        _ => name,
    };
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    let themes = THEMES.get_or_init(|| {
        [
            include_str!("../../assets/themes/kureha.toml"),
            include_str!("../../assets/themes/kureha-dark.toml"),
            include_str!("../../assets/themes/sakura.toml"),
            include_str!("../../assets/themes/dracula.toml"),
            include_str!("../../assets/themes/monokai.toml"),
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
