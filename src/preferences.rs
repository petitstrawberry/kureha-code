//! User choices, kept separate from hand-edited theme definitions.
use kureha_code::theme::{Theme, preset};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Preferences {
    pub color_theme: Option<String>,
    pub code_theme: Option<String>,
    pub font_size: Option<f32>,
    pub hide_minimap: bool,
    pub word_wrap: bool,
    pub wrap_at_words: bool,
    /// Format with the chosen formatter before each save (extension formatters).
    pub format_on_save: bool,
    /// Formatter per language id: `"lsp"` or an extension id.
    pub formatter: BTreeMap<String, String>,
    /// `[extensions.allow_process]`: programs each extension may run.
    pub allow_process: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone, Debug)]
pub(super) enum Change {
    Color(Option<String>),
    Code(Option<String>),
    FontSize(Option<f32>),
    Minimap(bool),
    WordWrap(bool),
    WrapAtWords(bool),
    FormatOnSave(bool),
    /// Grants or withdraws one program for one extension.
    AllowProcess {
        extension: String,
        program: String,
        allowed: bool,
    },
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            color_theme: None,
            code_theme: None,
            font_size: None,
            hide_minimap: false,
            word_wrap: true,
            wrap_at_words: true,
            format_on_save: false,
            formatter: BTreeMap::new(),
            allow_process: BTreeMap::new(),
        }
    }
}
impl Preferences {
    pub fn wrap_mode(&self) -> scarlet_ui::WrapMode {
        if !self.word_wrap {
            scarlet_ui::WrapMode::None
        } else if self.wrap_at_words {
            scarlet_ui::WrapMode::Code
        } else {
            scarlet_ui::WrapMode::CodeAnywhere
        }
    }
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
                "wrap_at_words" => {
                    result.wrap_at_words = value
                        .as_bool()
                        .ok_or("wrap_at_words must be true or false")?;
                }
                "word_wrap" => {
                    result.word_wrap = value.as_bool().ok_or("word_wrap must be true or false")?;
                }
                "hide_minimap" => {
                    result.hide_minimap = value
                        .as_bool()
                        .ok_or("hide_minimap must be true or false")?
                }
                "format_on_save" => {
                    result.format_on_save = value
                        .as_bool()
                        .ok_or("format_on_save must be true or false")?;
                }
                "formatter" => {
                    let table = value
                        .as_table()
                        .ok_or("formatter must be a table of language = formatter")?;
                    for (language, choice) in table {
                        let choice = choice
                            .as_str()
                            .filter(|c| *c == "lsp" || kureha_code::extensions::valid_id(c))
                            .ok_or_else(|| {
                                format!("formatter.{language} must be \"lsp\" or an extension id")
                            })?;
                        result.formatter.insert(language.clone(), choice.into());
                    }
                }
                "extensions" => {
                    let table = value.as_table().ok_or("extensions must be a table")?;
                    for (key, value) in table {
                        if key != "allow_process" {
                            return Err(format!("Unknown settings key: extensions.{key}"));
                        }
                        let grants = value.as_table().ok_or(
                            "extensions.allow_process must be a table of extension = [programs]",
                        )?;
                        for (extension, programs) in grants {
                            let context = format!("extensions.allow_process.\"{extension}\"");
                            if !kureha_code::extensions::valid_id(extension) {
                                return Err(format!("{context}: invalid extension id"));
                            }
                            let programs = programs
                                .as_array()
                                .ok_or_else(|| format!("{context} must be a list of programs"))?
                                .iter()
                                .map(|program| {
                                    program
                                        .as_str()
                                        .filter(|p| kureha_code::extensions::valid_program(p))
                                        .map(String::from)
                                        .ok_or_else(|| {
                                            format!("{context}: programs must be bare names")
                                        })
                                })
                                .collect::<Result<BTreeSet<_>, _>>()?;
                            if !programs.is_empty() {
                                result.allow_process.insert(extension.clone(), programs);
                            }
                        }
                    }
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
            Change::WordWrap(enabled) => self.word_wrap = *enabled,
            Change::WrapAtWords(enabled) => self.wrap_at_words = *enabled,
            Change::FormatOnSave(enabled) => self.format_on_save = *enabled,
            Change::AllowProcess {
                extension,
                program,
                allowed,
            } => {
                let programs = self.allow_process.entry(extension.clone()).or_default();
                if *allowed {
                    programs.insert(program.clone());
                } else {
                    programs.remove(program);
                }
                if programs.is_empty() {
                    self.allow_process.remove(extension);
                }
            }
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
        name => preset(name)
            .or_else(|| kureha_code::extensions::registry().theme(name))
            .ok_or_else(|| format!("Unknown theme: {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_defaults_on_and_validates_persisted_choices() {
        assert!(Preferences::parse("").unwrap().word_wrap);
        assert!(Preferences::default().wrap_at_words);
        assert_eq!(
            Preferences::default().wrap_mode(),
            scarlet_ui::WrapMode::Code
        );
        assert_eq!(
            Preferences::parse("wrap_at_words=false")
                .unwrap()
                .wrap_mode(),
            scarlet_ui::WrapMode::CodeAnywhere
        );
        assert!(Preferences::parse("wrap_at_words='yes'").is_err());
        assert!(!Preferences::parse("word_wrap = false").unwrap().word_wrap);
        assert!(Preferences::parse("word_wrap = 'off'").is_err());
        let mut preferences = Preferences::default();
        preferences.change(&Change::WordWrap(false));
        assert!(!preferences.word_wrap);
        preferences.change(&Change::Color(Some("Nord".into())));
        assert!(!preferences.word_wrap);
    }
    #[test]
    fn formatter_choices_are_validated() {
        let preferences = Preferences::parse(
            "format_on_save = true\n[formatter]\njson = \"example.text-tools\"\nrust = \"lsp\"\n",
        )
        .unwrap();
        assert!(preferences.format_on_save);
        assert_eq!(preferences.formatter["json"], "example.text-tools");
        assert_eq!(preferences.formatter["rust"], "lsp");
        assert!(Preferences::parse("format_on_save = 1").is_err());
        assert!(Preferences::parse("[formatter]\njson = \"Not An Id\"").is_err());
        assert!(Preferences::parse("formatter = \"lsp\"").is_err());
    }
    #[test]
    fn process_grants_are_validated_and_changed_one_program_at_a_time() {
        let preferences = Preferences::parse(
            "[extensions.allow_process]\n\"example.fmt\" = [\"ruff\", \"prettier\"]\n\"example.none\" = []\n",
        )
        .unwrap();
        assert_eq!(
            preferences.allow_process,
            BTreeMap::from([(
                "example.fmt".into(),
                BTreeSet::from(["prettier".into(), "ruff".into()])
            )])
        );
        for bad in [
            "[extensions]\nallow_process = 1",
            "[extensions]\nother = {}",
            "[extensions.allow_process]\n\"Not An Id\" = [\"ruff\"]",
            "[extensions.allow_process]\n\"example.fmt\" = [\"/bin/sh\"]",
            "[extensions.allow_process]\n\"example.fmt\" = [\"sh -c x\"]",
            "[extensions.allow_process]\n\"example.fmt\" = \"ruff\"",
        ] {
            assert!(Preferences::parse(bad).is_err(), "{bad}");
        }
        let mut preferences = preferences;
        let change = |program: &str, allowed| Change::AllowProcess {
            extension: "example.fmt".into(),
            program: program.into(),
            allowed,
        };
        preferences.change(&change("ruff", false));
        preferences.change(&change("prettier", false));
        assert!(preferences.allow_process.is_empty());
        preferences.change(&change("ruff", true));
        assert!(preferences.allow_process["example.fmt"].contains("ruff"));
    }
}
