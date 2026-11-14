//! The environment Kureha gives the programs it starts, in three layers:
//!
//! 1. its own process environment;
//! 2. the login shell's, when Kureha was started outside a terminal (from
//!    Finder or the Dock, whose `PATH` is only `/usr/bin:/bin:...` and
//!    misses Nix and Homebrew), read once at startup;
//! 3. an overlay for the open folder, such as the variables direnv exports
//!    (a Nix dev shell's `PATH`, say).
//!
//! The process environment itself is never changed (`set_var` is unsound
//! with other threads running). Every spawn site calls [`apply`] instead, and
//! looks programs up with [`var_os`]`("PATH")`. One overlay is active at a
//! time, for one folder; it is replaced or cleared when the folder changes.
//! What computes the overlay runs on the first two layers ([`apply_base`],
//! [`base_var_os`]), since direnv reports changes from the environment it
//! runs in.
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
        Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
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
/// Bumped whenever the overlay or the login shell's environment changes.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// The login shell's environment (layer 2).
enum Login {
    /// Not read: started from a terminal, or in tests. Nothing waits.
    Unused,
    /// Being read; language servers wait.
    Reading,
    Read(BTreeMap<String, String>),
}
static LOGIN: RwLock<Login> = RwLock::new(Login::Unused);
/// Why reading it failed, until the app shows it.
static LOGIN_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// How long a login shell may take to start and print its environment.
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(10);
/// Variables of the shell itself, not of the environment it was given.
const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// Reads the login shell's environment on a worker when Kureha was started
/// outside a terminal (`TERM` unset); from a terminal, Kureha already has it.
/// Call once at startup.
pub fn read_login_shell_if_outside_terminal() {
    if std::env::var_os("TERM").is_none() {
        read_login_shell(LOGIN_SHELL_TIMEOUT);
    }
}
/// Reads `$SHELL`'s login environment on a worker; until it is in (or the
/// shell failed or timed out), [`is_pending`] and [`reading_login_shell`]
/// are true.
pub fn read_login_shell(timeout: Duration) {
    #[cfg(unix)]
    {
        let Some(shell) = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|shell| shell.is_absolute())
        else {
            return;
        };
        *LOGIN.write().unwrap() = Login::Reading;
        let started = std::thread::Builder::new()
            .name("kureha-login-shell".into())
            .spawn(move || {
                let result = login_environment(&shell, timeout);
                let mut login = LOGIN.write().unwrap();
                match result {
                    Ok(vars) => *login = Login::Read(vars),
                    Err(error) => {
                        *login = Login::Unused;
                        *LOGIN_ERROR.lock().unwrap() = Some(error);
                    }
                }
                REVISION.fetch_add(1, Ordering::Relaxed);
            });
        if started.is_err() {
            *LOGIN.write().unwrap() = Login::Unused;
        }
    }
    #[cfg(not(unix))]
    let _ = timeout;
}
/// Whether the login shell's environment is still being read.
pub fn reading_login_shell() -> bool {
    matches!(*LOGIN.read().unwrap(), Login::Reading)
}
/// Why reading the login shell's environment failed, once.
pub fn take_login_shell_error() -> Option<String> {
    LOGIN_ERROR.lock().unwrap().take()
}

/// Runs `shell` as an interactive login shell (so `.zprofile`, `.zshrc` and
/// the like run, as in a terminal) and returns the environment it prints
/// between two markers; whatever the startup files print around it is
/// ignored. It runs in a process group of its own, killed after `timeout`.
#[cfg(unix)]
pub fn login_environment(
    shell: &Path,
    timeout: Duration,
) -> Result<BTreeMap<String, String>, String> {
    use std::{
        hash::{BuildHasher, Hasher},
        io::Read,
        os::unix::process::CommandExt,
        process::Stdio,
        time::Instant,
    };
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let marker = format!("__kureha_environment_{random:016x}__");
    let script = format!("printf '%s' '{marker}'; env -0; printf '%s' '{marker}'");
    let name = shell.display();
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir(std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from))
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{name}: {e}"))?;
    let group = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take().expect("piped");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(4 << 20).read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let deadline = Instant::now() + timeout;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => break false,
        }
    };
    // Whatever the startup files left running in the group would otherwise
    // keep the pipe open.
    // SAFETY: signals only the process group this function created.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
    if !exited {
        return Err(format!(
            "{name} did not finish starting within {} s",
            timeout.as_secs()
        ));
    }
    let bytes = receiver
        .recv_timeout(Duration::from_millis(500))
        .map_err(|_| format!("{name} kept its output open"))?;
    parse_login_environment(&bytes, &marker).map_err(|e| format!("{name}: {e}"))
}

/// The `env -0` output between the two `marker`s, without the shell's own
/// variables. Entries that are not UTF-8 are skipped.
pub fn parse_login_environment(
    bytes: &[u8],
    marker: &str,
) -> Result<BTreeMap<String, String>, String> {
    let find = |haystack: &[u8]| {
        haystack
            .windows(marker.len())
            .position(|window| window == marker.as_bytes())
    };
    let start = find(bytes).ok_or("it printed no environment")? + marker.len();
    let end = start + find(&bytes[start..]).ok_or("its environment was cut off")?;
    let vars: BTreeMap<String, String> = bytes[start..end]
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok()?.split_once('='))
        .filter(|(key, _)| !key.is_empty() && !SHELL_OWN.contains(key))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    if !vars.contains_key("PATH") {
        return Err("its environment has no PATH".into());
    }
    Ok(vars)
}

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
/// should wait for its environment. Every folder waits while the login
/// shell's environment is read.
pub fn is_pending(folder: &Path) -> bool {
    reading_login_shell()
        || STATE
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
        None => base_var_os(key),
    }
}
/// `key` without the overlay: the login shell's, else Kureha's own.
pub fn base_var_os(key: &str) -> Option<OsString> {
    if let Login::Read(vars) = &*LOGIN.read().unwrap()
        && let Some(value) = vars.get(key)
    {
        return Some(value.into());
    }
    std::env::var_os(key)
}

/// Gives `command` the login shell's environment, if it was read, and the
/// overlay. Setting `PATH` also makes a bare program name resolve in that
/// `PATH`.
pub fn apply(command: &mut Command) {
    apply_base(command);
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

/// Gives `command` the login shell's environment only, for what computes
/// the overlay.
pub fn apply_base(command: &mut Command) {
    if let Login::Read(vars) = &*LOGIN.read().unwrap() {
        command.envs(vars);
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

/// Stands in for a read login shell (`Some`) or none (`None`).
#[cfg(test)]
pub(crate) fn set_login_for_test(vars: Option<BTreeMap<String, String>>) {
    *LOGIN.write().unwrap() = vars.map_or(Login::Unused, Login::Read);
    REVISION.fetch_add(1, Ordering::Relaxed);
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

#[cfg(test)]
mod login_tests {
    use super::*;

    #[test]
    fn the_environment_between_the_markers_is_read_without_the_shells_own() {
        let printed = b"Last login: today\n\x1b[1mwelcome\x1b[0m\nMARK\
            PATH=/etc/profiles/per-user/me/bin:/usr/bin\0EMPTY=\0WITH=EQUALS=x\0\
            PWD=/home\0SHLVL=2\0_=/usr/bin/env\0BAD=\xff\0\0MARKbye\n";
        let vars = parse_login_environment(printed, "MARK").unwrap();
        assert_eq!(
            vars,
            BTreeMap::from([
                ("EMPTY".to_string(), String::new()),
                ("PATH".to_string(), "/etc/profiles/per-user/me/bin:/usr/bin".to_string()),
                ("WITH".to_string(), "EQUALS=x".to_string()),
            ])
        );
        assert!(parse_login_environment(b"no markers", "MARK").is_err());
        assert!(parse_login_environment(b"MARKPATH=/bin\0", "MARK").is_err());
        assert!(parse_login_environment(b"MARKHOME=/h\0MARK", "MARK").is_err());
    }

    #[cfg(unix)]
    fn fake_shell(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kureha-login {}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shell = dir.join("shell");
        std::fs::write(&shell, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        shell
    }

    /// A shell whose startup files print and export, then run the command
    /// (`-l -i -c <command>`).
    #[cfg(unix)]
    #[test]
    fn a_login_shell_gives_what_its_startup_files_export() {
        let shell = fake_shell(
            "rc",
            "echo 'motd from .zshrc'\nexport KUREHA_TEST_FROM_RC='a b'\n\
             export PATH=\"/opt/kureha-test/bin:$PATH\"\n[ \"$1 $2 $3\" = '-l -i -c' ] || exit 9\n\
             exec /bin/sh -c \"$4\"",
        );
        let vars = login_environment(&shell, Duration::from_secs(10)).unwrap();
        assert_eq!(vars["KUREHA_TEST_FROM_RC"], "a b");
        assert!(vars["PATH"].starts_with("/opt/kureha-test/bin:"));
        assert!(!vars.contains_key("PWD") && !vars.contains_key("SHLVL"));
        let _ = std::fs::remove_dir_all(shell.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_hangs_is_stopped_with_its_children() {
        let shell = fake_shell("hang", "sleep 30 &\nsleep 30");
        let started = std::time::Instant::now();
        let error = login_environment(&shell, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // A shell that exits but leaves a child holding its output.
        let shell = fake_shell("leftover", "sleep 30 &\nexec /bin/sh -c \"$4\"");
        let started = std::time::Instant::now();
        assert!(login_environment(&shell, Duration::from_secs(10)).is_ok());
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(shell.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn the_login_shell_is_under_the_overlay_and_alone_for_what_computes_it() {
        let _serial = test_serial();
        let run = |overlay: bool| {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "echo \"${KUREHA_TEST_LAYER-none} ${KUREHA_TEST_LOGIN-none}\""]);
            if overlay {
                apply(&mut command);
            } else {
                apply_base(&mut command);
            }
            String::from_utf8(command.output().unwrap().stdout).unwrap()
        };
        set_login_for_test(Some(BTreeMap::from([
            ("KUREHA_TEST_LAYER".to_string(), "login".to_string()),
            ("KUREHA_TEST_LOGIN".to_string(), "login".to_string()),
        ])));
        assert_eq!(base_var_os("KUREHA_TEST_LAYER").as_deref(), Some("login".as_ref()));
        assert_eq!(var_os("KUREHA_TEST_LAYER").as_deref(), Some("login".as_ref()));
        assert_eq!(base_var_os("HOME"), std::env::var_os("HOME"));
        set(Some(Overlay {
            folder: "/project".into(),
            vars: BTreeMap::from([("KUREHA_TEST_LAYER".into(), Some("folder".into()))]),
            source: "test.env".into(),
        }));
        assert_eq!(run(true), "folder login\n");
        assert_eq!(run(false), "login login\n");
        assert_eq!(var_os("KUREHA_TEST_LAYER").as_deref(), Some("folder".as_ref()));
        assert_eq!(base_var_os("KUREHA_TEST_LAYER").as_deref(), Some("login".as_ref()));
        set(None);
        set_login_for_test(None);
        assert_eq!(run(true), "none none\n");
        // While it is read, every folder's language servers wait.
        *LOGIN.write().unwrap() = Login::Reading;
        assert!(is_pending(Path::new("/anywhere")));
        set_login_for_test(None);
        assert!(!is_pending(Path::new("/anywhere")));
    }
}
