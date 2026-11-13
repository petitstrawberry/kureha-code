//! Extension commands, keybindings and formatters. Requests are queued on the
//! extension worker; replies are applied here from the poll loop.
use super::App;
use kureha_code::{
    extensions::{
        Extension, Key, KeyChord, Registry,
        host::{self, Host, Reply},
        process::{self, Grants},
        runtime::{CommandOutcome, DocumentSnapshot, FormatOptions},
    },
    workspace::DocumentId,
};
use scarlet_ui::{KeyCode, event::KeyModifiers};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

/// Format-on-save waits this long for an extension, then saves as is.
const FORMAT_ON_SAVE_TIMEOUT: Duration = Duration::from_millis(1500);
/// The indentation options the language server formatting request uses.
const FORMAT_OPTIONS: FormatOptions = FormatOptions {
    tab_size: 4,
    insert_spaces: true,
};
/// Primary shortcuts the workbench and editor handle whatever other
/// modifiers are held, so extensions can never receive them.
const PRIMARY_KEYS: &str = "\\|123456789jnoswfhgkpzyxcva,";
/// Built-in shortcuts not listed in the command palette or the menus.
const BUILT_IN: &[&str] = &[
    "Alt+Z",
    "Alt+Shift+F",
    "Primary+Alt+S",
    "Primary+Alt+F",
    "Primary+K",
    "F1",
    "F2",
    "F3",
    "Shift+F3",
    "F12",
    "Shift+F12",
];
/// Application menu accelerators (`ui.rs`), where `Ctrl` may mean Primary.
const MENU: &[&str] = &[
    "Ctrl+,",
    "Ctrl+-",
    "Ctrl+/",
    "Ctrl+A",
    "Ctrl+Alt+S",
    "Ctrl+C",
    "Ctrl+F",
    "Ctrl+G",
    "Ctrl+H",
    "Ctrl+J",
    "Ctrl+N",
    "Ctrl+O",
    "Ctrl+P",
    "Ctrl+S",
    "Ctrl+Shift+-",
    "Ctrl+Shift+F",
    "Ctrl+Shift+O",
    "Ctrl+Shift+P",
    "Ctrl+Shift+S",
    "Ctrl+Shift+V",
    "Ctrl+Shift+W",
    "Ctrl+Shift+Z",
    "Ctrl+Shift+\\",
    "Ctrl+Shift+`",
    "Ctrl+Space",
    "Ctrl+V",
    "Ctrl+W",
    "Ctrl+X",
    "Ctrl+Z",
    "Ctrl+[",
    "Ctrl+\\",
    "Ctrl+]",
    "Cmd+Alt+F",
];

enum Purpose {
    Command,
    /// `save` holds the path and deadline when formatting precedes a save.
    Format {
        save: Option<(PathBuf, Instant)>,
    },
}

/// What a queued request needs to apply its reply safely later.
pub(super) struct Pending {
    document: DocumentId,
    revision: u64,
    navigation: u64,
    text: Arc<String>,
    label: String,
    purpose: Purpose,
}

/// Per-window extension state, kept in the app runtime.
#[derive(Default)]
pub(super) struct State {
    /// Requests awaiting a reply, by process-unique request id.
    pending: HashMap<u64, Pending>,
    /// Messages to append to the next "Saved" status of a document.
    save_notes: HashMap<DocumentId, String>,
    /// Saves that format-on-save could not start because another file
    /// operation or a dialog was active; retried from the poll loop.
    queued_saves: Vec<(DocumentId, PathBuf)>,
    lsp_save_noted: bool,
    keys_reported: bool,
}

#[cfg(test)]
thread_local! {
    /// Tests swap in their own registry, worker and timeout per thread.
    static TEST: std::cell::Cell<Option<(&'static Registry, &'static Host, Duration)>> =
        const { std::cell::Cell::new(None) };
}
/// Points this thread's extension UI at a test registry and worker.
#[cfg(test)]
pub(super) fn use_test_extensions(registry: &'static Registry, host: &'static Host) {
    TEST.set(Some((registry, host, FORMAT_ON_SAVE_TIMEOUT)));
}
/// Installed extensions once loaded. Never blocks.
pub(super) fn registry() -> Option<&'static Registry> {
    #[cfg(test)]
    if let Some((registry, ..)) = TEST.get() {
        return Some(registry);
    }
    kureha_code::extensions::loaded()
}
fn start_host() -> Option<&'static Host> {
    #[cfg(test)]
    if let Some((_, host, _)) = TEST.get() {
        return Some(host);
    }
    host::host()
}
fn running_host() -> Option<&'static Host> {
    #[cfg(test)]
    if let Some((_, host, _)) = TEST.get() {
        return Some(host);
    }
    host::running()
}
/// The grants the extension workers check. Tests use their worker's own.
pub(super) fn process_grants() -> &'static Grants {
    #[cfg(test)]
    if let Some((_, host, _)) = TEST.get() {
        return host.grants();
    }
    process::grants()
}
fn save_timeout() -> Duration {
    #[cfg(test)]
    if let Some((.., timeout)) = TEST.get() {
        return timeout;
    }
    FORMAT_ON_SAVE_TIMEOUT
}

/// Who formats a language.
pub(super) enum Formatter {
    Extension(&'static Extension),
    LanguageServer,
    Unavailable(String),
}
/// The formatter for `language`: the user's choice from `[formatter]`, else
/// the first installed extension formatter, else the language server.
pub(super) fn formatter_for(language: &str, choice: Option<&str>) -> Formatter {
    let extensions = registry().map(|r| r.formatters(language));
    match choice {
        Some("lsp") => Formatter::LanguageServer,
        Some(id) => extensions
            .and_then(|mut e| e.find(|e| e.manifest.id == id))
            .map_or_else(
                || {
                    Formatter::Unavailable(format!(
                        "{id} is not installed or does not format {language}"
                    ))
                },
                Formatter::Extension,
            ),
        None => extensions
            .and_then(|mut e| e.next())
            .map_or(Formatter::LanguageServer, Formatter::Extension),
    }
}

/// A palette entry that grants or withdraws one declared program.
pub(super) struct PermissionCommand {
    pub extension: &'static Extension,
    pub program: &'static str,
    pub allow: bool,
    pub label: String,
    pub keywords: String,
}
/// "Allow" entries for declared programs the user has not granted, "Stop"
/// entries for granted ones. Labels name the extension id and the program
/// verbatim; the display name is only a search keyword.
pub(super) fn permission_commands() -> Vec<PermissionCommand> {
    let Some(registry) = registry() else {
        return Vec::new();
    };
    let grants = process_grants();
    registry
        .extensions
        .iter()
        .flat_map(|e| {
            e.manifest
                .capabilities
                .process
                .iter()
                .map(move |program| (e, program.as_str()))
        })
        .map(|(extension, program)| {
            let id = extension.manifest.id.as_str();
            let allow = !grants.allows(id, program);
            PermissionCommand {
                extension,
                program,
                allow,
                label: if allow {
                    process::allow_command_label(id, program)
                } else {
                    process::revoke_command_label(id, program)
                },
                keywords: format!("{} permission process program", extension.manifest.name),
            }
        })
        .collect()
}

/// Extension keybindings with the conflicts found while building them.
pub(super) struct KeyTable {
    bindings: Vec<(KeyChord, &'static Extension, &'static str)>,
    problems: Vec<String>,
}
impl KeyTable {
    fn listed_shortcuts() -> Vec<KeyChord> {
        let commands = super::navigation_ui::COMMANDS.iter().map(|c| c.shortcut);
        let mut chords = Vec::new();
        for text in commands
            .chain(BUILT_IN.iter().copied())
            .chain(MENU.iter().copied())
        {
            // A key sequence reserves its first press.
            let Some(first) = text.split(' ').next() else {
                continue;
            };
            // Menus write Primary as Ctrl; reserve both readings.
            for text in [first.to_owned(), first.replace("Ctrl", "Primary")] {
                if let Ok(chord) = KeyChord::parse(&text) {
                    chords.push(chord.normalized());
                }
            }
        }
        chords
    }
    fn reserved(chord: KeyChord, listed: &[KeyChord]) -> bool {
        let primary = if cfg!(target_os = "macos") {
            chord.primary
        } else {
            chord.control
        };
        listed.contains(&chord)
            || primary && matches!(chord.key, Key::Char(c) if PRIMARY_KEYS.contains(c))
    }
    pub(super) fn build(registry: &'static Registry) -> Self {
        let listed = Self::listed_shortcuts();
        let mut table = Self {
            bindings: Vec::new(),
            problems: Vec::new(),
        };
        for (extension, binding) in registry.keybindings() {
            let chord = binding.key.normalized();
            let name = &extension.manifest.name;
            let label = binding.key.label();
            if Self::reserved(chord, &listed) {
                table
                    .problems
                    .push(format!("{name}: {label} is a built-in shortcut"));
            } else if let Some((_, other, _)) = table.bindings.iter().find(|(c, ..)| *c == chord) {
                table.problems.push(format!(
                    "{name}: {label} is already used by {}",
                    other.manifest.name
                ));
            } else {
                table
                    .bindings
                    .push((chord, extension, binding.command.as_str()));
            }
        }
        table
    }
    fn find(&self, pressed: KeyChord) -> Option<(&'static Extension, &'static str)> {
        self.bindings
            .iter()
            .find(|(chord, ..)| *chord == pressed)
            .map(|(_, extension, command)| (*extension, *command))
    }
}
fn key_table() -> Option<&'static KeyTable> {
    #[cfg(test)]
    if TEST.get().is_some() {
        return registry().map(|r| &*Box::leak(Box::new(KeyTable::build(r))));
    }
    // Rebuilt when an extension is turned on or off. Tables hand out
    // `'static` references, so a replaced one is kept; toggles are rare.
    static TABLE: std::sync::Mutex<Option<(u64, &'static KeyTable)>> = std::sync::Mutex::new(None);
    let registry = registry()?;
    let revision = kureha_code::extensions::disabled_revision();
    let mut table = TABLE.lock().unwrap();
    match *table {
        Some((built, table)) if built == revision => Some(table),
        _ => {
            let built = &*Box::leak(Box::new(KeyTable::build(registry)));
            *table = Some((revision, built));
            Some(built)
        }
    }
}
/// The press as a chord, or `None` for keys extensions cannot bind.
fn pressed(key: KeyCode, modifiers: KeyModifiers) -> Option<KeyChord> {
    let key = match key {
        KeyCode::Char(c) if c.is_ascii_alphanumeric() => Key::Char(c.to_ascii_lowercase()),
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    if !cfg!(target_os = "macos") && modifiers.super_key {
        return None;
    }
    Some(KeyChord {
        primary: cfg!(target_os = "macos") && modifiers.super_key,
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        key,
    })
}
/// The display label of an extension command's shortcut, if bound.
pub(super) fn extension_shortcut(extension: &str, command: &str) -> Option<String> {
    key_table()?
        .bindings
        .iter()
        .find(|(_, e, c)| e.manifest.id == extension && *c == command)
        .map(|(chord, ..)| chord.label())
}

impl App {
    /// Makes the workers see the grants in `preferences`.
    pub(super) fn sync_process_grants(&self, preferences: &super::preferences::Preferences) {
        process_grants().replace(preferences.allow_process.clone());
        kureha_code::extensions::set_disabled(&preferences.disabled_extensions);
    }
    /// Turns `extension` on or off and saves the choice. Its contributions
    /// (commands, keys, formatters, themes in lists, languages) follow at
    /// once; its worker refuses further work while it is off.
    pub(super) fn set_extension_enabled(&self, extension: &str, enabled: bool) {
        self.change_preferences(super::preferences::Change::ExtensionEnabled {
            extension: extension.into(),
            enabled,
        });
        self.status.set(format!(
            "{extension} {}",
            if enabled { "enabled" } else { "disabled" }
        ));
    }
    /// Grants or withdraws `program` for `extension` and saves the choice.
    pub(super) fn set_process_permission(
        &self,
        extension: &'static Extension,
        program: &str,
        allow: bool,
    ) {
        let (id, name) = (
            extension.manifest.id.as_str(),
            extension.manifest.name.as_str(),
        );
        self.change_preferences(super::preferences::Change::AllowProcess {
            extension: id.to_owned(),
            program: program.to_owned(),
            allowed: allow,
        });
        let serves = extension
            .manifest
            .contributes
            .language_servers
            .iter()
            .any(|server| server.command == program);
        self.status.set(if allow {
            format!(
                "Allowed {name} ({id}) to run {program}. It can run {program} with any \
                 arguments, with your permissions, and {program} may load code from the \
                 project it runs in.{}",
                if serves {
                    " Restart the language server to start it."
                } else {
                    ""
                }
            )
        } else {
            format!("{name} ({id}) can no longer run {program}")
        });
    }
    fn pending<R>(&self, f: impl FnOnce(&mut HashMap<u64, Pending>) -> R) -> R {
        f(&mut self.runtime.lock().unwrap().extensions.pending)
    }
    fn note_save(&self, document: DocumentId, note: String) {
        self.runtime
            .lock()
            .unwrap()
            .extensions
            .save_notes
            .insert(document, note);
    }
    /// A message to append to the next "Saved" status of `document`.
    pub(super) fn take_save_note(&self, document: DocumentId) -> Option<String> {
        self.runtime
            .lock()
            .unwrap()
            .extensions
            .save_notes
            .remove(&document)
    }
    /// A snapshot of `id` as an extension sees it, with the text kept for later.
    fn extension_snapshot(&self, id: DocumentId) -> Option<(DocumentSnapshot, Arc<String>)> {
        let selection = self.bindings(id)?.selection.get();
        let (start, end) = (
            selection.anchor.byte.min(selection.caret.byte),
            selection.anchor.byte.max(selection.caret.byte),
        );
        let rt = self.runtime.lock().unwrap();
        let doc = rt.workspace.get(id)?;
        let text = Arc::new(doc.editor.document.as_str().to_string());
        Some((
            DocumentSnapshot {
                path: doc.path.clone().unwrap_or_default(),
                version: doc.editor.revision(),
                selection: start.min(text.len())..end.min(text.len()),
                text: text.clone(),
                workspace: rt.explorer.root.clone(),
            },
            text,
        ))
    }
    fn remember(
        &self,
        request: u64,
        snapshot_text: Arc<String>,
        id: DocumentId,
        version: u64,
        label: String,
        purpose: Purpose,
    ) {
        let navigation = self.navigation.load(Ordering::Relaxed);
        self.pending(|map| {
            map.insert(
                request,
                Pending {
                    document: id,
                    revision: version,
                    navigation,
                    text: snapshot_text,
                    label,
                    purpose,
                },
            )
        });
    }

    pub(super) fn run_extension_command(&self, extension: &str, command: &str) {
        if self.blocked() {
            return;
        }
        let Some(host) = start_host() else {
            self.status.set("Extensions are still loading".into());
            return;
        };
        let label = registry()
            .and_then(|registry| {
                let extension = registry
                    .extensions
                    .iter()
                    .find(|e| e.manifest.id == extension)?;
                let title = &extension
                    .manifest
                    .contributes
                    .commands
                    .iter()
                    .find(|c| c.id == command)?
                    .title;
                Some(format!("{}: {title}", extension.manifest.name))
            })
            .unwrap_or_else(|| command.to_owned());
        let Some(id) = self.active_id() else {
            self.status.set(format!("{label} needs an open editor"));
            return;
        };
        let Some((snapshot, text)) = self.extension_snapshot(id) else {
            return;
        };
        let version = snapshot.version;
        match host.command(extension, command, snapshot) {
            Some(request) => {
                self.remember(request, text, id, version, label.clone(), Purpose::Command);
                self.status.set(format!("{label}…"));
            }
            None => self.status.set("The extension worker has stopped".into()),
        }
    }

    /// The language and formatter of `id`, if it has a path and a language.
    fn document_formatter(&self, id: DocumentId) -> Option<(&'static str, Formatter)> {
        let path = self
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)?
            .path
            .clone()?;
        let language = super::language::language(&path)?;
        let choice = self.preferences.get().formatter.get(language).cloned();
        Some((language, formatter_for(language, choice.as_deref())))
    }
    /// Queues a format of `id`. `save` makes it format-on-save.
    fn start_format(
        &self,
        id: DocumentId,
        extension: &'static Extension,
        language: &str,
        save: Option<PathBuf>,
    ) -> bool {
        let Some((host, (snapshot, text))) = start_host().zip(self.extension_snapshot(id)) else {
            return false;
        };
        let version = snapshot.version;
        let name = extension.manifest.name.clone();
        let Some(request) = host.format(&extension.manifest.id, language, snapshot, FORMAT_OPTIONS)
        else {
            return false;
        };
        let deadline = save.map(|path| (path, Instant::now() + save_timeout()));
        self.status.set(if deadline.is_some() {
            format!("Formatting with {name} before saving…")
        } else {
            format!("Formatting with {name}…")
        });
        self.remember(
            request,
            text,
            id,
            version,
            name,
            Purpose::Format { save: deadline },
        );
        true
    }
    /// Formats the active document when an extension is its formatter.
    /// Returns false to let the language server format instead.
    pub(super) fn format_with_extension(&self) -> bool {
        if self.blocked() {
            return false;
        }
        let Some(id) = self.active_id() else {
            return false;
        };
        match self.document_formatter(id) {
            Some((language, Formatter::Extension(extension))) => {
                if !self.start_format(id, extension, language, None) {
                    self.status
                        .set(format!("{} cannot run right now", extension.manifest.name));
                }
                true
            }
            Some((_, Formatter::Unavailable(reason))) => {
                self.status.set(format!("Format: {reason}"));
                true
            }
            Some((_, Formatter::LanguageServer)) | None => false,
        }
    }
    /// Format-on-save. Returns true when the save now waits for the formatter;
    /// the reply, an error or the timeout always ends in a save.
    pub(super) fn format_before_save(&self, id: DocumentId, path: &Path) -> bool {
        if !self.preferences.get().format_on_save {
            return false;
        }
        let waiting = self.pending(|map| {
            map.values()
                .any(|p| p.document == id && matches!(p.purpose, Purpose::Format { save: Some(_) }))
        });
        if waiting {
            self.status.set("Formatting before saving…".into());
            return true;
        }
        match self.document_formatter(id) {
            Some((language, Formatter::Extension(extension))) => {
                self.start_format(id, extension, language, Some(path.to_path_buf()))
            }
            Some((_, Formatter::Unavailable(reason))) => {
                self.note_save(id, format!("not formatted: {reason}"));
                false
            }
            Some((_, Formatter::LanguageServer)) => {
                let noted = std::mem::replace(
                    &mut self.runtime.lock().unwrap().extensions.lsp_save_noted,
                    true,
                );
                if !noted {
                    self.note_save(
                        id,
                        "format on save uses extension formatters; language servers are not run on save yet"
                            .into(),
                    );
                }
                false
            }
            None => false,
        }
    }

    /// Extension keybindings, while the code editor has focus.
    pub(super) fn extension_key(&self, key: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(chord) = pressed(key, modifiers) else {
            return false;
        };
        let Some((extension, command)) = key_table().and_then(|table| table.find(chord)) else {
            return false;
        };
        if modifiers.alt {
            self.nav.swallow_shortcut_text();
        }
        self.run_extension_command(&extension.manifest.id, command);
        true
    }

    fn apply_command(&self, pending: &Pending, result: Result<CommandOutcome, String>) {
        match result {
            Err(error) => self.status.set(error),
            Ok(outcome) if outcome.edits.is_empty() => self.status.set(
                outcome
                    .status
                    .unwrap_or_else(|| format!("{} finished", pending.label)),
            ),
            Ok(outcome) => {
                let applied = self.apply_extension_edits(pending, outcome.edits, &pending.label);
                // The extension's message replaces "applied", never "discarded".
                if let Some(status) = outcome.status.filter(|_| applied) {
                    self.status.set(status);
                }
            }
        }
    }
    fn apply_extension_edits(
        &self,
        pending: &Pending,
        edits: Vec<kureha_code::extensions::runtime::TextEdit>,
        label: &str,
    ) -> bool {
        let edits = edits
            .into_iter()
            .map(|edit| (edit.range, edit.text))
            .collect::<Vec<_>>();
        self.apply_offset_edits(
            pending.document,
            pending.revision,
            pending.navigation,
            &pending.text,
            &edits,
            label,
        )
    }
    fn apply_format(&self, pending: Pending, result: Result<CommandOutcome, String>) {
        let Purpose::Format { save } = &pending.purpose else {
            return;
        };
        let name = &pending.label;
        let note = match result {
            Ok(outcome) if outcome.edits.is_empty() => {
                if save.is_none() {
                    self.status.set(format!("{name}: no changes needed"));
                }
                None
            }
            Ok(outcome) => {
                if self.apply_extension_edits(
                    &pending,
                    outcome.edits,
                    &format!("Format with {name}"),
                ) {
                    self.status.set(format!("Formatted with {name}"));
                    None
                } else {
                    Some("not formatted because the editor or document changed".to_owned())
                }
            }
            Err(error) => {
                self.status.set(error.clone());
                Some(format!("not formatted: {error}"))
            }
        };
        if let Some((path, _)) = save {
            if let Some(note) = note {
                self.note_save(pending.document, note);
            }
            self.deferred_save(pending.document, path.clone());
        }
    }
    /// Runs a save that format-on-save held back. `save_to` does nothing while
    /// another file operation or a dialog is active, so wait for those.
    fn deferred_save(&self, document: DocumentId, path: PathBuf) {
        let busy = {
            let mut rt = self.runtime.lock().unwrap();
            let busy = rt.job.is_some() || rt.dialog.is_some();
            if busy
                && !rt
                    .extensions
                    .queued_saves
                    .iter()
                    .any(|(id, _)| *id == document)
            {
                rt.extensions.queued_saves.push((document, path.clone()));
            }
            busy
        };
        if busy {
            self.status
                .set("Saving after the current file operation finishes…".into());
        } else {
            self.save_to(document, path);
        }
    }

    pub(super) fn poll_extensions(&self) {
        // Held-back saves start one at a time once nothing else is in progress.
        let queued = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.job.is_none() && rt.dialog.is_none() && !rt.extensions.queued_saves.is_empty() {
                Some(rt.extensions.queued_saves.remove(0))
            } else {
                None
            }
        };
        if let Some((document, path)) = queued {
            self.save_to(document, path);
        }
        // Format-on-save that ran out of time saves now; its late reply is dropped.
        let now = Instant::now();
        let expired = self.pending(|map| {
            let ids = map
                .iter()
                .filter(|(_, p)| {
                    matches!(&p.purpose, Purpose::Format { save: Some((_, deadline)) } if now >= *deadline)
                })
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| map.remove(&id))
                .collect::<Vec<_>>()
        });
        for pending in expired {
            if let Purpose::Format {
                save: Some((path, _)),
            } = pending.purpose
            {
                self.note_save(
                    pending.document,
                    format!("{} took too long; saved without formatting", pending.label),
                );
                self.deferred_save(pending.document, path);
            }
        }
        if let Some(table) = key_table() {
            let reported = !table.problems.is_empty()
                && std::mem::replace(
                    &mut self.runtime.lock().unwrap().extensions.keys_reported,
                    true,
                );
            if !table.problems.is_empty() && !reported {
                self.status.set(match table.problems.len() {
                    1 => format!("Extension shortcut ignored: {}", table.problems[0]),
                    n => format!(
                        "{n} extension shortcuts ignored; first: {}",
                        table.problems[0]
                    ),
                });
            }
        }
        let Some(host) = running_host() else {
            return;
        };
        for reply in host.poll() {
            let (id, result) = match reply {
                Reply::Command { id, result } | Reply::Format { id, result } => (id, result),
                Reply::Decorations { .. } => continue,
            };
            let Some(pending) = self.pending(|map| map.remove(&id)) else {
                continue;
            };
            match pending.purpose {
                Purpose::Command => self.apply_command(&pending, result),
                Purpose::Format { .. } => self.apply_format(pending, result),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditDelta;
    use kureha_code::extensions::runtime::Budgets;

    fn app_with(text: &str) -> (App, DocumentId) {
        let app = App::with_document();
        let id = app.document_id();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: text.into(),
            },
        );
        (app, id)
    }
    fn revision(app: &App, id: DocumentId) -> u64 {
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .revision()
    }

    #[test]
    fn extension_edits_apply_as_one_undo_step() {
        let text = "名前 = 1";
        let (app, id) = app_with(text);
        let applied = app.apply_offset_edits(
            id,
            revision(&app, id),
            app.navigation.load(Ordering::Relaxed),
            text,
            &[(0..6, "name".into()), (9..10, "2".into())],
            "Demo: Rename",
        );
        assert!(applied);
        assert_eq!(
            app.bindings(id).unwrap().document.get().as_str(),
            "name = 2"
        );
        app.undo(false);
        assert_eq!(app.bindings(id).unwrap().document.get().as_str(), text);
    }

    #[test]
    fn extension_edits_for_an_older_revision_are_discarded() {
        let (app, id) = app_with("abcd");
        let stale = revision(&app, id);
        app.edit(
            id,
            &EditDelta {
                replaced_range: 4..4,
                inserted: "e".into(),
            },
        );
        let applied = app.apply_offset_edits(
            id,
            stale,
            app.navigation.load(Ordering::Relaxed),
            "abcd",
            &[(0..4, "zzzz".into())],
            "Demo: Upper",
        );
        assert!(!applied);
        assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "abcde");
        assert!(app.status.get().contains("discarded"));
    }

    /// A module whose every non-activate call spins `loops` times, then answers `reply`.
    fn guest(reply: &str, loops: u32) -> Vec<u8> {
        let activate = "{}";
        let data = format!("{activate}{reply}");
        let escaped: String = data
            .bytes()
            .map(|b| match b {
                b'"' => "\\\"".into(),
                b'\\' => "\\\\".into(),
                _ => (b as char).to_string(),
            })
            .collect();
        let packed = |offset: usize, len: usize| ((offset as i64) << 32) | len as i64;
        wat::parse_str(format!(
            r#"(module
                (memory (export "memory") 2)
                (global $heap (mut i32) (i32.const 32768))
                (func (export "kx_alloc") (param $len i32) (result i32)
                    (local $p i32)
                    (local.set $p (global.get $heap))
                    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
                    (local.get $p))
                (func (export "kx_free") (param i32 i32))
                (func (export "kx_call") (param $ptr i32) (param $len i32) (result i64)
                    (local $i i32)
                    (if (i32.eq (i32.load8_u (i32.add (local.get $ptr) (i32.const 9))) (i32.const 97))
                        (then (return (i64.const {activate}))))
                    (block $done (loop $spin
                        (br_if $done (i32.ge_u (local.get $i) (i32.const {loops})))
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $spin)))
                    (i64.const {reply}))
                (data (i32.const 0) "{escaped}"))"#,
            activate = packed(0, activate.len()),
            reply = packed(activate.len(), reply.len()),
        ))
        .unwrap()
    }

    /// A command that runs `cat` through the host. A refusal is passed on as
    /// the extension's error; a run becomes `{"status":"ran"}`.
    fn process_guest() -> Vec<u8> {
        let (activate, request, ran) = (
            "{}",
            r#"{"op":"process","program":"cat","stdin":"x"}"#,
            r#"{"status":"ran"}"#,
        );
        let escaped: String = format!("{activate}{request}{ran}")
            .bytes()
            .map(|b| match b {
                b'"' => "\\\"".into(),
                b'\\' => "\\\\".into(),
                _ => (b as char).to_string(),
            })
            .collect();
        let packed = |offset: usize, len: usize| ((offset as i64) << 32) | len as i64;
        wat::parse_str(format!(
            r#"(module
                (import "kureha" "kx_host" (func $host (param i32 i32) (result i64)))
                (import "kureha" "kx_host_take" (func $take (param i32 i32) (result i32)))
                (memory (export "memory") 2)
                (global $heap (mut i32) (i32.const 32768))
                (func $alloc (export "kx_alloc") (param $len i32) (result i32)
                    (local $p i32)
                    (local.set $p (global.get $heap))
                    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
                    (local.get $p))
                (func (export "kx_free") (param i32 i32))
                (func (export "kx_call") (param $ptr i32) (param $len i32) (result i64)
                    (local $n i32) (local $buf i32)
                    (if (i32.eq (i32.load8_u (i32.add (local.get $ptr) (i32.const 9))) (i32.const 97))
                        (then (return (i64.const {activate}))))
                    (local.set $n (i32.wrap_i64
                        (call $host (i32.const {request_at}) (i32.const {request_len}))))
                    (local.set $buf (call $alloc (local.get $n)))
                    (drop (call $take (local.get $buf) (local.get $n)))
                    ;; `{{"s…` (status) means the program ran.
                    (if (i32.eq (i32.load8_u (i32.add (local.get $buf) (i32.const 2))) (i32.const 115))
                        (then (return (i64.const {ran}))))
                    (i64.or (i64.shl (i64.extend_i32_u (local.get $buf)) (i64.const 32))
                            (i64.extend_i32_u (local.get $n))))
                (data (i32.const 0) "{escaped}"))"#,
            activate = packed(0, activate.len()),
            request_at = activate.len(),
            request_len = request.len(),
            ran = packed(activate.len() + request.len(), ran.len()),
        ))
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn process_permissions_from_the_palette_reach_the_worker() {
        let dir = std::env::temp_dir().join(format!(
            "kureha-extension-ui-process-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("tool");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("ext.wasm"), process_guest()).unwrap();
        std::fs::write(
            folder.join("extension.toml"),
            "[extension]\nid='test.tool'\nname='Tool'\nversion='1'\napi=1\nwasm='ext.wasm'\n\
             [capabilities]\nprocess=['cat']\n\
             [[contributes.commands]]\nid='tool.run'\ntitle='Run'\n",
        )
        .unwrap();
        let registry: &'static Registry = Box::leak(Box::new(Registry::load(&dir)));
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let host: &'static Host = Box::leak(Box::new(Host::start(
            registry,
            Budgets::default(),
            Default::default(),
        )));
        TEST.set(Some((registry, host, FORMAT_ON_SAVE_TIMEOUT)));
        let (app, _) = app_with("text");
        let run = |expected: &str| {
            app.run_extension_command("test.tool", "tool.run");
            wait(&app, expected, |app| app.status.get().contains(expected));
        };

        // The manifest asks; nothing runs until the user allows it.
        let listed = permission_commands();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].allow);
        assert_eq!(listed[0].label, "Extensions: Allow test.tool to Run cat");
        run("Extensions: Allow test.tool to Run cat");

        app.set_process_permission(listed[0].extension, "cat", true);
        assert!(
            app.status
                .get()
                .contains("Allowed Tool (test.tool) to run cat")
        );
        assert!(app.preferences.get().allow_process["test.tool"].contains("cat"));
        let listed = permission_commands();
        assert!(!listed[0].allow);
        assert_eq!(listed[0].label, "Extensions: Stop test.tool Running cat");
        run("ran");

        app.set_process_permission(listed[0].extension, "cat", false);
        assert!(app.preferences.get().allow_process.is_empty());
        run("needs your permission");
        TEST.set(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Installs formatters for the `ok`, `fail` and `slow` languages plus
    /// keybindings, and points this thread's App at them.
    fn install(name: &str, timeout: Duration) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kureha-extension-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let extensions = dir.join("extensions");
        for (id, language, reply, loops) in [
            (
                "test.ok",
                "ok",
                r#"{"edits":[{"start":0,"end":0,"text":"formatted\n"}]}"#,
                0,
            ),
            ("test.fail", "fail", r#"{"error":"bad input"}"#, 0),
            (
                "test.slow",
                "slow",
                r#"{"edits":[{"start":0,"end":0,"text":"late\n"}]}"#,
                3_000_000,
            ),
        ] {
            let folder = extensions.join(id);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("ext.wasm"), guest(reply, loops)).unwrap();
            std::fs::write(
                folder.join("extension.toml"),
                format!(
                    "[extension]\nid='{id}'\nname='Fmt {language}'\nversion='1'\napi=1\nwasm='ext.wasm'\n\
                     [[contributes.languages]]\nid='{language}'\nextensions=['{language}']\n\
                     [[contributes.formatters]]\nlanguages=['{language}']\n\
                     [[contributes.commands]]\nid='{language}.run'\ntitle='Run'\n\
                     [[contributes.keybindings]]\ncommand='{language}.run'\nkey='{}'\n",
                    match language {
                        "ok" => "Alt+Shift+K",
                        "fail" => "Alt+Z", // built in: rejected
                        _ => "Alt+Shift+K", // taken by test.ok: rejected
                    }
                ),
            )
            .unwrap();
        }
        let registry: &'static Registry = Box::leak(Box::new(Registry::load(&extensions)));
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let host: &'static Host = Box::leak(Box::new(Host::start(
            registry,
            Budgets::default(),
            Default::default(),
        )));
        TEST.set(Some((registry, host, timeout)));
        dir
    }
    fn open(app: &App, path: &Path, text: &str) -> DocumentId {
        std::fs::write(path, text).unwrap();
        let id = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .open(path.into(), text);
        app.activate(id);
        id
    }
    fn wait(app: &App, what: &str, done: impl Fn(&App) -> bool) {
        let until = Instant::now() + Duration::from_secs(30);
        while !done(app) {
            assert!(
                Instant::now() < until,
                "{what}; status: {}",
                app.status.get()
            );
            app.poll_extensions();
            app.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn idle(app: &App) -> bool {
        let rt = app.runtime.lock().unwrap();
        rt.job.is_none()
            && rt.extensions.pending.is_empty()
            && rt.extensions.queued_saves.is_empty()
    }
    fn text(app: &App, id: DocumentId) -> String {
        app.bindings(id)
            .unwrap()
            .document
            .get()
            .as_str()
            .to_string()
    }

    #[test]
    fn turning_an_extension_off_hides_its_contributions_and_refuses_its_work() {
        // An id of its own: the enabled set is shared by the whole process.
        let dir = std::env::temp_dir().join(format!("kureha-extension-toggle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("extensions/test.toggle");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("ext.wasm"), guest(r#"{"edits":[]}"#, 0)).unwrap();
        std::fs::write(
            folder.join("extension.toml"),
            "[extension]\nid='test.toggle'\nname='Toggle'\nversion='2'\napi=1\nwasm='ext.wasm'\n\
             [[contributes.languages]]\nid='tog'\nextensions=['tog']\n\
             [[contributes.formatters]]\nlanguages=['tog']\n\
             [[contributes.commands]]\nid='tog.run'\ntitle='Run'\n\
             [[contributes.keybindings]]\ncommand='tog.run'\nkey='Alt+Shift+J'\n",
        )
        .unwrap();
        let registry: &'static Registry =
            Box::leak(Box::new(Registry::load(&dir.join("extensions"))));
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let host: &'static Host = Box::leak(Box::new(Host::start(
            registry,
            Budgets::default(),
            Default::default(),
        )));
        use_test_extensions(registry, host);
        let app = App::with_document();
        let mut chord = KeyModifiers::empty();
        chord.alt = true;
        chord.shift = true;
        let bound = || {
            key_table()
                .and_then(|table| table.find(pressed(KeyCode::Char('j'), chord)?))
                .is_some()
        };
        let contributes = || {
            (
                registry.formatters("tog").count(),
                registry.language_for_extension("tog").is_some(),
                bound(),
            )
        };
        assert_eq!(contributes(), (1, true, true));
        app.show_extensions();
        let rows = app.extensions_view_rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].enabled && rows[0].summary.contains("1 command") && rows[0].summary.contains("formats tog"));
        // A picture of the view when KUREHA_WORKBENCH_PREVIEW names a folder.
        if let Some(folder) = std::env::var_os("KUREHA_WORKBENCH_PREVIEW") {
            use scarlet_ui::{pipeline::RenderingPipeline, prelude::*};
            let mut pipeline = RenderingPipeline::new();
            pipeline.set_root(
                Window::new("Kureha Code", app.content())
                    .decorated(false)
                    .size(Size::new(1100.0, 560.0))
                    .create_element(),
            );
            pipeline.layout_initial();
            let buffer = pipeline.render().expect("workbench paints");
            let pixels = buffer.data().chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
            image::RgbImage::from_raw(buffer.width(), buffer.height(), pixels)
                .unwrap()
                .save(PathBuf::from(folder).join("extensions-view.png"))
                .unwrap();
            pipeline.teardown();
        }

        app.toggle_extension("test.toggle");
        assert!(!kureha_code::extensions::is_enabled("test.toggle"));
        assert!(app.preferences.get().disabled_extensions.contains("test.toggle"));
        assert_eq!(contributes(), (0, false, false));
        assert!(!app.extensions_view_rows()[0].enabled);
        // A caller that missed the switch is still refused by the worker.
        let id = open(&app, &dir.join("a.tog"), "body\n");
        app.activate(id);
        app.run_extension_command("test.toggle", "tog.run");
        wait(&app, "the refusal", |app| app.status.get().contains("disabled"));

        app.toggle_extension("test.toggle");
        assert_eq!(contributes(), (1, true, true));
        assert!(app.preferences.get().disabled_extensions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keybindings_skip_built_ins_and_duplicates_and_dispatch_commands() {
        let _dir = install("keys", FORMAT_ON_SAVE_TIMEOUT);
        let table = key_table().unwrap();
        assert_eq!(table.bindings.len(), 1, "{:?}", table.problems);
        assert_eq!(table.problems.len(), 2);
        assert!(
            table
                .problems
                .iter()
                .any(|p| p.contains("Alt+Z is a built-in"))
        );
        assert!(
            table
                .problems
                .iter()
                .any(|p| p.contains("already used by Fmt ok"))
        );
        assert_eq!(
            extension_shortcut("test.ok", "ok.run").as_deref(),
            Some("Alt+Shift+K")
        );
        // Every shortcut the palette lists is reserved.
        let listed = KeyTable::listed_shortcuts();
        for command in super::super::navigation_ui::COMMANDS {
            if let Ok(chord) = KeyChord::parse(command.shortcut.split(' ').next().unwrap()) {
                assert!(
                    KeyTable::reserved(chord.normalized(), &listed),
                    "{}",
                    command.shortcut
                );
            }
        }
        let (app, _) = app_with("text");
        let mut modifiers = KeyModifiers::empty();
        modifiers.alt = true;
        modifiers.shift = true;
        assert!(app.extension_key(KeyCode::Char('K'), modifiers));
        assert!(app.status.get().starts_with("Fmt ok: Run"));
        // The Alt text some backends send after the shortcut is dropped.
        assert!(app.nav.capture_shortcut_text(&scarlet_ui::Event::Keyboard(
            scarlet_ui::event::KeyEvent::Char { c: 'ﬁ' }
        )));
        assert!(!app.extension_key(KeyCode::Char('k'), KeyModifiers::empty()));
        assert!(!app.extension_key(KeyCode::Enter, modifiers));
    }

    #[test]
    fn format_document_uses_the_extension_and_reports_it() {
        let dir = install("format", FORMAT_ON_SAVE_TIMEOUT);
        let app = App::with_document();
        let id = open(&app, &dir.join("a.ok"), "body\n");
        app.format_document();
        wait(&app, "format", |app| text(app, id) == "formatted\nbody\n");
        assert_eq!(app.status.get(), "Formatted with Fmt ok");
        // Preferences can hand the language back to the language server.
        app.preferences
            .update(|p| drop(p.formatter.insert("ok".into(), "lsp".into())));
        assert!(!app.format_with_extension());
        app.preferences
            .update(|p| drop(p.formatter.insert("ok".into(), "test.missing".into())));
        assert!(app.format_with_extension());
        assert!(app.status.get().contains("not installed"));
    }

    #[test]
    fn format_on_save_writes_the_formatted_text() {
        let dir = install("save", FORMAT_ON_SAVE_TIMEOUT);
        let app = App::with_document();
        app.preferences.update(|p| p.format_on_save = true);
        let path = dir.join("a.ok");
        let id = open(&app, &path, "body\n");
        app.save();
        app.save(); // A second save while formatting does not start another.
        assert_eq!(app.pending(|map| map.len()), 1);
        wait(&app, "save", |app| {
            idle(app) && app.status.get().starts_with("Saved")
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "formatted\nbody\n");
        assert_eq!(text(&app, id), "formatted\nbody\n");
    }

    #[test]
    fn format_on_save_still_saves_after_errors_and_changes() {
        let dir = install("errors", FORMAT_ON_SAVE_TIMEOUT);
        let app = App::with_document();
        app.preferences.update(|p| p.format_on_save = true);
        let failing = dir.join("a.fail");
        open(&app, &failing, "keep\n");
        app.save();
        wait(&app, "failed format", |app| {
            idle(app) && app.status.get().starts_with("Saved")
        });
        assert!(
            app.status
                .get()
                .contains("not formatted: Fmt fail: bad input"),
            "{}",
            app.status.get()
        );
        assert_eq!(std::fs::read_to_string(&failing).unwrap(), "keep\n");

        let edited = dir.join("b.ok");
        let id = open(&app, &edited, "body\n");
        app.save();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "typed ".into(),
            },
        );
        wait(&app, "save after edit", |app| {
            idle(app) && app.status.get().starts_with("Saved")
        });
        assert!(
            app.status.get().contains("document changed"),
            "{}",
            app.status.get()
        );
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), "typed body\n");
    }

    #[test]
    fn format_on_save_times_out_and_drops_the_late_reply() {
        // A zero timeout expires on the first poll, before any reply is read,
        // so the outcome does not depend on how fast the formatter runs.
        let dir = install("timeout", Duration::ZERO);
        let app = App::with_document();
        app.preferences.update(|p| p.format_on_save = true);
        let path = dir.join("a.slow");
        let id = open(&app, &path, "body\n");
        app.save();
        wait(&app, "timeout save", |app| {
            app.status.get().starts_with("Saved")
        });
        assert!(
            app.status.get().contains("took too long"),
            "{}",
            app.status.get()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
        // Replies come back in order, so once a later format has finished the
        // slow reply has been read and dropped without touching the document.
        let other = open(&app, &dir.join("b.ok"), "other\n");
        app.format_document();
        wait(&app, "later format", |app| {
            text(app, other) == "formatted\nother\n"
        });
        assert_eq!(text(&app, id), "body\n");
    }

    /// Format-on-save finishing while another save runs still saves later.
    fn save_waits_for_a_busy_file_operation(name: &str, timeout: Duration) {
        let dir = install(name, timeout);
        let app = App::with_document();
        app.preferences.update(|p| p.format_on_save = true);
        let other = open(&app, &dir.join("b.txt"), "other\n");
        let path = dir.join("a.ok");
        let id = open(&app, &path, "body\n");
        app.save();
        // Another save holds the file operation slot until `poll` reads it.
        app.save_to(other, dir.join("b.txt"));
        assert!(app.runtime.lock().unwrap().job.is_some());
        let until = Instant::now() + Duration::from_secs(30);
        while !app.pending(|map| map.is_empty()) {
            assert!(Instant::now() < until, "status: {}", app.status.get());
            app.poll_extensions();
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!(
            "after reply: {} / {:?} / active {:?} id {id}",
            app.status.get(),
            text(&app, id),
            app.active_id()
        );
        assert_eq!(app.runtime.lock().unwrap().extensions.queued_saves.len(), 1);
        wait(&app, "queued save", |app| {
            idle(app) && app.status.get().starts_with("Saved a.ok")
        });
        // Starting the other save counts as navigation, so the edits are
        // discarded like late language server edits; the save still happens.
        let reason = if timeout.is_zero() {
            "took too long"
        } else {
            "not formatted"
        };
        assert!(app.status.get().contains(reason), "{}", app.status.get());
        assert_eq!(text(&app, id), "body\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body\n");
    }

    #[test]
    fn format_on_save_waits_for_another_save_to_finish() {
        save_waits_for_a_busy_file_operation("busy", FORMAT_ON_SAVE_TIMEOUT);
    }

    #[test]
    fn format_on_save_timeout_waits_for_another_save_to_finish() {
        save_waits_for_a_busy_file_operation("busy-timeout", Duration::ZERO);
    }

    /// The real route: window events reach the focused editor, the binding
    /// runs, and the text the platform sends after Alt+Shift+K is not typed.
    #[test]
    fn alt_shortcut_from_the_window_runs_the_command_and_types_nothing() {
        use scarlet_ui::{
            Event,
            event::{KeyEvent, MouseButton, MouseEvent},
            pipeline::RenderingPipeline,
            prelude::*,
        };
        let _dir = install("window-keys", FORMAT_ON_SAVE_TIMEOUT);
        let (app, id) = app_with("body\n");
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 700.0))
                .create_element(),
        );
        pipeline.layout_initial();
        // Click into the editor text so it has keyboard focus.
        let (x, y) = (700, 140);
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            click_count: 1,
            x,
            y,
        }));
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            click_count: 1,
            x,
            y,
        }));
        let mut modifiers = KeyModifiers::empty();
        modifiers.alt = true;
        modifiers.shift = true;
        let key = KeyCode::Char('K');
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: key,
            modifiers,
        }));
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: '\u{f8ff}' }));
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Released {
            keycode: key,
            modifiers,
        }));
        let until = Instant::now() + Duration::from_secs(30);
        while !text(&app, id).starts_with("formatted") {
            assert!(Instant::now() < until, "status: {}", app.status.get());
            app.poll_extensions();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&app, id), "formatted\nbody\n");
        // An ordinary key afterwards still types, so focus was the editor's.
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'x' }));
        assert!(text(&app, id).contains('x'), "{:?}", text(&app, id));
        pipeline.teardown();
    }

    #[test]
    fn closing_prompt_saves_without_waiting_for_the_formatter() {
        let dir = install("close", FORMAT_ON_SAVE_TIMEOUT);
        let app = App::with_document();
        app.preferences.update(|p| p.format_on_save = true);
        let path = dir.join("a.ok");
        let id = open(&app, &path, "body\n");
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "x".into(),
            },
        );
        app.request_close(None);
        assert!(app.runtime.lock().unwrap().close.is_some());
        app.save();
        assert!(app.pending(|map| map.is_empty()));
        wait(&app, "close save", |app| {
            app.runtime.lock().unwrap().job.is_none()
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xbody\n");
    }
}
