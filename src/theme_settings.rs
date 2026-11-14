//! Theme I/O and parsing stay off the event loop. Invalid saves retain the last palette.
use super::preferences::{Change, Preferences};
use kureha_code::theme::{THEME_TEMPLATE, Theme, parse_theme};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::{Mutex, mpsc},
    thread,
    time::Duration,
};

pub(super) enum Update {
    Loaded(Theme, Preferences),
    Saved(Theme, Preferences),
    Open(PathBuf),
    Error(String),
}

enum Command {
    Reload,
    Open,
    OpenSettings,
    Save(Change),
    Shutdown,
}

pub(super) struct Watcher {
    commands: mpsc::Sender<Command>,
    updates: Mutex<mpsc::Receiver<Update>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let (sender, updates) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("kureha-theme".into())
            .spawn(move || {
                let settings_path = path.with_file_name("settings.toml");
                let mut previous = None;
                let mut force = false;
                let mut saved = false;
                loop {
                    let result = (read(&path), read(&settings_path));
                    if force || previous.as_ref() != Some(&result) {
                        force = false;
                        let from_save = std::mem::take(&mut saved);
                        let loaded = (|| {
                            let source = result.0.as_ref().map_err(Clone::clone)?;
                            let settings = result.1.as_ref().map_err(Clone::clone)?;
                            let prefs = Preferences::parse(settings.as_deref().unwrap_or(""))?;
                            let custom = match source {
                                _ if prefs.color_theme.is_some() => {
                                    kureha_code::theme::Theme::default()
                                }
                                Some(source) => parse_theme(source)?.prepare_font(
                                    path.parent().unwrap_or(std::path::Path::new(".")),
                                )?,
                                None => kureha_code::theme::Theme::default(),
                            };
                            let theme = prefs.apply(custom)?;
                            Ok::<_, String>(if from_save {
                                Update::Saved(theme, prefs)
                            } else {
                                Update::Loaded(theme, prefs)
                            })
                        })();
                        previous = Some(result);
                        if sender
                            .send(
                                loaded.unwrap_or_else(|e| Update::Error(format!("Settings: {e}"))),
                            )
                            .is_err()
                        {
                            break;
                        }
                    }
                    match receiver.recv_timeout(Duration::from_millis(500)) {
                        Ok(Command::Reload) => force = true,
                        Ok(Command::Shutdown) => break,
                        Ok(Command::Open) => {
                            let result = create(&path)
                                .map(|()| Update::Open(path.clone()))
                                .unwrap_or_else(|e| Update::Error(format!("Theme: {e}")));
                            if sender.send(result).is_err() {
                                break;
                            }
                        }
                        Ok(Command::OpenSettings) => {
                            let result = create_settings(&settings_path)
                                .map(|()| Update::Open(settings_path.clone()))
                                .unwrap_or_else(|e| Update::Error(format!("Settings: {e}")));
                            if sender.send(result).is_err() {
                                break;
                            }
                        }
                        Ok(Command::Save(change)) => match save(&settings_path, &change) {
                            Ok(()) => {
                                force = true;
                                saved = true;
                            }
                            Err(e) => {
                                if sender
                                    .send(Update::Error(format!("Cannot save settings: {e}")))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        },
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self {
            commands,
            updates: Mutex::new(updates),
            worker: Some(worker),
        })
    }

    pub fn open(&self) {
        let _ = self.commands.send(Command::Open);
    }

    pub fn open_settings(&self) {
        let _ = self.commands.send(Command::OpenSettings);
    }

    pub fn save(&self, change: Change) {
        let _ = self.commands.send(Command::Save(change));
    }

    pub fn reload(&self) {
        let _ = self.commands.send(Command::Reload);
    }

    pub fn poll(&self) -> Vec<Update> {
        self.updates.lock().unwrap().try_iter().collect()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read(path: &std::path::Path) -> Result<Option<String>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Theme exceeds 64 KiB".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn create(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(THEME_TEMPLATE.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

const SETTINGS_TEMPLATE: &str = "# Theme selections and editor preferences. Changes apply automatically.\n# Omit color_theme to use theme.toml; omit code_theme to follow it.\n# color_theme = \"kureha-light\"\n# code_theme = \"monokai\"\n# font_size = 14\nhide_minimap = false\nword_wrap = true\nwrap_at_words = true\nformat_on_save = false\nindent_depth_background = false\nactive_block_guide = false\ninline_type_hints = false\ninsert_spaces = true\ntab_size = 4\n# Pick a formatter per language: \"lsp\" or an extension id.\n# [formatter]\n# json = \"lsp\"\n";

fn create_settings(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(SETTINGS_TEMPLATE.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Adds `extension` to (or, with `listed` false, removes it from) the list
/// `[extensions] <key>`, dropping empty lists and tables.
fn set_listed(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    extension: &str,
    listed: bool,
) -> Result<(), String> {
    let extensions = document
        .entry("extensions")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .ok_or("extensions must be a table")?;
    extensions.set_implicit(true);
    let ids = extensions
        .entry(key)
        .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .ok_or_else(|| format!("extensions.{key} must be a list"))?;
    ids.retain(|id| id.as_str() != Some(extension));
    if listed {
        ids.push(extension);
    }
    if ids.is_empty() {
        extensions.remove(key);
    }
    if extensions.is_empty() {
        document.remove("extensions");
    }
    Ok(())
}

fn save(path: &std::path::Path, change: &Change) -> Result<(), String> {
    let resolved = if path.is_symlink() {
        Some(path.canonicalize().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let path = resolved.as_deref().unwrap_or(path);
    let original = read(path)?;
    let source = original.as_deref().unwrap_or(SETTINGS_TEMPLATE);
    Preferences::parse(source)?; // Do not overwrite malformed user settings.
    let mut document: toml_edit::DocumentMut = source.parse().map_err(|e| format!("{e}"))?;
    match change {
        Change::Color(name) | Change::Code(name) => {
            let key = if matches!(change, Change::Color(_)) {
                "color_theme"
            } else {
                "code_theme"
            };
            if let Some(name) = name {
                document[key] = toml_edit::value(name.clone());
            } else {
                document.remove(key);
            }
            if matches!(change, Change::Color(_)) {
                document.remove("code_theme");
            }
        }
        Change::FontSize(size) => {
            if let Some(size) = size {
                document["font_size"] = toml_edit::value(*size as f64);
            } else {
                document.remove("font_size");
            }
        }
        Change::Minimap(visible) => document["hide_minimap"] = toml_edit::value(!visible),
        Change::WordWrap(enabled) => document["word_wrap"] = toml_edit::value(*enabled),
        Change::WrapAtWords(enabled) => document["wrap_at_words"] = toml_edit::value(*enabled),
        Change::IndentDepthBackground(enabled) => {
            document["indent_depth_background"] = toml_edit::value(*enabled)
        }
        Change::ActiveBlockGuide(enabled) => {
            document["active_block_guide"] = toml_edit::value(*enabled)
        }
        Change::InlineTypeHints(enabled) => {
            document["inline_type_hints"] = toml_edit::value(*enabled)
        }
        Change::InsertSpaces(enabled) => document["insert_spaces"] = toml_edit::value(*enabled),
        Change::TabSize(size) => {
            document["tab_size"] = toml_edit::value((*size).clamp(1, 8) as i64)
        }
        Change::FormatOnSave(enabled) => document["format_on_save"] = toml_edit::value(*enabled),
        Change::AllowProcess {
            extension,
            program,
            allowed,
        } => {
            let extensions = document
                .entry("extensions")
                .or_insert_with(toml_edit::table)
                .as_table_mut()
                .ok_or("extensions must be a table")?;
            extensions.set_implicit(true);
            let grants = extensions
                .entry("allow_process")
                .or_insert_with(toml_edit::table)
                .as_table_mut()
                .ok_or("extensions.allow_process must be a table")?;
            let programs = grants
                .entry(extension)
                .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
                .as_array_mut()
                .ok_or("extensions.allow_process entries must be lists")?;
            programs.retain(|p| p.as_str() != Some(program.as_str()));
            if *allowed {
                programs.push(program.as_str());
            }
            if programs.is_empty() {
                grants.remove(extension);
            }
            if grants.is_empty() {
                extensions.remove("allow_process");
            }
            if extensions.is_empty() {
                document.remove("extensions");
            }
        }
        Change::ExtensionEnabled { extension, enabled } => {
            set_listed(&mut document, "disabled", extension, !*enabled)?;
        }
        Change::AllowEnvironment { extension, allowed } => {
            set_listed(&mut document, "allow_environment", extension, *allowed)?;
        }
    }
    let updated = document.to_string();
    Preferences::parse(&updated)?;
    if read(path)? != original {
        return Err("settings changed externally; try again".into());
    }
    let parent = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(
        ".settings-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        if read(path).map_err(std::io::Error::other)? != original {
            return Err(std::io::Error::other(
                "settings changed externally; try again",
            ));
        }
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|e| e.to_string())
}

impl super::App {
    pub(super) fn start_theme_settings(&self) {
        let mut watcher = self.theme_settings.lock().unwrap();
        if watcher.is_some() {
            return;
        }
        let Some(path) = kureha_code::theme::theme_path() else {
            self.status
                .set("Cannot locate theme file; set KUREHA_THEME".into());
            return;
        };
        match Watcher::start(path) {
            Ok(value) => *watcher = Some(value),
            Err(e) => self.status.set(format!("Cannot watch theme: {e}")),
        }
    }

    pub(super) fn open_theme_settings(&self) {
        self.start_theme_settings();
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.open();
        }
    }

    pub(super) fn open_preferences_file(&self) {
        self.start_theme_settings();
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.open_settings();
        }
    }

    pub(super) fn change_preferences(&self, change: Change) {
        self.preferences.update(|p| p.change(&change));
        let preferences = self.preferences.get();
        self.sync_process_grants(&preferences);
        self.settings_ui.sync_preferences(&preferences);
        if self.minimap_visible.get() == preferences.hide_minimap {
            self.minimap_visible.set(!preferences.hide_minimap);
        }
        // Production starts the worker before its event loop. Tests without a
        // worker must never write into the developer's real configuration.
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.save(change);
        }
    }

    pub(super) fn reload_theme_settings(&self) {
        self.start_theme_settings();
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.reload();
        }
    }

    pub(super) fn poll_theme_settings(&self) {
        let updates = self
            .theme_settings
            .lock()
            .unwrap()
            .as_ref()
            .map(Watcher::poll)
            .unwrap_or_default();
        for update in updates {
            self.apply_settings_update(update);
        }
        if !self.nav.is_open() {
            let deferred = self.runtime.lock().unwrap().deferred_preferences.take();
            if let Some((theme, preferences)) = deferred {
                self.apply_settings_update(Update::Saved(theme, preferences));
            }
        }
    }

    fn apply_settings_update(&self, update: Update) {
        let (theme, preferences, from_save) = match update {
            Update::Loaded(theme, preferences) => (theme, preferences, false),
            Update::Saved(theme, preferences) => (theme, preferences, true),
            Update::Open(path) => {
                self.open(path);
                return;
            }
            Update::Error(error) => {
                self.status.set(error);
                return;
            }
        };
        // A slow save must not undo a newer choice or close a live-preview picker.
        if from_save {
            if self.preferences.get() != preferences {
                return;
            }
            if self.nav.is_open() {
                self.runtime.lock().unwrap().deferred_preferences = Some((theme, preferences));
                return;
            }
        }
        self.runtime.lock().unwrap().deferred_preferences = None;
        self.minimap_visible.set(!preferences.hide_minimap);
        self.settings_ui.sync_preferences(&preferences);
        self.sync_process_grants(&preferences);
        self.preferences.set(preferences);
        self.set_theme(theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_recovers_from_invalid_and_atomic_saves_without_overwriting_settings() {
        let dir = std::env::temp_dir().join(format!("kureha-theme-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.toml");
        fs::write(&path, "name='First'\nbase='light'").unwrap();
        let watcher = Watcher::start(path.clone()).unwrap();

        fn next(w: &Watcher) -> Update {
            w.updates
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        }

        assert!(matches!(next(&watcher), Update::Loaded(t, _) if t.name() == "First"));
        fs::write(&path, "[broken").unwrap();
        watcher.reload();
        assert!(matches!(next(&watcher), Update::Error(_)));
        fs::write(
            dir.join("next"),
            "name='Second'\n[colors]\naccent='#123456'",
        )
        .unwrap();
        fs::rename(dir.join("next"), &path).unwrap();
        // No explicit reload: saving a replacement file must be observed.
        assert!(matches!(next(&watcher), Update::Loaded(t, _) if t.name() == "Second"));
        create(&path).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("Second"));
        drop(watcher);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "kureha-preferences-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn theme(&self) -> PathBuf {
            self.0.join("theme.toml")
        }

        fn settings(&self) -> PathBuf {
            self.0.join("settings.toml")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn next(w: &Watcher) -> Update {
        match w
            .updates
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            Update::Saved(theme, prefs) => Update::Loaded(theme, prefs),
            update => update,
        }
    }

    #[test]
    fn choices_are_saved_restored_and_preserve_comments_and_custom_theme() {
        let f = Fixture::new();
        let custom = "name='Mine'\nbase='kureha'\n[code]\nfont_size=17\n";
        fs::write(f.theme(), custom).unwrap();
        fs::write(f.settings(), "# keep my explanation\nhide_minimap=false\n").unwrap();
        let watcher = Watcher::start(f.theme()).unwrap();
        assert!(matches!(next(&watcher),Update::Loaded(t,_) if t.name()=="Mine"));
        watcher.save(Change::Color(Some("Monokai".into())));
        assert!(matches!(next(&watcher),Update::Loaded(t,_) if t.name()=="Monokai"));
        watcher.save(Change::Code(Some("Nord".into())));
        assert!(
            matches!(next(&watcher),Update::Loaded(t,_) if t.colors()==kureha_code::theme::preset("monokai").unwrap().colors() && t.editor_colors()==kureha_code::theme::preset("nord").unwrap().editor_colors())
        );
        watcher.save(Change::FontSize(Some(19.0)));
        assert!(matches!(next(&watcher),Update::Loaded(t,_) if t.editor_font_size()==19.0));
        watcher.save(Change::Minimap(false));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if p.hide_minimap));
        watcher.save(Change::WrapAtWords(false));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if !p.wrap_at_words));
        watcher.save(Change::IndentDepthBackground(true));
        assert!(
            matches!(next(&watcher),Update::Loaded(_,p) if p.indent_depth_background && !p.active_block_guide)
        );
        watcher.save(Change::ActiveBlockGuide(true));
        assert!(
            matches!(next(&watcher),Update::Loaded(_,p) if p.indent_depth_background && p.active_block_guide)
        );
        watcher.save(Change::InsertSpaces(false));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if !p.insert_spaces));
        watcher.save(Change::TabSize(2));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if p.tab_size == 2));
        watcher.save(Change::InlineTypeHints(true));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if p.inline_type_hints));
        watcher.save(Change::InlineTypeHints(false));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if !p.inline_type_hints));
        watcher.save(Change::WordWrap(false));
        assert!(matches!(next(&watcher),Update::Loaded(_,p) if !p.word_wrap));
        drop(watcher);
        let restored = Watcher::start(f.theme()).unwrap();
        assert!(
            matches!(next(&restored),Update::Loaded(t,p) if t.editor_font_size()==19.0 && p.hide_minimap && !p.word_wrap && !p.wrap_at_words && p.indent_depth_background && p.active_block_guide && p.code_theme.as_deref()==Some("Nord"))
        );
        assert_eq!(fs::read_to_string(f.theme()).unwrap(), custom);
        assert!(
            fs::read_to_string(f.settings())
                .unwrap()
                .contains("# keep my explanation")
        );
        drop(restored);
    }

    #[test]
    fn invalid_settings_are_not_overwritten_and_atomic_edits_reload() {
        let f = Fixture::new();
        fs::write(f.settings(), "broken=[").unwrap();
        assert!(save(&f.settings(), &Change::Minimap(false)).is_err());
        assert_eq!(fs::read_to_string(f.settings()).unwrap(), "broken=[");
        fs::write(f.settings(), "color_theme='nord'\n").unwrap();
        let watcher = Watcher::start(f.theme()).unwrap();
        assert!(matches!(next(&watcher),Update::Loaded(t,_) if t.name()=="Nord"));
        fs::write(f.0.join("new"), "color_theme='monokai'\nfont_size=20\n").unwrap();
        fs::rename(f.0.join("new"), f.settings()).unwrap();
        assert!(
            matches!(next(&watcher),Update::Loaded(t,_) if t.name()=="Monokai" && t.editor_font_size()==20.0)
        );
        drop(watcher);
    }

    #[test]
    fn process_grants_are_saved_and_removed_without_touching_other_settings() {
        let f = Fixture::new();
        fs::write(f.settings(), "# mine\ncolor_theme = 'nord'\n").unwrap();
        let change = |extension: &str, program: &str, allowed| Change::AllowProcess {
            extension: extension.into(),
            program: program.into(),
            allowed,
        };
        save(&f.settings(), &change("example.fmt", "ruff", true)).unwrap();
        save(&f.settings(), &change("example.fmt", "prettier", true)).unwrap();
        save(&f.settings(), &change("example.fmt", "ruff", true)).unwrap();
        let saved = fs::read_to_string(f.settings()).unwrap();
        assert!(
            saved.starts_with("# mine\ncolor_theme = 'nord'\n"),
            "{saved}"
        );
        let preferences = Preferences::parse(&saved).unwrap();
        assert_eq!(
            preferences.allow_process["example.fmt"],
            ["prettier".to_string(), "ruff".to_string()].into()
        );
        save(&f.settings(), &change("example.fmt", "ruff", false)).unwrap();
        save(&f.settings(), &change("example.fmt", "prettier", false)).unwrap();
        let saved = fs::read_to_string(f.settings()).unwrap();
        assert_eq!(saved, "# mine\ncolor_theme = 'nord'\n");
    }

    #[test]
    fn disabled_extensions_are_saved_beside_their_grants() {
        let f = Fixture::new();
        fs::write(f.settings(), "# mine\ncolor_theme = 'nord'\n").unwrap();
        let grant = Change::AllowProcess {
            extension: "example.fmt".into(),
            program: "ruff".into(),
            allowed: true,
        };
        save(&f.settings(), &grant).unwrap();
        let toggle = |enabled| Change::ExtensionEnabled {
            extension: "example.fmt".into(),
            enabled,
        };
        save(&f.settings(), &toggle(false)).unwrap();
        save(&f.settings(), &toggle(false)).unwrap();
        let preferences = Preferences::parse(&fs::read_to_string(f.settings()).unwrap()).unwrap();
        assert_eq!(
            preferences.disabled_extensions,
            ["example.fmt".to_string()].into()
        );
        // Turning it off keeps what it may run.
        assert!(preferences.allow_process["example.fmt"].contains("ruff"));
        save(&f.settings(), &toggle(true)).unwrap();
        let saved = fs::read_to_string(f.settings()).unwrap();
        assert!(!saved.contains("disabled"), "{saved}");
        assert!(
            Preferences::parse(&saved)
                .unwrap()
                .disabled_extensions
                .is_empty()
        );
        assert!(Preferences::parse("[extensions]\ndisabled = [\"Not An Id\"]").is_err());
    }

    #[test]
    fn environment_grants_are_saved_beside_the_disabled_list() {
        let f = Fixture::new();
        fs::write(f.settings(), "# mine\ncolor_theme = 'nord'\n").unwrap();
        let allow = |allowed| Change::AllowEnvironment {
            extension: "kureha.direnv".into(),
            allowed,
        };
        let disable = Change::ExtensionEnabled {
            extension: "example.fmt".into(),
            enabled: false,
        };
        save(&f.settings(), &allow(true)).unwrap();
        save(&f.settings(), &allow(true)).unwrap();
        save(&f.settings(), &disable).unwrap();
        let preferences = Preferences::parse(&fs::read_to_string(f.settings()).unwrap()).unwrap();
        assert_eq!(
            preferences.allow_environment,
            ["kureha.direnv".to_string()].into()
        );
        assert_eq!(
            preferences.disabled_extensions,
            ["example.fmt".to_string()].into()
        );
        save(&f.settings(), &allow(false)).unwrap();
        let saved = fs::read_to_string(f.settings()).unwrap();
        assert!(!saved.contains("allow_environment"), "{saved}");
        assert!(saved.contains("disabled"), "{saved}");
        assert!(
            Preferences::parse(&saved)
                .unwrap()
                .allow_environment
                .is_empty()
        );
        assert!(Preferences::parse("[extensions]\nallow_environment = \"x.y\"").is_err());
        assert!(Preferences::parse("[extensions]\nallow_environment = [\"Not An Id\"]").is_err());
    }

    #[test]
    fn save_acknowledgement_waits_for_preview_without_losing_preferences() {
        let app = crate::App::new();
        let mut prefs = Preferences::default();
        prefs.font_size = Some(20.0);
        app.preferences.set(prefs.clone());
        let theme = app.theme.get().with_font_size(20.0);
        app.show_theme_picker(false);
        app.apply_settings_update(Update::Saved(theme.clone(), prefs.clone()));
        assert!(app.nav.is_open());
        assert_eq!(app.theme.get().editor_font_size(), 14.0);
        app.close_navigation();
        app.poll_theme_settings();
        assert_eq!(app.theme.get().editor_font_size(), 20.0);
        app.show_theme_picker(false);
        app.apply_settings_update(Update::Saved(theme, prefs));
        app.preferences.update(|p| p.font_size = Some(22.0));
        app.close_navigation();
        app.poll_theme_settings();
        assert_eq!(app.preferences.get().font_size, Some(22.0));
    }

    #[test]
    fn shutdown_flushes_queued_choices() {
        let f = Fixture::new();
        let watcher = Watcher::start(f.theme()).unwrap();
        watcher.save(Change::Color(Some("Monokai".into())));
        watcher.save(Change::Code(Some("Nord".into())));
        watcher.save(Change::FontSize(Some(18.0)));
        drop(watcher);
        let saved = Preferences::parse(&fs::read_to_string(f.settings()).unwrap()).unwrap();
        assert_eq!(saved.color_theme.as_deref(), Some("Monokai"));
        assert_eq!(saved.code_theme.as_deref(), Some("Nord"));
        assert_eq!(saved.font_size, Some(18.0));
    }

    #[test]
    #[cfg(unix)]
    fn saving_follows_settings_symlinks() {
        let f = Fixture::new();
        let target = f.0.join("dotfiles.toml");
        fs::write(&target, "# dotfiles\nhide_minimap=false\n").unwrap();
        std::os::unix::fs::symlink(&target, f.settings()).unwrap();
        save(&f.settings(), &Change::Minimap(false)).unwrap();
        assert!(f.settings().is_symlink());
        assert!(
            Preferences::parse(&fs::read_to_string(target).unwrap())
                .unwrap()
                .hide_minimap
        );
    }

    #[test]
    fn preset_selection_recovers_from_a_broken_custom_theme() {
        let f = Fixture::new();
        fs::write(f.theme(), "[broken").unwrap();
        let watcher = Watcher::start(f.theme()).unwrap();
        assert!(matches!(next(&watcher), Update::Error(_)));
        watcher.save(Change::Color(Some("Kureha".into())));
        assert!(matches!(next(&watcher),Update::Loaded(t,_) if t.name()=="Kureha Light"));
        assert_eq!(fs::read_to_string(f.theme()).unwrap(), "[broken");
        drop(watcher);
    }
}
