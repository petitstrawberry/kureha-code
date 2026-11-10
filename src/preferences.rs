//! User choices, kept separate from hand-edited theme definitions.
use kureha_code::theme::{preset, Theme};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Preferences {
    pub color_theme: Option<String>,
    pub code_theme: Option<String>,
    pub font_size: Option<f32>,
    pub hide_minimap: bool,
}
#[derive(Clone, Debug)]
pub(super) enum Change {
    Color(Option<String>),
    Code(Option<String>),
    FontSize(Option<f32>),
    Minimap(bool),
}
impl Preferences {
    pub fn parse(source: &str) -> Result<Self, String> {
        let value: toml::Table = source
            .parse()
            .map_err(|e| format!("Invalid settings: {e}"))?;
        let mut result = Self::default();
        for (key, value) in value {
            match key.as_str() {
                "color_theme" | "code_theme" => {
                    let name = value
                        .as_str()
                        .ok_or_else(|| format!("{key} must be a theme name"))?;
                    let _ = named_theme(name)?;
                    if key == "color_theme" {
                        result.color_theme = Some(name.into());
                    } else {
                        result.code_theme = Some(name.into());
                    }
                }
                "font_size" => {
                    let size = value
                        .as_float()
                        .or_else(|| value.as_integer().map(|n| n as f64))
                        .filter(|n| n.is_finite() && (8.0..=48.0).contains(n))
                        .ok_or("font_size must be between 8 and 48")?;
                    result.font_size = Some(size as f32);
                }
                "hide_minimap" => {
                    result.hide_minimap = value
                        .as_bool()
                        .ok_or("hide_minimap must be true or false")?
                }
                _ => return Err(format!("Unknown settings key: {key}")),
            }
        }
        Ok(result)
    }
    pub fn change(&mut self, change: &Change) {
        match change {
            Change::Color(value) => {
                self.color_theme = value.clone();
                self.code_theme = None;
            }
            Change::Code(value) => self.code_theme = value.clone(),
            Change::FontSize(value) => self.font_size = *value,
            Change::Minimap(visible) => self.hide_minimap = !visible,
        }
    }
    pub fn apply(&self, custom: Theme) -> Result<Theme, String> {
        let mut theme = match &self.color_theme {
            Some(name) => named_theme(name)?,
            None => custom,
        };
        if let Some(name) = &self.code_theme {
            theme = theme.with_code_theme(&named_theme(name)?);
        }
        if let Some(size) = self.font_size {
            theme = theme.with_font_size(size);
        }
        Ok(theme)
    }
}
pub(super) fn named_theme(name: &str) -> Result<Theme, String> {
    match name.to_ascii_lowercase().as_str() {
        "light" => Ok(Theme::Light),
        "dark" => Ok(Theme::Dark),
        name => preset(name).ok_or_else(|| format!("Unknown theme: {name}")),
    }
}
