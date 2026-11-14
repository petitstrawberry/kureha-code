//! Extension manifests and declarative contributions.
//!
//! Extensions live in `<extensions dir>/<folder>/extension.toml`. This module
//! only reads data: themes, file-extension mappings and language server
//! launch settings. Extension code runs only in [`host`]. See `docs/extensions.md`.

pub mod host;
pub mod process;
pub mod runtime;

use crate::theme::Theme;
use std::{
    path::{Component, Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Host API generation. Manifests asking for another value are rejected.
pub const API_VERSION: i64 = 1;
pub const MANIFEST_FILE: &str = "extension.toml";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_EXTENSIONS: usize = 256;
const MAX_LIST: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceAccess {
    None,
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub workspace: WorkspaceAccess,
    /// Bare executable names the extension may launch.
    pub process: Vec<String>,
    /// Host names the extension may connect to.
    pub network: Vec<String>,
    pub clock: bool,
    /// May set the environment of the programs Kureha starts (with the
    /// user's grant), through `[contributes.environment]`.
    pub environment: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            workspace: WorkspaceAccess::None,
            process: Vec::new(),
            network: Vec::new(),
            clock: false,
            environment: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    pub languages: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandContribution {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewLocation {
    Side,
    Bottom,
    Tab,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewContribution {
    pub id: String,
    pub title: String,
    pub location: ViewLocation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageContribution {
    pub id: String,
    /// Lowercase file extensions without the leading dot.
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageServerContribution {
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
}

/// The key part of a shortcut. Characters are stored in lowercase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    F(u8),
}

/// One key press with modifiers, written like `Primary+Alt+L`.
///
/// `Primary` is Command on macOS and Control elsewhere; `Ctrl` is always the
/// Control key, so on other platforms the two are the same press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub primary: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl KeyChord {
    /// Parses one chord. Chords with a second press (`Primary+K Primary+T`)
    /// are rejected.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().contains(' ') {
            return Err(format!("`{text}`: key sequences are not supported"));
        }
        let mut chord = Self {
            primary: false,
            control: false,
            alt: false,
            shift: false,
            key: Key::F(0),
        };
        let mut parts = text.split('+').collect::<Vec<_>>();
        // `Primary++` names the plus key itself.
        if text.ends_with("++") {
            parts.truncate(parts.len() - 2);
            parts.push("+");
        }
        let (key, modifiers) = parts
            .split_last()
            .filter(|(key, _)| !key.is_empty())
            .ok_or_else(|| format!("`{text}`: missing key"))?;
        for modifier in modifiers {
            let flag = match modifier.to_ascii_lowercase().as_str() {
                "primary" | "cmd" | "command" => &mut chord.primary,
                "ctrl" | "control" => &mut chord.control,
                "alt" | "option" => &mut chord.alt,
                "shift" => &mut chord.shift,
                _ => return Err(format!("`{text}`: unknown modifier `{modifier}`")),
            };
            if std::mem::replace(flag, true) {
                return Err(format!("`{text}`: `{modifier}` is repeated"));
            }
        }
        let mut chars = key.chars();
        chord.key = match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_graphic() => Key::Char(c.to_ascii_lowercase()),
            _ => match key
                .strip_prefix(['F', 'f'])
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=24).contains(n))
            {
                Some(n) => Key::F(n),
                None => return Err(format!("`{text}`: unknown key `{key}`")),
            },
        };
        Ok(chord)
    }

    /// The same press with `Primary` resolved for this platform.
    pub fn normalized(self) -> Self {
        if cfg!(target_os = "macos") || !self.primary {
            self
        } else {
            Self {
                primary: false,
                control: true,
                ..self
            }
        }
    }

    /// The display form used by built-in shortcuts, such as `Primary+Alt+L`.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        for (on, name) in [
            (self.primary, "Primary"),
            (self.control, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(match self.key {
            Key::Char(c) => c.to_ascii_uppercase().to_string(),
            Key::F(n) => format!("F{n}"),
        });
        parts.join("+")
    }

    /// Extensions get Alt combinations and function keys, with a letter,
    /// digit or F-key. Most built-in shortcuts use Primary without Alt, so
    /// this space rarely collides, and plain typing is never taken.
    pub fn check_for_extension(&self) -> Result<(), String> {
        match self.key {
            Key::F(_) => Ok(()),
            Key::Char(c) if !c.is_ascii_alphanumeric() => Err(format!(
                "`{}`: extensions may bind letters, digits or F1-F24",
                self.label()
            )),
            Key::Char(_) if !self.alt => Err(format!(
                "`{}`: extension shortcuts need Alt, or a function key",
                self.label()
            )),
            Key::Char(_) => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindingContribution {
    pub command: String,
    pub key: KeyChord,
}

/// Supplies the environment for an open folder, such as direnv's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentContribution {
    /// File names, one of which must be in the folder for the extension to
    /// be asked (`.envrc`).
    pub files: Vec<String>,
    /// File names whose save asks again (`.envrc`, `flake.nix`, …).
    pub watch: Vec<String>,
}

/// Formats whole documents of the given languages, on request or on save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatterContribution {
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contributions {
    pub commands: Vec<CommandContribution>,
    pub views: Vec<ViewContribution>,
    /// Paths relative to the extension folder.
    pub themes: Vec<PathBuf>,
    pub languages: Vec<LanguageContribution>,
    pub language_servers: Vec<LanguageServerContribution>,
    pub keybindings: Vec<KeybindingContribution>,
    pub formatters: Vec<FormatterContribution>,
    pub environment: Option<EnvironmentContribution>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Path relative to the extension folder. Not executed yet.
    pub wasm: Option<PathBuf>,
    pub activation: Activation,
    pub capabilities: Capabilities,
    pub contributes: Contributions,
}

type Table = toml::map::Map<String, toml::Value>;

fn reject_unknown(table: &Table, context: &str, allowed: &[&str]) -> Result<(), String> {
    match table.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("{context}: unknown key `{key}`")),
        None => Ok(()),
    }
}

fn sub_table<'a>(table: &'a Table, key: &str) -> Result<Option<&'a Table>, String> {
    table
        .get(key)
        .map(|v| {
            v.as_table()
                .ok_or_else(|| format!("`{key}` must be a table"))
        })
        .transpose()
}

fn string(table: &Table, key: &str, context: &str) -> Result<String, String> {
    optional_string(table, key, context)?.ok_or_else(|| format!("{context}: `{key}` is required"))
}

fn optional_string(table: &Table, key: &str, context: &str) -> Result<Option<String>, String> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{context}: `{key}` must be a string"))?;
    if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
        return Err(format!(
            "{context}: `{key}` must be 1-256 printable characters"
        ));
    }
    Ok(Some(text.into()))
}

fn strings(table: &Table, key: &str, context: &str) -> Result<Vec<String>, String> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("{context}: `{key}` must be a list of strings"))?;
    if items.len() > MAX_LIST {
        return Err(format!(
            "{context}: `{key}` has more than {MAX_LIST} entries"
        ));
    }
    items
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                .map(String::from)
                .ok_or_else(|| format!("{context}: `{key}` must contain non-empty strings"))
        })
        .collect()
}

fn tables<'a>(table: &'a Table, key: &str) -> Result<Vec<&'a Table>, String> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("`contributes.{key}` must be an array of tables"))?;
    if items.len() > MAX_LIST {
        return Err(format!(
            "`contributes.{key}` has more than {MAX_LIST} entries"
        ));
    }
    items
        .iter()
        .map(|item| {
            item.as_table()
                .ok_or_else(|| format!("`contributes.{key}` must be an array of tables"))
        })
        .collect()
}

/// Lowercase ASCII words joined by `.` or `-`, such as `example.todo-highlight`.
pub fn valid_id(id: &str) -> bool {
    id.len() <= 64
        && id.split(['.', '-']).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn valid_version(version: &str) -> bool {
    version.len() <= 32
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// A bare executable name: no path, no shell syntax.
pub fn valid_program(program: &str) -> bool {
    program.len() <= 128
        && !matches!(program, "." | "..")
        && program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// A relative path that stays inside its base by construction.
fn relative_path(text: &str, context: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        && path.components().next().is_some()
    {
        Ok(path)
    } else {
        Err(format!(
            "{context}: `{text}` must be a relative path inside the extension"
        ))
    }
}

pub fn parse_manifest(source: &str) -> Result<Manifest, String> {
    if source.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("Manifest exceeds 64 KiB".into());
    }
    let root: Table = source
        .parse()
        .map_err(|e| format!("Invalid manifest TOML: {e}"))?;
    reject_unknown(
        &root,
        "manifest",
        &["extension", "activation", "capabilities", "contributes"],
    )?;

    let extension = sub_table(&root, "extension")?.ok_or("`[extension]` is required")?;
    reject_unknown(
        extension,
        "[extension]",
        &["id", "name", "version", "api", "wasm"],
    )?;
    let id = string(extension, "id", "[extension]")?;
    if !valid_id(&id) {
        return Err(format!(
            "[extension]: id `{id}` must be lowercase words joined by `.` or `-`"
        ));
    }
    let name = string(extension, "name", "[extension]")?;
    let version = string(extension, "version", "[extension]")?;
    if !valid_version(&version) {
        return Err(format!("[extension]: invalid version `{version}`"));
    }
    match extension.get("api").map(toml::Value::as_integer) {
        None => return Err("[extension]: `api` is required".into()),
        Some(Some(API_VERSION)) => {}
        Some(Some(api)) => {
            return Err(format!(
                "Requires extension API {api}; this build provides {API_VERSION}"
            ));
        }
        Some(None) => return Err("[extension]: `api` must be an integer".into()),
    }
    let wasm = optional_string(extension, "wasm", "[extension]")?
        .map(|path| relative_path(&path, "[extension] wasm"))
        .transpose()?;

    let mut activation = Activation::default();
    if let Some(table) = sub_table(&root, "activation")? {
        reject_unknown(table, "[activation]", &["languages", "commands"])?;
        activation.languages = strings(table, "languages", "[activation]")?;
        activation.commands = strings(table, "commands", "[activation]")?;
    }

    let mut capabilities = Capabilities::default();
    if let Some(table) = sub_table(&root, "capabilities")? {
        reject_unknown(
            table,
            "[capabilities]",
            &["workspace", "process", "network", "clock", "environment"],
        )?;
        capabilities.workspace =
            match optional_string(table, "workspace", "[capabilities]")?.as_deref() {
                None | Some("none") => WorkspaceAccess::None,
                Some("read") => WorkspaceAccess::Read,
                Some("read-write") => WorkspaceAccess::ReadWrite,
                Some(other) => {
                    return Err(format!(
                        "[capabilities]: workspace `{other}` must be none, read or read-write"
                    ));
                }
            };
        capabilities.process = strings(table, "process", "[capabilities]")?;
        if let Some(bad) = capabilities.process.iter().find(|p| !valid_program(p)) {
            return Err(format!(
                "[capabilities]: process `{bad}` must be a bare executable name"
            ));
        }
        capabilities.network = strings(table, "network", "[capabilities]")?;
        if let Some(bad) = capabilities.network.iter().find(|h| !valid_host(h)) {
            return Err(format!(
                "[capabilities]: network `{bad}` must be a lowercase host name"
            ));
        }
        capabilities.clock = match table.get("clock") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("[capabilities]: `clock` must be true or false")?,
        };
        capabilities.environment = match table.get("environment") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("[capabilities]: `environment` must be true or false")?,
        };
    }

    let mut contributes = Contributions::default();
    if let Some(table) = sub_table(&root, "contributes")? {
        reject_unknown(
            table,
            "[contributes]",
            &[
                "commands",
                "views",
                "themes",
                "languages",
                "language_servers",
                "keybindings",
                "formatters",
                "environment",
            ],
        )?;
        for item in tables(table, "commands")? {
            let context = "[[contributes.commands]]";
            reject_unknown(item, context, &["id", "title"])?;
            let id = string(item, "id", context)?;
            if !valid_id(&id) {
                return Err(format!("{context}: invalid command id `{id}`"));
            }
            let title = string(item, "title", context)?;
            contributes.commands.push(CommandContribution { id, title });
        }
        for item in tables(table, "views")? {
            let context = "[[contributes.views]]";
            reject_unknown(item, context, &["id", "title", "location"])?;
            let id = string(item, "id", context)?;
            if !valid_id(&id) {
                return Err(format!("{context}: invalid view id `{id}`"));
            }
            let title = string(item, "title", context)?;
            let location = match optional_string(item, "location", context)?.as_deref() {
                None | Some("side") => ViewLocation::Side,
                Some("bottom") => ViewLocation::Bottom,
                Some("tab") => ViewLocation::Tab,
                Some(other) => {
                    return Err(format!(
                        "{context}: location `{other}` must be side, bottom or tab"
                    ));
                }
            };
            contributes.views.push(ViewContribution {
                id,
                title,
                location,
            });
        }
        for item in tables(table, "themes")? {
            let context = "[[contributes.themes]]";
            reject_unknown(item, context, &["path"])?;
            let path = string(item, "path", context)?;
            contributes.themes.push(relative_path(&path, context)?);
        }
        for item in tables(table, "languages")? {
            let context = "[[contributes.languages]]";
            reject_unknown(item, context, &["id", "extensions"])?;
            let id = string(item, "id", context)?;
            if !valid_id(&id) {
                return Err(format!("{context}: invalid language id `{id}`"));
            }
            let extensions = strings(item, "extensions", context)?
                .into_iter()
                .map(|ext| {
                    let ext = ext.strip_prefix('.').unwrap_or(&ext).to_ascii_lowercase();
                    if ext.is_empty()
                        || !ext
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                    {
                        Err(format!("{context}: invalid file extension `{ext}`"))
                    } else {
                        Ok(ext)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            if extensions.is_empty() {
                return Err(format!("{context}: `extensions` must not be empty"));
            }
            contributes
                .languages
                .push(LanguageContribution { id, extensions });
        }
        for item in tables(table, "language_servers")? {
            let context = "[[contributes.language_servers]]";
            reject_unknown(item, context, &["language", "command", "args"])?;
            let language = string(item, "language", context)?;
            if !valid_id(&language) {
                return Err(format!("{context}: invalid language id `{language}`"));
            }
            let command = string(item, "command", context)?;
            // Launching a process is a capability: the manifest must say so.
            if !capabilities.process.contains(&command) {
                return Err(format!(
                    "{context}: `{command}` must also be listed in [capabilities] process"
                ));
            }
            let args = strings(item, "args", context)?;
            contributes
                .language_servers
                .push(LanguageServerContribution {
                    language,
                    command,
                    args,
                });
        }
        for item in tables(table, "keybindings")? {
            let context = "[[contributes.keybindings]]";
            reject_unknown(item, context, &["command", "key"])?;
            let command = string(item, "command", context)?;
            if !contributes.commands.iter().any(|c| c.id == command) {
                return Err(format!(
                    "{context}: `{command}` is not one of this extension's commands"
                ));
            }
            let key = KeyChord::parse(&string(item, "key", context)?)
                .map_err(|e| format!("{context}: {e}"))?;
            key.check_for_extension()
                .map_err(|e| format!("{context}: {e}"))?;
            contributes
                .keybindings
                .push(KeybindingContribution { command, key });
        }
        for item in tables(table, "formatters")? {
            let context = "[[contributes.formatters]]";
            reject_unknown(item, context, &["languages"])?;
            let languages = strings(item, "languages", context)?;
            if languages.is_empty() {
                return Err(format!("{context}: `languages` must not be empty"));
            }
            if let Some(bad) = languages.iter().find(|l| !valid_id(l)) {
                return Err(format!("{context}: invalid language id `{bad}`"));
            }
            contributes
                .formatters
                .push(FormatterContribution { languages });
        }
        if let Some(item) = sub_table(table, "environment")? {
            let context = "[contributes.environment]";
            reject_unknown(item, context, &["files", "watch"])?;
            let files = strings(item, "files", context)?;
            let mut watch = strings(item, "watch", context)?;
            if files.is_empty() {
                return Err(format!("{context}: `files` must not be empty"));
            }
            let plain = |name: &String| {
                !name.is_empty() && !name.contains(['/', '\\', '\0']) && name != "." && name != ".."
            };
            if let Some(bad) = files.iter().chain(&watch).find(|name| !plain(name)) {
                return Err(format!("{context}: `{bad}` must be a file name"));
            }
            if !capabilities.environment {
                return Err(format!(
                    "{context} needs `environment = true` in [capabilities]"
                ));
            }
            for file in &files {
                if !watch.contains(file) {
                    watch.push(file.clone());
                }
            }
            contributes.environment = Some(EnvironmentContribution { files, watch });
        }
        if wasm.is_none()
            && !(contributes.keybindings.is_empty()
                && contributes.formatters.is_empty()
                && contributes.environment.is_none())
        {
            return Err(
                "keybindings, formatters and environments need a `wasm` module in [extension]"
                    .into(),
            );
        }
    }

    Ok(Manifest {
        id,
        name,
        version,
        wasm,
        activation,
        capabilities,
        contributes,
    })
}

#[derive(Clone, Debug)]
pub struct Extension {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub themes: Vec<Theme>,
    /// Resolved module path, checked to stay inside `root`.
    pub wasm: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub extensions: Vec<Extension>,
    /// Folder and reason for each extension that was skipped.
    pub errors: Vec<(PathBuf, String)>,
}

fn read_limited(path: &Path, limit: u64) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    if metadata.len() > limit {
        return Err(format!("{}: exceeds {} KiB", path.display(), limit / 1024));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Resolves `relative` under `root`, rejecting symbolic links that leave it.
fn contained(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", relative.display()))?;
    if path.starts_with(root) {
        Ok(path)
    } else {
        Err(format!(
            "{}: leaves the extension folder",
            relative.display()
        ))
    }
}

fn load_extension(folder: &Path) -> Result<Extension, String> {
    let root = folder.canonicalize().map_err(|e| e.to_string())?;
    let manifest = parse_manifest(&read_limited(
        &root.join(MANIFEST_FILE),
        MAX_MANIFEST_BYTES,
    )?)?;
    let wasm = manifest
        .wasm
        .as_ref()
        .map(|wasm| contained(&root, wasm))
        .transpose()?;
    let themes = manifest
        .contributes
        .themes
        .iter()
        .map(|relative| {
            let path = contained(&root, relative)?;
            crate::theme::parse_theme(&read_limited(&path, 64 * 1024)?)
                .map_err(|e| format!("{}: {e}", relative.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Extension {
        root,
        manifest,
        themes,
        wasm,
    })
}

impl Registry {
    /// Loads every `<dir>/<folder>/extension.toml`. A missing directory is empty.
    pub fn load(dir: &Path) -> Self {
        let mut registry = Self::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return registry,
            Err(e) => {
                registry.errors.push((dir.into(), e.to_string()));
                return registry;
            }
        };
        let mut folders = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && !path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with('.'))
            })
            .collect::<Vec<_>>();
        folders.sort();
        if folders.len() > MAX_EXTENSIONS {
            registry.errors.push((
                dir.into(),
                format!("Only the first {MAX_EXTENSIONS} extensions are loaded"),
            ));
            folders.truncate(MAX_EXTENSIONS);
        }
        for folder in folders {
            match load_extension(&folder) {
                Ok(extension) => {
                    if registry
                        .extensions
                        .iter()
                        .any(|e| e.manifest.id == extension.manifest.id)
                    {
                        registry.errors.push((
                            folder,
                            format!("Duplicate extension id `{}`", extension.manifest.id),
                        ));
                    } else {
                        registry.extensions.push(extension);
                    }
                }
                Err(e) => registry.errors.push((folder, e)),
            }
        }
        registry
    }

    /// Installed extensions the user has not disabled, in load order.
    /// Every contribution below comes from these only.
    pub fn enabled(&self) -> impl Iterator<Item = &Extension> {
        self.extensions
            .iter()
            .filter(|extension| is_enabled(&extension.manifest.id))
    }

    /// Contributed themes in load order, as (extension id, theme).
    pub fn themes(&self) -> impl Iterator<Item = (&str, &Theme)> {
        self.enabled().flat_map(|extension| {
            extension
                .themes
                .iter()
                .map(|theme| (extension.manifest.id.as_str(), theme))
        })
    }

    /// The first contributed theme whose name matches, ignoring ASCII case.
    pub fn theme(&self, name: &str) -> Option<Theme> {
        self.themes()
            .find(|(_, theme)| theme.name().eq_ignore_ascii_case(name))
            .map(|(_, theme)| theme.clone())
    }

    /// Language id for a lowercase file extension, first contribution wins.
    pub fn language_for_extension(&self, extension: &str) -> Option<&str> {
        self.enabled()
            .flat_map(|e| &e.manifest.contributes.languages)
            .find(|language| language.extensions.iter().any(|ext| ext == extension))
            .map(|language| language.id.as_str())
    }

    /// Extensions able to format `language`, in load order.
    pub fn formatters(&self, language: &str) -> impl Iterator<Item = &Extension> {
        self.enabled().filter(move |e| {
            e.wasm.is_some()
                && e.manifest
                    .contributes
                    .formatters
                    .iter()
                    .any(|f| f.languages.iter().any(|l| l == language))
        })
    }

    /// Enabled extensions that supply an environment for a folder.
    pub fn environment_providers(
        &self,
    ) -> impl Iterator<Item = (&Extension, &EnvironmentContribution)> {
        self.enabled().filter_map(|extension| {
            extension
                .manifest
                .contributes
                .environment
                .as_ref()
                .filter(|_| extension.wasm.is_some())
                .map(|contribution| (extension, contribution))
        })
    }

    /// Contributed keybindings as (extension, binding), in load order.
    pub fn keybindings(&self) -> impl Iterator<Item = (&Extension, &KeybindingContribution)> {
        self.enabled().flat_map(|extension| {
            extension
                .manifest
                .contributes
                .keybindings
                .iter()
                .map(move |binding| (extension, binding))
        })
    }

    /// Launch settings for a language, first contribution wins.
    pub fn language_server(&self, language: &str) -> Option<&LanguageServerContribution> {
        self.language_server_entry(language)
            .map(|(_, server)| server)
    }

    /// The language server for `language` with the extension that provides it.
    pub fn language_server_entry(
        &self,
        language: &str,
    ) -> Option<(&Extension, &LanguageServerContribution)> {
        self.enabled().find_map(|e| {
            e.manifest
                .contributes
                .language_servers
                .iter()
                .find(|server| server.language == language)
                .map(|server| (e, server))
        })
    }

    /// One-line summary for the status bar, or `None` when nothing failed.
    pub fn error_summary(&self) -> Option<String> {
        let (folder, reason) = self.errors.first()?;
        let name = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        Some(match self.errors.len() {
            1 => format!("Extension {name} was not loaded: {reason}"),
            n => format!("{n} extensions were not loaded; first: {name}: {reason}"),
        })
    }
}

/// `KUREHA_EXTENSIONS_DIR`, else `$XDG_CONFIG_HOME/kureha-code/extensions`.
pub fn extensions_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KUREHA_EXTENSIONS_DIR").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    Some(root.join("kureha-code/extensions"))
}

/// Extensions the user turned off (`[extensions] disabled` in the settings).
static DISABLED: std::sync::RwLock<std::collections::BTreeSet<String>> =
    std::sync::RwLock::new(std::collections::BTreeSet::new());
/// Bumped whenever [`DISABLED`] changes, for caches built from contributions.
static DISABLED_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether the extension `id` may contribute and run.
pub fn is_enabled(id: &str) -> bool {
    !DISABLED.read().unwrap().contains(id)
}

/// Replaces the disabled set; returns whether it changed.
pub fn set_disabled(ids: &std::collections::BTreeSet<String>) -> bool {
    let mut disabled = DISABLED.write().unwrap();
    if *disabled == *ids {
        return false;
    }
    *disabled = ids.clone();
    DISABLED_REVISION.fetch_add(1, Ordering::Relaxed);
    true
}

/// Changes whenever an extension is turned on or off.
pub fn disabled_revision() -> u64 {
    DISABLED_REVISION.load(Ordering::Relaxed)
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static REPORTED: AtomicBool = AtomicBool::new(false);

/// The installed extensions, loading them on first use. Reads files, so call
/// it from worker threads; the UI thread should use [`loaded`].
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        extensions_dir()
            .map(|dir| Registry::load(&dir))
            .unwrap_or_default()
    })
}

/// The registry if loading has finished, without blocking.
pub fn loaded() -> Option<&'static Registry> {
    REGISTRY.get()
}

/// Starts loading on a worker so later lookups do not touch the disk.
pub fn preload() {
    if REGISTRY.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("extensions".into())
            .spawn(|| {
                registry();
            });
    }
}

/// Returns load problems once, after loading finished.
pub fn take_load_report() -> Option<String> {
    let registry = loaded()?;
    if REPORTED.swap(true, Ordering::Relaxed) {
        return None;
    }
    registry.error_summary()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[extension]
id = "example.todo-highlight"
name = "TODO Highlight"
version = "0.1.0"
api = 1
wasm = "extension.wasm"

[activation]
languages = ["rust"]
commands = ["todo.list"]

[capabilities]
workspace = "read"
process = ["rust-analyzer"]
network = ["example.com"]
clock = true

[[contributes.commands]]
id = "todo.list"
title = "List TODOs"

[[contributes.views]]
id = "todo.panel"
title = "TODO"
location = "bottom"

[[contributes.themes]]
path = "themes/dark.toml"

[[contributes.languages]]
id = "zig"
extensions = [".ZIG", "zon"]

[[contributes.language_servers]]
language = "rust"
command = "rust-analyzer"
args = ["--log", "off"]
"#;

    #[test]
    fn parses_every_section() {
        let m = parse_manifest(FULL).unwrap();
        assert_eq!(m.id, "example.todo-highlight");
        assert_eq!(m.wasm, Some(PathBuf::from("extension.wasm")));
        assert_eq!(m.activation.commands, ["todo.list"]);
        assert_eq!(m.capabilities.workspace, WorkspaceAccess::Read);
        assert!(m.capabilities.clock);
        assert_eq!(m.contributes.views[0].location, ViewLocation::Bottom);
        assert_eq!(m.contributes.themes, [PathBuf::from("themes/dark.toml")]);
        assert_eq!(m.contributes.languages[0].extensions, ["zig", "zon"]);
        assert_eq!(m.contributes.language_servers[0].args, ["--log", "off"]);
    }

    #[test]
    fn key_chords_parse_and_limit_extensions() {
        let chord = KeyChord::parse("Primary+Alt+l").unwrap();
        assert_eq!(chord.key, Key::Char('l'));
        assert!(chord.primary && chord.alt && !chord.shift);
        assert_eq!(chord.label(), "Primary+Alt+L");
        assert_eq!(KeyChord::parse("Shift+F12").unwrap().key, Key::F(12));
        assert_eq!(
            KeyChord::parse("Ctrl+Shift+\\").unwrap().key,
            Key::Char('\\')
        );
        for bad in [
            "",
            "Alt+",
            "Hyper+X",
            "Alt+Alt+X",
            "Alt+Enter",
            "Primary+K Primary+T",
            "F25",
        ] {
            assert!(KeyChord::parse(bad).is_err(), "{bad}");
        }
        assert!(
            KeyChord::parse("Alt+Shift+S")
                .unwrap()
                .check_for_extension()
                .is_ok()
        );
        assert!(KeyChord::parse("F6").unwrap().check_for_extension().is_ok());
        for refused in ["Primary+S", "Shift+X", "Alt+/", "Ctrl+Shift+P"] {
            assert!(
                KeyChord::parse(refused)
                    .unwrap()
                    .check_for_extension()
                    .is_err(),
                "{refused}"
            );
        }
        let ctrl = KeyChord::parse("Ctrl+Alt+S").unwrap().normalized();
        let primary = KeyChord::parse("Primary+Alt+S").unwrap().normalized();
        assert_eq!(ctrl == primary, !cfg!(target_os = "macos"));
    }

    #[test]
    fn keybindings_and_formatters_are_validated() {
        let base = "[extension]\nid='a.b'\nname='A'\nversion='1'\napi=1\nwasm='a.wasm'\n\
                    [[contributes.commands]]\nid='a.run'\ntitle='Run'\n";
        let m = parse_manifest(&format!(
            "{base}[[contributes.keybindings]]\ncommand='a.run'\nkey='Alt+Shift+R'\n\
             [[contributes.formatters]]\nlanguages=['json']\n"
        ))
        .unwrap();
        assert_eq!(m.contributes.keybindings[0].key.label(), "Alt+Shift+R");
        assert_eq!(m.contributes.formatters[0].languages, ["json"]);
        for (extra, needle) in [
            (
                "[[contributes.keybindings]]\ncommand='a.other'\nkey='Alt+R'\n",
                "not one of",
            ),
            (
                "[[contributes.keybindings]]\ncommand='a.run'\nkey='Primary+R'\n",
                "need Alt",
            ),
            (
                "[[contributes.formatters]]\nlanguages=[]\n",
                "must not be empty",
            ),
            (
                "[[contributes.formatters]]\nlanguages=['C++']\n",
                "invalid language",
            ),
        ] {
            let err = parse_manifest(&format!("{base}{extra}")).unwrap_err();
            assert!(err.contains(needle), "{extra}: {err}");
        }
        let no_wasm = base.replace("wasm='a.wasm'\n", "");
        assert!(
            parse_manifest(&format!(
                "{no_wasm}[[contributes.formatters]]\nlanguages=['json']\n"
            ))
            .unwrap_err()
            .contains("need a `wasm` module")
        );
    }

    #[test]
    fn minimal_manifest_grants_nothing() {
        let m = parse_manifest("[extension]\nid='a.b'\nname='A'\nversion='1'\napi=1").unwrap();
        assert_eq!(m.capabilities, Capabilities::default());
        assert_eq!(m.contributes, Contributions::default());
    }

    #[test]
    fn rejects_invalid_manifests() {
        let base = "[extension]\nid='a.b'\nname='A'\nversion='1'\napi=1\n";
        for (extra, needle) in [
            ("[extra]\n", "unknown key"),
            ("[capabilities]\nworkspace='all'\n", "workspace"),
            ("[capabilities]\nprocess=['/bin/sh']\n", "bare executable"),
            ("[capabilities]\nnetwork=['Example.COM']\n", "host name"),
            (
                "[[contributes.themes]]\npath='../x.toml'\n",
                "relative path",
            ),
            (
                "[[contributes.themes]]\npath='/etc/x.toml'\n",
                "relative path",
            ),
            (
                "[[contributes.views]]\nid='v'\ntitle='V'\nlocation='float'\n",
                "location",
            ),
            (
                "[[contributes.languages]]\nid='zig'\nextensions=[]\n",
                "must not be empty",
            ),
            (
                "[[contributes.language_servers]]\nlanguage='zig'\ncommand='zls'\n",
                "[capabilities] process",
            ),
        ] {
            let err = parse_manifest(&format!("{base}{extra}")).unwrap_err();
            assert!(err.contains(needle), "{extra}: {err}");
        }
        assert!(
            parse_manifest(&base.replace("api=1", "api=2"))
                .unwrap_err()
                .contains("API 2")
        );
        assert!(parse_manifest(&base.replace("a.b", "A.B")).is_err());
        assert!(parse_manifest(&base.replace("version='1'", "version='v1'")).is_err());
        assert!(parse_manifest("[extension]\nid='a'\nname='A'\nversion='1'").is_err());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kureha-extensions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn registry_loads_contributions_and_reports_failures() {
        let dir = temp_dir("registry");
        write(
            &dir.join("good/extension.toml"),
            "[extension]\nid='ex.good'\nname='Good'\nversion='1.0.0'\napi=1\n\
             [capabilities]\nprocess=['zls']\n\
             [[contributes.themes]]\npath='plum.toml'\n\
             [[contributes.languages]]\nid='zig'\nextensions=['zig']\n\
             [[contributes.language_servers]]\nlanguage='zig'\ncommand='zls'\n",
        );
        write(
            &dir.join("good/plum.toml"),
            "name='Plum Extension'\nbase='dark'\n",
        );
        write(
            &dir.join("same/extension.toml"),
            "[extension]\nid='ex.good'\nname='Again'\nversion='1'\napi=1\n",
        );
        write(&dir.join("broken/extension.toml"), "[extension]\nid=1\n");
        write(&dir.join(".hidden/extension.toml"), "not toml at all");
        std::fs::create_dir_all(dir.join("empty")).unwrap();

        let registry = Registry::load(&dir);
        assert_eq!(registry.extensions.len(), 1);
        assert_eq!(registry.errors.len(), 3, "{:?}", registry.errors);
        assert!(
            registry
                .errors
                .iter()
                .any(|(_, e)| e.contains("Duplicate extension id"))
        );
        assert_eq!(
            registry.theme("plum extension").unwrap().name(),
            "Plum Extension"
        );
        assert_eq!(registry.language_for_extension("zig"), Some("zig"));
        assert_eq!(registry.language_server("zig").unwrap().command, "zls");
        assert!(
            registry
                .error_summary()
                .unwrap()
                .starts_with("3 extensions")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_cannot_leave_the_extension() {
        let dir = temp_dir("symlink");
        write(&dir.join("outside.toml"), "name='Outside'\n");
        write(
            &dir.join("exts/sneaky/extension.toml"),
            "[extension]\nid='ex.sneaky'\nname='S'\nversion='1'\napi=1\n\
             [[contributes.themes]]\npath='theme.toml'\n",
        );
        std::os::unix::fs::symlink(dir.join("outside.toml"), dir.join("exts/sneaky/theme.toml"))
            .unwrap();
        let registry = Registry::load(&dir.join("exts"));
        assert!(registry.extensions.is_empty());
        assert!(registry.errors[0].1.contains("leaves the extension folder"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_directory_is_empty() {
        let registry = Registry::load(Path::new("/nonexistent/kureha/extensions"));
        assert!(registry.extensions.is_empty() && registry.errors.is_empty());
        assert_eq!(registry.error_summary(), None);
    }
}
