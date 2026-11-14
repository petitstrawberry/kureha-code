//! The open folder's environment from an extension (direnv's, say), given
//! to every program Kureha starts: language servers (restarted, since with
//! Nix they may only exist in the dev shell), new terminals, extension
//! programs (see `kureha_code::environment`).
//!
//! Only an enabled extension that declares `[capabilities] environment`, and
//! that the user allowed to set the environment, is asked. The poll asks
//! whenever the folder's provider differs from the one last asked (a folder
//! opened, the extensions or settings finished loading, an extension came
//! back on, permission was given); saving a watched file and "Environment:
//! Reload" ask again. An answer applies only to the request it answers; a
//! changed folder, a disabled extension or a withdrawn permission clears the
//! overlay.
use super::extension_ui::Consent;
use super::*;
use kureha_code::{
    environment::{self, Overlay},
    extensions::{EnvironmentContribution, Extension},
};
use std::time::{Duration, Instant};

/// Keys of the prompts this module shows, all about the open folder.
const CONSENT_PROMPT: &str = "environment:consent";
const FIX_PROMPT: &str = "environment:fix";

/// How long language servers for a newly opened folder wait for the
/// extensions and settings to load before starting without its environment.
const STARTUP_WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct State {
    /// The folder the environment is for (the explorer's), and when it opened.
    folder: Option<(PathBuf, Instant)>,
    /// The folder's provider, cached per folder and disabled-set revision
    /// (looking for its files touches the disk).
    provider: Option<((PathBuf, u64), Option<String>)>,
    /// The folder and extension last asked; asked again only when either
    /// changes, or on request.
    asked_for: Option<(PathBuf, String)>,
    /// The request in flight: host request id, folder, extension id.
    request: Option<(u64, PathBuf, String)>,
    /// The status bar's progress item for it.
    progress: Option<u64>,
    /// Folders whose permission prompt the user answered "Not now".
    declined: BTreeSet<PathBuf>,
    /// Whether the folder's language servers wait for the extensions and
    /// settings to load.
    waiting: bool,
    /// Times the language servers were restarted for a new environment.
    #[cfg(test)]
    restarts: u32,
}

/// The extension that supplies `folder`'s environment: the first enabled
/// provider one of whose files is there.
fn provider(folder: &Path) -> Option<(&'static Extension, &'static EnvironmentContribution)> {
    super::extension_ui::registry()?
        .environment_providers()
        .find(|(_, contribution)| contribution.files.iter().any(|f| folder.join(f).is_file()))
}

impl App {
    /// The explorer's folder, once the extensions, the settings (which hold
    /// the grants) and the login shell's environment (direnv may only be on
    /// its PATH) are in.
    fn environment_folder(&self) -> Option<PathBuf> {
        let rt = self.runtime.lock().unwrap();
        let (folder, _) = rt.environment.folder.as_ref()?;
        (super::extension_ui::registry().is_some()
            && rt.extensions.settings_applied
            && !environment::reading_login_shell())
        .then(|| folder.clone())
    }

    /// The id of the folder's provider, if any (cached).
    fn environment_provider(&self, folder: &Path) -> Option<String> {
        let key = (
            folder.to_owned(),
            kureha_code::extensions::disabled_revision(),
        );
        if let Some((cached, id)) = &self.runtime.lock().unwrap().environment.provider
            && *cached == key
        {
            return id.clone();
        }
        let id = provider(folder).map(|(extension, _)| extension.manifest.id.clone());
        self.runtime.lock().unwrap().environment.provider = Some((key, id.clone()));
        id
    }

    /// Follows the explorer's folder and asks its provider when that changes;
    /// removes an overlay no allowed provider supplies any more.
    pub(super) fn poll_environment(&self) {
        if let Some(error) = environment::take_login_shell_error() {
            self.notify(
                super::messages::Level::Error,
                format!("Could not read your login shell's environment, so programs may not be found: {error}"),
            );
        }
        let root = self.runtime.lock().unwrap().explorer.root.clone();
        let changed = {
            let mut rt = self.runtime.lock().unwrap();
            let state = &mut rt.environment;
            if state.folder.as_ref().map(|(folder, _)| folder) != root.as_ref() {
                state.folder = root.clone().map(|folder| (folder, Instant::now()));
                state.request = None;
                state.asked_for = None;
                state.waiting = false;
                true
            } else {
                false
            }
        };
        if changed {
            self.withdraw(CONSENT_PROMPT);
            self.withdraw(FIX_PROMPT);
            self.end_environment_progress();
            environment::set_pending(None);
            if environment::overlay().is_some() {
                environment::set(None);
                self.restart_language_servers_for_environment();
            }
        }
        let Some(folder) = self.environment_folder() else {
            // Until it is known whether the folder has an environment, its
            // language servers wait (for a moment) rather than start without.
            let mut rt = self.runtime.lock().unwrap();
            let wait = rt
                .environment
                .folder
                .as_ref()
                .filter(|(_, at)| at.elapsed() < STARTUP_WAIT)
                .map(|(folder, _)| folder.clone());
            if wait.is_some() != rt.environment.waiting {
                rt.environment.waiting = wait.is_some();
                drop(rt);
                environment::set_pending(wait);
            }
            return;
        };
        self.runtime.lock().unwrap().environment.waiting = false;
        // Asked only once it has everything it declares, so it is never
        // refused halfway.
        let consent = self.environment_consent(&folder);
        let wanted = consent
            .as_ref()
            .filter(|consent| consent.granted())
            .map(|consent| consent.extension.manifest.id.clone());
        if wanted.is_some() || consent.is_none() {
            self.withdraw(CONSENT_PROMPT);
        }
        let asked_for = self.runtime.lock().unwrap().environment.asked_for.clone();
        match wanted {
            Some(id) if asked_for.as_ref() != Some(&(folder.clone(), id.clone())) => {
                self.request_environment()
            }
            Some(_) => {}
            None => {
                let dropped = {
                    let mut rt = self.runtime.lock().unwrap();
                    rt.environment.asked_for = None;
                    rt.environment.request.take().is_some()
                };
                if dropped {
                    self.end_environment_progress();
                }
                if environment::is_pending(&folder) {
                    environment::set_pending(None);
                }
                if environment::overlay().is_some() {
                    environment::set(None);
                    self.restart_language_servers_for_environment();
                    self.notify(
                        super::messages::Level::Info,
                        "The folder's environment was removed",
                    );
                }
                if let Some(consent) = consent {
                    self.ask_environment_consent(&folder, consent);
                }
            }
        }
    }

    /// What the folder's provider still needs: the right to set the
    /// environment and the programs it declares. `None` without a provider.
    fn environment_consent(&self, folder: &Path) -> Option<Consent> {
        let id = self.environment_provider(folder)?;
        let extension = super::extension_ui::registry()?
            .extensions
            .iter()
            .find(|e| e.manifest.id == id)?;
        let mut consent = Consent::missing(extension);
        // It supplies the environment only once allowed to set it, even if
        // that was granted already.
        consent.environment = !environment::allows(&id);
        Some(consent)
    }

    /// Treats the open folder's permission prompt as answered "Not now".
    pub(super) fn decline_environment_prompt(&self) {
        let mut rt = self.runtime.lock().unwrap();
        if let Some((folder, _)) = rt.environment.folder.clone() {
            rt.environment.declined.insert(folder);
        }
        drop(rt);
        self.withdraw(CONSENT_PROMPT);
    }

    /// Asks for what the folder's provider needs, unless the user said "Not
    /// now" for this folder.
    fn ask_environment_consent(&self, folder: &Path, consent: Consent) {
        if self
            .runtime
            .lock()
            .unwrap()
            .environment
            .declined
            .contains(folder)
        {
            return;
        }
        let folder = folder.to_owned();
        let prompt = consent.prompt(
            CONSENT_PROMPT.into(),
            "can load this folder's environment",
            "",
        );
        self.ask(prompt, move |app, choice| match choice {
            0 => app.grant(&consent),
            _ => drop(
                app.runtime
                    .lock()
                    .unwrap()
                    .environment
                    .declined
                    .insert(folder.clone()),
            ),
        });
    }

    /// Asks the folder's provider for its environment (again).
    pub(super) fn request_environment(&self) {
        let Some(folder) = self.environment_folder() else {
            return;
        };
        // A file may have appeared or gone: look again.
        self.runtime.lock().unwrap().environment.provider = None;
        let Some(id) = self.environment_provider(&folder) else {
            if environment::overlay().is_some() {
                environment::set(None);
                self.restart_language_servers_for_environment();
            }
            return;
        };
        if let Some(consent) = self
            .environment_consent(&folder)
            .filter(|consent| !consent.granted())
        {
            // Asked for on purpose (Reload, say): ask again even after "Not now".
            self.runtime
                .lock()
                .unwrap()
                .environment
                .declined
                .remove(&folder);
            self.ask_environment_consent(&folder, consent);
            return;
        }
        self.withdraw(FIX_PROMPT);
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
        let progress = self.progress(format!(
            "{}: loading the environment…",
            self.extension_name(&id)
        ));
        let mut rt = self.runtime.lock().unwrap();
        rt.environment.asked_for = Some((folder.clone(), id.clone()));
        rt.environment.request = Some((request, folder, id));
        rt.environment.progress = Some(progress);
    }

    fn extension_name(&self, id: &str) -> String {
        super::extension_ui::registry()
            .and_then(|registry| registry.extensions.iter().find(|e| e.manifest.id == id))
            .map_or_else(|| id.to_owned(), |e| e.manifest.name.clone())
    }

    /// An extension answered a request.
    pub(super) fn environment_answered(
        &self,
        request: u64,
        result: std::result::Result<BTreeMap<String, Option<String>>, String>,
        actions: Vec<String>,
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
        let name = self.extension_name(&id);
        let before = environment::revision();
        let message = match result {
            Ok(vars) => {
                let count = vars.len();
                environment::set(Some(Overlay {
                    folder,
                    vars,
                    source: id.clone(),
                }));
                (
                    super::messages::Level::Info,
                    match count {
                        1 => format!("{name}: loaded the environment (1 variable)"),
                        n => format!("{name}: loaded the environment ({n} variables)"),
                    },
                )
            }
            // The worker already names the extension.
            Err(error) => {
                environment::set(None);
                (super::messages::Level::Error, error)
            }
        };
        // Servers that waited start now; running ones restart only if what
        // they would get changed.
        if environment::revision() != before {
            self.restart_language_servers_for_environment();
        }
        // A failure that offers a way out (allowing `.envrc`, say) asks.
        if message.0 == super::messages::Level::Error && !actions.is_empty() {
            self.ask_environment_fix(&id, message.1, actions);
        } else {
            self.notify(message.0, message.1);
        }
    }

    /// Shows `error` with buttons for the provider's `actions`, labelled with
    /// the titles of its manifest, not with anything the reply says.
    fn ask_environment_fix(&self, extension: &str, error: String, actions: Vec<String>) {
        let Some(manifest) = super::extension_ui::registry().and_then(|registry| {
            registry
                .extensions
                .iter()
                .find(|e| e.manifest.id == extension)
                .map(|e| &e.manifest)
        }) else {
            return;
        };
        let commands: Vec<(String, String)> = actions
            .into_iter()
            .filter_map(|action| {
                let command = manifest
                    .contributes
                    .commands
                    .iter()
                    .find(|c| c.id == action)?;
                Some((command.id.clone(), command.title.clone()))
            })
            .collect();
        let mut choices: Vec<String> = commands.iter().map(|(_, title)| title.clone()).collect();
        choices.push("Not now".into());
        let extension = extension.to_owned();
        self.ask(
            super::messages::Prompt {
                key: FIX_PROMPT.into(),
                title: error,
                detail: String::new(),
                choices,
            },
            move |app, choice| {
                if let Some((command, _)) = commands.get(choice) {
                    app.run_extension_command(&extension, command);
                }
            },
        );
    }

    /// A file was saved: if the folder's provider watches it, ask again.
    pub(super) fn environment_file_saved(&self, path: &Path) {
        let Some(folder) = self.environment_folder() else {
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
        let mut rt = self.runtime.lock().unwrap();
        rt.language = Default::default();
        #[cfg(test)]
        {
            rt.environment.restarts += 1;
        }
        drop(rt);
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

    fn prompt(app: &App, key: &str) -> Option<crate::messages::Prompt> {
        app.messages
            .prompts
            .get()
            .into_iter()
            .find(|p| p.key == key)
    }

    fn latest(app: &App) -> (Level, String) {
        let message = app.messages.history().remove(0);
        (message.level, message.text)
    }

    /// The permission and fix prompts as pictures, at the default and the
    /// smallest window size, with the real direnv manifest:
    /// `KUREHA_WORKBENCH_PREVIEW=<folder> cargo test --bin kureha-code prompts_render -- --ignored`
    #[test]
    #[ignore = "renders pictures into KUREHA_WORKBENCH_PREVIEW"]
    fn prompts_render_above_the_status_bar() {
        use scarlet_ui::{pipeline::RenderingPipeline, prelude::*};
        let out = PathBuf::from(std::env::var_os("KUREHA_WORKBENCH_PREVIEW").expect("a folder"));
        let dir =
            std::env::temp_dir().join(format!("kureha-prompt-preview-{}", std::process::id()));
        let folder = dir.join("direnv.kx");
        std::fs::create_dir_all(&folder).unwrap();
        let manifest =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/direnv/extension.toml");
        std::fs::copy(manifest, folder.join("extension.toml")).unwrap();
        std::fs::write(folder.join("extension.wasm"), guest("{}", 0)).unwrap();
        let registry: &'static Registry = Box::leak(Box::new(Registry::load(&dir)));
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let host: &'static Host = Box::leak(Box::new(Host::start(
            registry,
            Budgets::default(),
            Default::default(),
        )));
        use_test_extensions(registry, host);
        let app = App::with_document();
        let consent = Consent::missing(&registry.extensions[0]);
        app.ask(
            consent.prompt(
                CONSENT_PROMPT.into(),
                "can load this folder's environment",
                "",
            ),
            |_, _| {},
        );
        app.ask(
            crate::messages::Prompt {
                key: FIX_PROMPT.into(),
                title: "direnv: this folder's .envrc is not allowed yet".into(),
                detail: String::new(),
                choices: vec!["Allow .envrc".into(), "Not now".into()],
            },
            |_, _| {},
        );
        for (name, width, height) in [
            ("prompts.png", 1100.0, 760.0),
            ("prompts-narrow.png", 720.0, 400.0),
        ] {
            let mut pipeline = RenderingPipeline::new();
            pipeline.set_root(
                Window::new("Kureha Code", app.content())
                    .decorated(false)
                    .size(Size::new(width, height))
                    .create_element(),
            );
            pipeline.layout_initial();
            let buffer = pipeline.render().expect("workbench paints");
            let pixels = buffer
                .data()
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect();
            image::RgbImage::from_raw(buffer.width(), buffer.height(), pixels)
                .unwrap()
                .save(out.join(name))
                .unwrap();
            pipeline.teardown();
        }
        let _ = std::fs::remove_dir_all(&dir);
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
             [capabilities]\nenvironment=true\nprocess=['envtool']\n\
             [contributes.environment]\nfiles=['.envrc']\nwatch=['flake.nix']\n\
             [[contributes.commands]]\nid='env.allow'\ntitle='Allow .envrc'\n",
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

        // Before the settings (with the grants) load, nothing is asked or
        // told, and the folder's language servers wait for a moment.
        app.runtime.lock().unwrap().explorer.root = Some(project.clone());
        app.poll_environment();
        assert_eq!(request(&app), None);
        assert!(app.messages.history().is_empty());
        assert!(environment::is_pending(&project));
        // Loaded without a grant: nothing runs until the user allows it; a
        // prompt asks for everything it declares at once.
        app.sync_process_grants(&app.preferences.get());
        app.poll_environment();
        assert_eq!(request(&app), None);
        assert!(!environment::is_pending(&project));
        assert!(
            kureha_code::lsp::Client::spawn("kureha-test-tool", &[], &project).is_err(),
            "only the dev shell has it"
        );
        assert_eq!(
            permission(),
            (
                true,
                "Extensions: Allow test.env to Set the Environment".into()
            )
        );
        let asked = prompt(&app, CONSENT_PROMPT).expect("a permission prompt");
        assert_eq!(
            asked.title,
            "Env (test.env) can load this folder's environment"
        );
        assert_eq!(
            asked.detail,
            "Allowing it runs envtool with your permissions and sets PATH and other \
             variables for the language servers, terminals and tools Kureha starts."
        );
        assert_eq!(asked.choices, ["Allow", "Not now"]);
        assert!(app.messages.history().is_empty(), "a prompt, not a message");
        // "Not now" holds for the folder until it is asked for on purpose.
        app.answer(CONSENT_PROMPT, 1);
        app.poll_environment();
        assert!(prompt(&app, CONSENT_PROMPT).is_none());
        assert!(app.preferences.get().allow_environment.is_empty());
        app.request_environment();
        assert!(prompt(&app, CONSENT_PROMPT).is_some());
        assert_eq!(request(&app), None);

        // Allowed with one answer, which grants all of it; language servers
        // for the folder wait while it loads.
        app.answer(CONSENT_PROMPT, 0);
        assert!(app.preferences.get().allow_environment.contains("test.env"));
        assert!(app.preferences.get().allow_process["test.env"].contains("envtool"));
        assert!(prompt(&app, CONSENT_PROMPT).is_none());
        app.poll_environment();
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
        assert!(kureha_code::lsp::Client::spawn("kureha-test-tool", &[], &project).is_ok());
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
            Vec::new(),
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
        app.environment_answered(blocked, Err("Env: .envrc is blocked".into()), Vec::new());
        assert!(environment::overlay().is_none());
        assert!(!environment::is_pending(&project));
        assert_eq!(
            latest(&app),
            (Level::Error, "Env: .envrc is blocked".into())
        );
        // A failure that offers the extension's own command asks with it, by
        // its manifest title; others are dropped.
        app.request_environment();
        let blocked = request(&app).unwrap();
        app.environment_answered(
            blocked,
            Err("Env: this folder's .envrc is not allowed yet".into()),
            vec!["env.allow".into(), "someone.elses".into()],
        );
        let fix = prompt(&app, FIX_PROMPT).expect("a prompt with the way out");
        assert_eq!(fix.title, "Env: this folder's .envrc is not allowed yet");
        assert_eq!(fix.choices, ["Allow .envrc", "Not now"]);
        app.answer(FIX_PROMPT, 0);
        assert!(
            app.status.get().contains("Env: Allow .envrc"),
            "{}",
            app.status.get()
        );
        assert!(prompt(&app, FIX_PROMPT).is_none());
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
        // Back on, it is asked again without being told to.
        app.toggle_extension("test.env");
        app.poll_environment();
        assert!(request(&app).is_some());
        wait(&app, "the environment once on", |_| {
            environment::overlay().is_some()
        });
        // An answer that changes nothing keeps the language servers running.
        let restarts = app.runtime.lock().unwrap().environment.restarts;
        app.request_environment();
        wait(&app, "the same environment", |app| request(app).is_none());
        assert_eq!(app.runtime.lock().unwrap().environment.restarts, restarts);
        // Withdrawn from the palette: no prompt at once, but Reload asks.
        app.set_environment_permission("test.env", false);
        app.poll_environment();
        assert!(environment::overlay().is_none());
        assert!(prompt(&app, CONSENT_PROMPT).is_none());
        app.request_environment();
        assert_eq!(request(&app), None);
        assert!(prompt(&app, CONSENT_PROMPT).is_some());
        assert_eq!(shell("echo \"${KUREHA_TEST_SHELL-base}\""), "base\n");
        // Another folder takes the folder's prompts away.
        app.runtime.lock().unwrap().explorer.root = Some(plain.clone());
        app.poll_environment();
        assert!(app.messages.prompts.get().is_empty());

        // A grant already saved: the environment loads once the settings do,
        // even if the folder opened first.
        let app = App::with_document();
        app.runtime.lock().unwrap().explorer.root = Some(project.clone());
        app.poll_environment();
        assert_eq!(request(&app), None);
        app.preferences.update(|p| {
            p.allow_environment.insert("test.env".into());
            p.allow_process
                .insert("test.env".into(), ["envtool".to_string()].into());
        });
        app.sync_process_grants(&app.preferences.get());
        app.poll_environment();
        assert!(request(&app).is_some());
        assert!(app.messages.prompts.get().is_empty());
        wait(&app, "the environment at startup", |_| {
            environment::overlay().is_some()
        });
        app.set_environment_permission("test.env", false);
        app.poll_environment();
        assert!(environment::overlay().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The real direnv extension, direnv and Nix, through the app's own flow:
/// `KUREHA_TEST_DIRENV_EXTENSIONS=<folder scripts/build-extension.py --out built it into>
/// KUREHA_TEST_DIRENV_PROJECT=<folder with an allowed `.envrc`>
/// KUREHA_TEST_DIRENV_PROGRAM=<a program only its dev shell has>
/// cargo test real_direnv_dev_shell -- --ignored`
#[cfg(all(test, unix))]
#[test]
#[ignore]
fn real_direnv_dev_shell_reaches_language_servers() {
    use kureha_code::extensions::{Registry, host::Host, runtime::Budgets};
    let setting = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name}; see the comment above"))
    };
    let (extensions, project, program) = (
        PathBuf::from(setting("KUREHA_TEST_DIRENV_EXTENSIONS")),
        PathBuf::from(setting("KUREHA_TEST_DIRENV_PROJECT")),
        setting("KUREHA_TEST_DIRENV_PROGRAM"),
    );
    let registry: &'static Registry = Box::leak(Box::new(Registry::load(&extensions)));
    assert!(registry.errors.is_empty(), "{:?}", registry.errors);
    let host: &'static Host = Box::leak(Box::new(Host::start(
        registry,
        Budgets::default(),
        Default::default(),
    )));
    super::extension_ui::use_test_extensions(registry, host);
    // As `main` does: outside a terminal (run it under `env -i` to see a
    // Finder launch), the login shell's environment comes first.
    environment::read_login_shell_if_outside_terminal();
    let app = App::with_document();
    app.preferences.update(|p| {
        p.allow_environment.insert("kureha.direnv".into());
        p.allow_process
            .insert("kureha.direnv".into(), ["direnv".to_string()].into());
    });
    app.sync_process_grants(&app.preferences.get());
    while environment::reading_login_shell() {
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(error) = environment::take_login_shell_error() {
        panic!("login shell: {error}");
    }
    assert!(
        kureha_code::lsp::Client::spawn(&program, &[], &project).is_err(),
        "`{program}` is already on PATH"
    );
    app.runtime.lock().unwrap().explorer.root = Some(project.clone());
    let started = Instant::now();
    app.poll_environment();
    while environment::overlay().is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "no environment; {}",
            app.status.get()
        );
        assert!(
            environment::is_pending(&project),
            "stopped waiting: {}",
            app.status.get()
        );
        app.poll_extensions();
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "{} in {:.1} s",
        app.status.get(),
        started.elapsed().as_secs_f64()
    );
    let found =
        kureha_code::extensions::process::resolve(&program, environment::var_os("PATH").as_deref())
            .expect("the dev shell's PATH has it");
    println!("{program}: {}", found.display());
    assert!(found.starts_with("/nix/store"), "{}", found.display());
    assert!(kureha_code::lsp::Client::spawn(&program, &[], &project).is_ok());
    environment::set(None);
}
