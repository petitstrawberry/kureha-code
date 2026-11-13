//! Git in the workbench: the branch and change count in the status bar and
//! change marks in the explorer. Repositories are read on the Git worker.
use super::App;
use kureha_code::{
    explorer::Row,
    git::{Outcome, Snapshot, Watcher},
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// How often the folder is read again, for changes made outside the editor.
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);
/// Slow reads (many files whose times changed but whose contents did not, as
/// the index is never refreshed) wait this many times their duration.
const SLOW_READ_FACTOR: u32 = 10;

/// Git state kept in the app runtime.
#[derive(Default)]
pub(super) struct State {
    watcher: Option<Watcher>,
    started: bool,
    /// Bumped whenever the folder changes; older answers are ignored.
    generation: u64,
    folder: Option<PathBuf>,
    requested: Option<Instant>,
    waiting: bool,
    /// How long the last answered read took.
    last_read: Duration,
    snapshot: Option<Arc<Snapshot>>,
}

/// What the status bar says for an answer.
fn status_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ready(snapshot) => snapshot.summary(),
        Outcome::NotRepository => String::new(),
        Outcome::Failed(error) => format!("Git: {}", error.lines().next().unwrap_or(error)),
        Outcome::Unsupported => "Git: not available on Scarlet yet".into(),
    }
}

impl App {
    /// The open folder, else the active file's folder.
    fn git_folder(&self) -> Option<PathBuf> {
        let active = self.active_id();
        let rt = self.runtime.lock().unwrap();
        rt.explorer.root.clone().or_else(|| {
            rt.workspace
                .get(active?)?
                .path
                .as_ref()?
                .parent()
                .map(PathBuf::from)
        })
    }
    /// Reads the repository again at the next poll (after a save, say).
    pub(super) fn refresh_git(&self) {
        self.runtime.lock().unwrap().git.requested = None;
    }
    /// Sets each explorer row's change from the latest snapshot.
    pub(super) fn apply_git_marks(&self, rows: &mut [Row]) {
        let snapshot = self.runtime.lock().unwrap().git.snapshot.clone();
        for row in rows {
            row.git = match (&snapshot, &row.path) {
                (Some(snapshot), Some(path)) if row.directory => snapshot.folder_change(path),
                (Some(snapshot), Some(path)) => snapshot.change(path),
                _ => None,
            };
        }
    }
    pub(super) fn poll_git(&self) {
        let folder = self.git_folder();
        let (answer, cleared, failed_to_start) = {
            let mut rt = self.runtime.lock().unwrap();
            let state = &mut rt.git;
            let mut failed_to_start = None;
            if !state.started {
                state.started = true;
                match Watcher::start() {
                    Ok(watcher) => state.watcher = Some(watcher),
                    Err(e) => failed_to_start = Some(e),
                }
            }
            let Some(watcher) = &state.watcher else {
                drop(rt);
                if let Some(error) = failed_to_start {
                    self.status.set(error);
                }
                return;
            };
            let mut answer = None;
            if let Some((generation, answered, outcome)) = watcher.poll()
                && generation == state.generation
                && state.folder.as_ref() == Some(&answered)
            {
                state.waiting = false;
                state.last_read = state.requested.map_or(Duration::ZERO, |at| at.elapsed());
                answer = Some(outcome);
            }
            let cleared = folder != state.folder;
            if cleared {
                state.folder = folder;
                state.generation += 1;
                state.requested = None;
                state.waiting = false;
                state.snapshot = None;
                answer = None;
            }
            if let Some(folder) = &state.folder {
                let interval = REFRESH_INTERVAL.max(state.last_read * SLOW_READ_FACTOR);
                let due = state.requested.is_none_or(|at| at.elapsed() >= interval);
                if due && !state.waiting {
                    watcher.request(state.generation, folder.clone());
                    state.requested = Some(Instant::now());
                    state.waiting = true;
                }
            }
            (answer, cleared, failed_to_start)
        };
        if let Some(error) = failed_to_start {
            self.status.set(error);
        }
        let mut marks_changed = cleared;
        if cleared && !self.git_status.get().is_empty() {
            self.git_status.set(String::new());
        }
        if let Some(outcome) = answer {
            let text = status_text(&outcome);
            if self.git_status.get() != text {
                self.git_status.set(text);
            }
            let snapshot = match outcome {
                Outcome::Ready(snapshot) => Some(snapshot),
                _ => None,
            };
            let mut rt = self.runtime.lock().unwrap();
            if rt.git.snapshot.as_deref() != snapshot.as_ref() {
                rt.git.snapshot = snapshot.map(Arc::new);
                marks_changed = true;
            }
        }
        if marks_changed {
            self.refresh_tree();
        }
    }
}

#[cfg(all(test, not(target_os = "scarlet")))]
mod tests {
    use super::*;
    use kureha_code::git::Change;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git is on PATH for these tests");
        assert!(output.status.success(), "git {args:?}");
    }
    fn wait(app: &App, what: &str, done: impl Fn(&App) -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !done(app) {
            assert!(
                Instant::now() < until,
                "{what}; git status: {}",
                app.git_status.get()
            );
            app.poll();
            app.poll_git();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    /// Writes a PNG of the workbench when `KUREHA_WORKBENCH_PREVIEW` names a folder.
    fn render_preview(app: &App, name: &str) {
        use scarlet_ui::{pipeline::RenderingPipeline, prelude::*};
        let Some(directory) = std::env::var_os("KUREHA_WORKBENCH_PREVIEW") else {
            return;
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 560.0))
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
            .save(PathBuf::from(directory).join(name))
            .unwrap();
        pipeline.teardown();
    }
    fn mark(app: &App, label: &str) -> Option<Change> {
        app.tree_rows
            .get()
            .into_iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("{label} is listed"))
            .git
    }

    #[test]
    fn the_status_bar_and_explorer_show_the_repository() {
        let dir = std::env::temp_dir().join(format!("kureha-git-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for file in ["README.md", "keep.txt", "src/lib.rs"] {
            std::fs::write(dir.join(file), "one\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("src/lib.rs"), "two\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "new\n").unwrap();

        let app = App::with_document();
        app.open_folder(dir.clone());
        wait(&app, "the folder opens", |app| {
            app.tree_rows
                .get()
                .iter()
                .any(|row| row.label == "notes.txt")
        });
        wait(&app, "the repository is read", |app| {
            app.git_status.get() == "main · 2 changes"
        });
        assert_eq!(mark(&app, "notes.txt"), Some(Change::Untracked));
        assert_eq!(mark(&app, "src"), Some(Change::Modified));
        assert_eq!(mark(&app, "keep.txt"), None);

        // A change made outside the editor appears on the next read.
        std::fs::write(dir.join("keep.txt"), "two\n").unwrap();
        app.refresh_git();
        wait(&app, "the change is noticed", |app| {
            app.git_status.get() == "main · 3 changes"
        });
        assert_eq!(mark(&app, "keep.txt"), Some(Change::Modified));
        render_preview(&app, "git-explorer.png");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
