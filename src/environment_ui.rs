//! The open folder's environment from an extension (direnv's, say). It is
//! asked for when a folder opens and when a watched file is saved, and then
//! given to every program Kureha starts: language servers (restarted, since
//! with Nix they may only exist in the dev shell), new terminals, extension
//! programs (see `kureha_code::environment`).
//!
//! Only an enabled extension that declares `[capabilities] environment`, and
//! that the user allowed to set the environment, is asked. Its answer applies
//! only if the folder is still the same; a changed folder, a disabled
//! extension or a withdrawn permission clears the overlay.
use super::*;
use kureha_code::{
    environment::{self, Overlay},
    extensions::{EnvironmentContribution, Extension},
};

#[derive(Default)]
pub(super) struct State {
    /// The folder the environment is for (the explorer's).
    folder: Option<PathBuf>,
    /// The request in flight: host request id, folder, extension id.
    request: Option<(u64, PathBuf, String)>,
    /// The status bar's progress item for it.
    progress: Option<u64>,
    /// Folders already told that the extension needs permission.
    asked: BTreeSet<PathBuf>,
}

/// The extension that supplies `folder`'s environment: the first enabled
/// provider one of whose files is there.
fn provider(folder: &Path) -> Option<(&'static Extension, &'static EnvironmentContribution)> {
    super::extension_ui::registry()?
        .environment_providers()
        .find(|(_, contribution)| contribution.files.iter().any(|f| folder.join(f).is_file()))
}

impl App {
    /// Follows the explorer's folder; drops an overlay its extension may no
    /// longer supply.
    pub(super) fn poll_environment(&self) {
        let folder = self.runtime.lock().unwrap().explorer.root.clone();
        let changed = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.environment.folder != folder {
                rt.environment.folder = folder.clone();
                rt.environment.request = None;
                true
            } else {
                false
            }
        };
        if changed {
            self.end_environment_progress();
            if environment::overlay().is_some() {
                environment::set(None);
                self.restart_language_servers_for_environment();
            }
            environment::set_pending(None);
            self.request_environment();
            return;
        }
        if let Some(overlay) = environment::overlay() {
            let source = overlay.source.as_str();
            if !kureha_code::extensions::is_enabled(source) || !environment::allows(source) {
                environment::set(None);
                self.restart_language_servers_for_environment();
                self.notify(
                    super::messages::Level::Info,
                    "The folder's environment was removed",
                );
            }
        }
    }

    /// Asks the folder's provider for its environment (again).
    pub(super) fn request_environment(&self) {
        let Some(folder) = self.runtime.lock().unwrap().environment.folder.clone() else {
            return;
        };
        let Some((extension, _)) = provider(&folder) else {
            if environment::overlay().is_some() {
                environment::set(None);
                self.restart_language_servers_for_environment();
            }
            return;
        };
        let id = extension.manifest.id.clone();
        let name = extension.manifest.name.clone();
        if !environment::allows(&id) {
            let first = self
                .runtime
                .lock()
                .unwrap()
                .environment
                .asked
                .insert(folder.clone());
            if first {
                self.notify(
                    super::messages::Level::Info,
                    format!(
                        "{name} can load this folder's environment: run \"{}\" from the command palette",
                        environment::allow_command_label(&id)
                    ),
                );
            }
            return;
        }
        let Some(host) = super::extension_ui::start_host() else {
            return;
        };
        let Some(request) = host.environment(&id, folder.clone()) else {
            self.notify(
                super::messages::Level::Error,
                "The extension worker has stopped",
            );
            return;
        };
        // Language servers for this folder wait for its PATH.
        environment::set_pending(Some(folder.clone()));
        self.end_environment_progress();
        let progress = self.progress(format!("{name}: loading the environment…"));
        let mut rt = self.runtime.lock().unwrap();
        rt.environment.request = Some((request, folder, id));
        rt.environment.progress = Some(progress);
    }

    /// An extension answered a request.
    pub(super) fn environment_answered(
        &self,
        request: u64,
        result: std::result::Result<BTreeMap<String, Option<String>>, String>,
    ) {
        let (folder, id) = {
            let mut rt = self.runtime.lock().unwrap();
            match rt.environment.request.take() {
                Some((current, folder, id)) if current == request => (folder, id),
                // A newer request or another folder: this answer is stale.
                other => {
                    rt.environment.request = other;
                    return;
                }
            }
        };
        self.end_environment_progress();
        environment::set_pending(None);
        let name = super::extension_ui::registry()
            .and_then(|registry| registry.extensions.iter().find(|e| e.manifest.id == id))
            .map_or_else(|| id.clone(), |e| e.manifest.name.clone());
        match result {
            Ok(vars) => {
                let count = vars.len();
                environment::set(Some(Overlay {
                    folder,
                    vars,
                    source: id,
                }));
                self.restart_language_servers_for_environment();
                self.notify(
                    super::messages::Level::Info,
                    match count {
                        1 => format!("{name}: loaded the environment (1 variable)"),
                        n => format!("{name}: loaded the environment ({n} variables)"),
                    },
                );
            }
            Err(error) => {
                if environment::overlay().is_some() {
                    environment::set(None);
                    self.restart_language_servers_for_environment();
                }
                self.notify(super::messages::Level::Error, format!("{name}: {error}"));
            }
        }
    }

    /// A file was saved: if the folder's provider watches it, ask again.
    pub(super) fn environment_file_saved(&self, path: &Path) {
        let Some(folder) = self.runtime.lock().unwrap().environment.folder.clone() else {
            return;
        };
        if path.parent() != Some(folder.as_path()) {
            return;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let watched = super::extension_ui::registry().is_some_and(|registry| {
            registry
                .environment_providers()
                .any(|(_, contribution)| contribution.watch.iter().any(|w| w == name))
        });
        if watched {
            self.request_environment();
        }
    }

    fn end_environment_progress(&self) {
        let progress = self.runtime.lock().unwrap().environment.progress.take();
        if let Some(progress) = progress {
            self.progress_done(progress);
        }
    }

    /// Language servers start again with the new environment, and servers
    /// that failed without it get another try.
    fn restart_language_servers_for_environment(&self) {
        self.runtime.lock().unwrap().language = Default::default();
        self.changed();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        extension_ui::{
            permission_commands,
            tests::{guest, wait},
            use_test_extensions,
        },
        messages::Level,
    };
    use kureha_code::extensions::{Registry, host::Host, runtime::Budgets};
    use std::os::unix::fs::PermissionsExt;

    fn request(app: &App) -> Option<u64> {
        app.runtime
            .lock()
            .unwrap()
            .environment
            .request
            .as_ref()
            .map(|(id, ..)| *id)
    }
    /// What a program Kureha starts now prints for `script`.
    fn shell(script: &str) -> String {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", script]);
        environment::apply(&mut command);
        String::from_utf8(command.output().unwrap().stdout).unwrap()
    }
    fn latest(app: &App) -> (Level, String) {
        let message = app.messages.history().remove(0);
        (message.level, message.text)
    }

    /// The whole flow with a stand-in for direnv: its answer puts a dev
    /// shell's tool on PATH.
    #[test]
    fn the_folder_environment_reaches_new_programs_once_allowed() {
        let dir = std::env::temp_dir().join(format!("kureha-environment-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (project, plain, tools) = (
            dir.join("project"),
            dir.join("plain"),
            dir.join("shell/bin"),
        );
        for folder in [&project, &plain, &tools] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(project.join(".envrc"), "use flake\n").unwrap();
        let tool = tools.join("kureha-test-tool");
        std::fs::write(&tool, "#!/bin/sh\necho from the dev shell\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            tools.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let extension = dir.join("extensions/test.env");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(
            extension.join("ext.wasm"),
            guest(
                &format!(
                    r#"{{"environment":{{"PATH":"{path}","KUREHA_TEST_SHELL":"dev","KUREHA_TEST_UNSET":null}}}}"#
                ),
                0,
            ),
        )
        .unwrap();
        std::fs::write(
            extension.join("extension.toml"),
            "[extension]\nid='test.env'\nname='Env'\nversion='1'\napi=1\nwasm='ext.wasm'\n\
             [capabilities]\nenvironment=true\n\
             [contributes.environment]\nfiles=['.envrc']\nwatch=['flake.nix']\n",
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
        let permission = || {
            permission_commands()
                .into_iter()
                .find(|p| p.environment())
                .map(|p| (p.allow, p.label))
                .unwrap()
        };

        // Nothing runs until the user allows it; they are told how, once.
        app.runtime.lock().unwrap().explorer.root = Some(project.clone());
        app.poll_environment();
        assert_eq!(request(&app), None);
        assert_eq!(
            permission(),
            (
                true,
                "Extensions: Allow test.env to Set the Environment".into()
            )
        );
        let (level, text) = latest(&app);
        assert_eq!(level, Level::Info);
        assert!(
            text.contains("\"Extensions: Allow test.env to Set the Environment\""),
            "{text}"
        );
        let told = app.messages.history().len();
        app.request_environment();
        assert_eq!(app.messages.history().len(), told);

        // Allowed: language servers for the folder wait while it loads.
        app.set_environment_permission("test.env", true);
        assert!(app.preferences.get().allow_environment.contains("test.env"));
        assert!(request(&app).is_some());
        assert!(environment::is_pending(&project.join("crates/a")));
        assert_eq!(app.messages.progress.get().len(), 1);
        wait(&app, "the environment", |_| {
            environment::overlay().is_some()
        });
        assert!(!environment::is_pending(&project));
        assert!(app.messages.progress.get().is_empty());
        assert_eq!(
            latest(&app),
            (
                Level::Info,
                "Env: loaded the environment (3 variables)".into()
            )
        );
        assert_eq!(
            permission(),
            (
                false,
                "Extensions: Stop test.env Setting the Environment".into()
            )
        );
        // New programs get it, and a bare name resolves in its PATH.
        assert_eq!(
            shell("kureha-test-tool; echo \"$KUREHA_TEST_SHELL\""),
            "from the dev shell\ndev\n"
        );
        assert_eq!(
            kureha_code::extensions::process::resolve(
                "kureha-test-tool",
                environment::var_os("PATH").as_deref()
            ),
            Some(tool.clone())
        );

        // Saving a watched file of the folder reads it again.
        app.environment_file_saved(&project.join("main.rs"));
        app.environment_file_saved(&plain.join("flake.nix"));
        assert_eq!(request(&app), None);
        app.environment_file_saved(&project.join("flake.nix"));
        assert!(request(&app).is_some());
        wait(&app, "the reload", |app| request(app).is_none());
        assert_eq!(shell("echo \"$KUREHA_TEST_SHELL\""), "dev\n");

        // Another folder: the overlay goes, and the answer for the old one
        // is dropped when it comes.
        app.request_environment();
        let old = request(&app).unwrap();
        app.runtime.lock().unwrap().explorer.root = Some(plain.clone());
        app.poll_environment();
        assert_eq!(request(&app), None);
        assert!(environment::overlay().is_none());
        app.environment_answered(
            old,
            Ok(BTreeMap::from([(
                "KUREHA_TEST_SHELL".into(),
                Some("stale".into()),
            )])),
        );
        assert!(environment::overlay().is_none());
        assert_eq!(shell("echo \"${KUREHA_TEST_SHELL-base}\""), "base\n");

        // Back again; then a failure (a blocked `.envrc`) removes it.
        app.runtime.lock().unwrap().explorer.root = Some(project.clone());
        app.poll_environment();
        wait(&app, "the environment again", |_| {
            environment::overlay().is_some()
        });
        app.request_environment();
        let blocked = request(&app).unwrap();
        app.environment_answered(blocked, Err("direnv: .envrc is blocked".into()));
        assert!(environment::overlay().is_none());
        assert!(!environment::is_pending(&project));
        assert_eq!(
            latest(&app),
            (Level::Error, "Env: direnv: .envrc is blocked".into())
        );
        app.request_environment();
        wait(&app, "the environment after a failure", |_| {
            environment::overlay().is_some()
        });

        // Turning the extension off, or withdrawing the grant, removes it.
        app.toggle_extension("test.env");
        app.poll_environment();
        assert!(environment::overlay().is_none());
        assert_eq!(
            latest(&app),
            (Level::Info, "The folder's environment was removed".into())
        );
        app.toggle_extension("test.env");
        app.request_environment();
        wait(&app, "the environment once on", |_| {
            environment::overlay().is_some()
        });
        app.set_environment_permission("test.env", false);
        app.poll_environment();
        assert!(environment::overlay().is_none());
        let told = app.messages.history().len();
        app.request_environment();
        assert_eq!(request(&app), None);
        assert_eq!(app.messages.history().len(), told, "asked once per folder");
        assert_eq!(shell("echo \"${KUREHA_TEST_SHELL-base}\""), "base\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
