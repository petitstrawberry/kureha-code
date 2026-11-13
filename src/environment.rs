//! The environment Kureha gives the programs it starts: its own process
//! environment with an overlay on top, such as the variables direnv exports
//! for the open folder (a Nix dev shell's `PATH`, say).
//!
//! The process environment itself is never changed (`set_var` is unsound
//! with other threads running). Every spawn site calls [`apply`] instead, and
//! looks programs up with [`var_os`]`("PATH")`. One overlay is active at a
//! time, for one folder; it is replaced or cleared when the folder changes.
//!
//! An overlay can come from an extension only with the user's grant: setting
//! `PATH` or `DYLD_INSERT_LIBRARIES` for every terminal and language server is
//! as strong as running a program.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// Variables to set (`Some`) or remove (`None`) for one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub folder: PathBuf,
    pub vars: BTreeMap<String, Option<String>>,
    /// The id of the extension that provided it.
    pub source: String,
}

#[derive(Default)]
struct State {
    overlay: Option<Overlay>,
    /// A folder whose environment is being read; language servers for it
    /// wait so they do not start without its `PATH`.
    pending: Option<PathBuf>,
}

static STATE: RwLock<State> = RwLock::new(State {
    overlay: None,
    pending: None,
});
/// Bumped whenever the overlay changes.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// Extensions the user allowed to set the environment
/// (`[extensions] allow_environment` in the settings).
static ALLOWED: RwLock<std::collections::BTreeSet<String>> =
    RwLock::new(std::collections::BTreeSet::new());

/// Replaces the extensions allowed to set the environment.
pub fn set_allowed(ids: &std::collections::BTreeSet<String>) {
    *ALLOWED.write().unwrap() = ids.clone();
}
/// Whether the user allowed `extension` to set the environment.
pub fn allows(extension: &str) -> bool {
    ALLOWED.read().unwrap().contains(extension)
}
/// The palette entry granting it, for messages that ask for it.
pub fn allow_command_label(extension: &str) -> String {
    format!("Extensions: Allow {extension} to Set the Environment")
}
pub fn revoke_command_label(extension: &str) -> String {
    format!("Extensions: Stop {extension} Setting the Environment")
}

/// Replaces the overlay (or clears it with `None`); it is no longer pending.
pub fn set(overlay: Option<Overlay>) {
    let mut state = STATE.write().unwrap();
    state.pending = None;
    if state.overlay != overlay {
        state.overlay = overlay;
        REVISION.fetch_add(1, Ordering::Relaxed);
    }
}
/// Marks `folder`'s environment as being read (or nothing with `None`).
pub fn set_pending(folder: Option<PathBuf>) {
    STATE.write().unwrap().pending = folder;
}
/// Whether programs for `folder` (the pending folder or one inside it)
/// should wait for its environment.
pub fn is_pending(folder: &Path) -> bool {
    STATE
        .read()
        .unwrap()
        .pending
        .as_deref()
        .is_some_and(|pending| folder.starts_with(pending))
}
pub fn overlay() -> Option<Overlay> {
    STATE.read().unwrap().overlay.clone()
}
/// Changes whenever the overlay does.
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// `key` as programs Kureha starts see it.
pub fn var_os(key: &str) -> Option<OsString> {
    match STATE
        .read()
        .unwrap()
        .overlay
        .as_ref()
        .and_then(|overlay| overlay.vars.get(key))
    {
        Some(Some(value)) => Some(value.into()),
        Some(None) => None,
        None => std::env::var_os(key),
    }
}

/// Gives `command` the overlay. Setting `PATH` also makes a bare program name
/// resolve in the overlay's `PATH`.
pub fn apply(command: &mut Command) {
    let state = STATE.read().unwrap();
    let Some(overlay) = &state.overlay else {
        return;
    };
    for (key, value) in &overlay.vars {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}

/// Parses `direnv export json`-style output: an object of name to string,
/// or `null` to unset. Empty output means no changes.
pub fn parse_exported(text: &str) -> Result<BTreeMap<String, Option<String>>, String> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let object = value.as_object().ok_or("expected an object of variables")?;
    object
        .iter()
        .map(|(key, value)| {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(format!("invalid variable name `{key}`"));
            }
            match value {
                serde_json::Value::Null => Ok((key.clone(), None)),
                serde_json::Value::String(text) if !text.contains('\0') => {
                    Ok((key.clone(), Some(text.clone())))
                }
                _ => Err(format!("`{key}` must be a string or null")),
            }
        })
        .collect()
}

/// Serialises tests that set the process-wide overlay.
#[cfg(test)]
pub(crate) fn test_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_overlay_sets_and_removes_variables_for_spawned_programs() {
        let _serial = test_serial();
        let echo = || {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "echo \"${KUREHA_TEST_SET-unset} ${KUREHA_TEST_GONE-unset}\"",
                ])
                .env("KUREHA_TEST_GONE", "inherited");
            apply(&mut command);
            String::from_utf8(command.output().unwrap().stdout).unwrap()
        };
        assert_eq!(echo(), "unset inherited\n");
        let before = revision();
        set(Some(Overlay {
            folder: "/project".into(),
            vars: BTreeMap::from([
                ("KUREHA_TEST_SET".into(), Some("from the shell".into())),
                ("KUREHA_TEST_GONE".into(), None),
            ]),
            source: "test.env".into(),
        }));
        assert_ne!(revision(), before);
        assert_eq!(echo(), "from the shell unset\n");
        assert_eq!(
            var_os("KUREHA_TEST_SET").as_deref(),
            Some("from the shell".as_ref())
        );
        assert_eq!(var_os("KUREHA_TEST_GONE"), None);
        assert_eq!(var_os("PATH"), std::env::var_os("PATH"));
        set(None);
        assert_eq!(echo(), "unset inherited\n");
        assert_eq!(var_os("KUREHA_TEST_SET"), None);
    }

    #[test]
    fn only_the_pending_folder_and_folders_inside_it_wait() {
        let _serial = test_serial();
        set_pending(Some("/work/project".into()));
        assert!(is_pending(Path::new("/work/project")));
        assert!(is_pending(Path::new("/work/project/crates/a")));
        assert!(!is_pending(Path::new("/work")));
        assert!(!is_pending(Path::new("/work/project-two")));
        // An answer (or a cleared overlay) ends the wait.
        set(None);
        assert!(!is_pending(Path::new("/work/project")));
    }

    #[test]
    fn exported_variables_set_and_unset() {
        let vars = parse_exported(r#"{"PATH":"/nix/store/x/bin:/usr/bin","OLD":null}"#).unwrap();
        assert_eq!(vars["PATH"].as_deref(), Some("/nix/store/x/bin:/usr/bin"));
        assert_eq!(vars["OLD"], None);
        assert!(parse_exported("").unwrap().is_empty());
        assert!(parse_exported("[]").is_err());
        assert!(parse_exported(r#"{"A=B":"x"}"#).is_err());
        assert!(parse_exported(r#"{"A":1}"#).is_err());
    }
}
