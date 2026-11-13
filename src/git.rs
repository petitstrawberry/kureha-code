//! Git state for the workbench: the branch and the changed files of the
//! repository that contains the open folder, computed on a worker thread.
//! [`diff`] reads the lines of one change; [`write`] stages and commits.
//!
//! Opening a folder must never run that folder's code. Git configuration can
//! name commands (clean/smudge filters, for example) and secrets; settings like
//! these are read only from the user's and the system's configuration, never
//! from the repository's own `.git/config`, whoever owns it. Reading never
//! writes: the index is read, never refreshed or locked.
//!
//! On Scarlet, gix runs on patched dependencies (memory maps, file times,
//! entropy; see `docs/git.md`).

pub mod branch;
pub mod diff;
pub mod write;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// Changed paths kept per snapshot; the count still includes the rest.
pub const MAX_CHANGES: usize = 20_000;
/// The worker's stack, the same as the file worker's (`kureha-file`).
const STACK_SIZE: usize = 2 * 1024 * 1024;

/// How a path differs from `HEAD`, in ascending order of significance, so a
/// folder shows the most significant change below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Change {
    Untracked,
    Renamed,
    Added,
    Deleted,
    Modified,
    Conflicted,
}
impl Change {
    /// The letter shown next to a file.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Untracked => "U",
            Self::Renamed => "R",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Modified => "M",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// A commit checked out directly; the abbreviated id.
    Detached(String),
    /// A branch without commits yet.
    Unborn(String),
}
impl Head {
    pub fn label(&self) -> String {
        match self {
            Self::Branch(name) => name.clone(),
            Self::Detached(id) => format!("detached at {id}"),
            Self::Unborn(name) => format!("{name} (no commits)"),
        }
    }
}

/// A changed path of the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the work tree.
    pub path: PathBuf,
    pub change: Change,
    /// Where a rename came from, relative to the work tree.
    pub source: Option<PathBuf>,
    /// An untracked folder reported as a whole.
    pub folder: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub head: Head,
    /// Changed paths inside the requested folder, spelled from that folder,
    /// with the most significant change of each.
    pub changes: BTreeMap<PathBuf, Change>,
    /// Untracked folders reported as a whole (their files are untracked).
    pub untracked_folders: Vec<PathBuf>,
    /// Changes in the whole repository, including any not kept.
    pub total: usize,
    /// The repository's work tree.
    pub workdir: PathBuf,
    /// Changes from `HEAD` to the index (what a commit records), in the whole
    /// repository, at most [`MAX_CHANGES`].
    pub staged: Vec<Entry>,
    /// Changes from the index to the work tree, untracked files included.
    pub unstaged: Vec<Entry>,
    /// The repository has hooks that `git commit` would run; Kureha does not.
    pub hooks: bool,
    /// A merge, rebase or similar is in progress (committing is left to Git).
    pub in_progress: Option<String>,
    /// Local branches.
    pub branches: Vec<branch::Branch>,
}
impl Snapshot {
    /// The change of a file, including files inside untracked folders.
    pub fn change(&self, path: &Path) -> Option<Change> {
        self.changes.get(path).copied().or_else(|| {
            self.untracked_folders
                .iter()
                .any(|folder| path.starts_with(folder))
                .then_some(Change::Untracked)
        })
    }
    /// The most significant change at or below `folder`.
    pub fn folder_change(&self, folder: &Path) -> Option<Change> {
        // Paths order by component, so everything below `folder` follows it.
        let below = self
            .changes
            .range(folder.to_path_buf()..)
            .take_while(|(path, _)| path.starts_with(folder))
            .map(|(_, change)| *change)
            .max();
        let untracked = self
            .untracked_folders
            .iter()
            .any(|f| f.starts_with(folder) || folder.starts_with(f))
            .then_some(Change::Untracked);
        below.max(untracked)
    }
    /// For the status bar: `main · 3 changes`.
    pub fn summary(&self) -> String {
        match self.total {
            0 => self.head.label(),
            1 => format!("{} · 1 change", self.head.label()),
            n => format!("{} · {n} changes", self.head.label()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    NotRepository,
    Ready(Snapshot),
    Failed(String),
}

/// Reads the repository containing `folder`. Stops early, with an error, when
/// `interrupt` is set.
pub fn read(folder: &Path, interrupt: Arc<AtomicBool>) -> Outcome {
    // `.git` (a folder, or a file for linked work trees) here or above.
    if !folder.ancestors().any(|dir| dir.join(".git").exists()) {
        return Outcome::NotRepository;
    }
    imp::read(folder, interrupt)
}

mod imp {
    use super::*;
    use gix::{bstr::ByteSlice, status::index_worktree, status::plumbing::index_as_worktree};

    pub(super) fn open(folder: &Path) -> Result<Option<gix::Repository>, String> {
        // Command- and secret-bearing settings (filters, credentials, signing,
        // remotes, submodules) only from outside the repository.
        let options = gix::open::Options::default().filter_config_section(|meta| {
            meta.source.kind() != gix::config::source::Kind::Repository
        });
        let mapping = gix::sec::trust::Mapping {
            full: options.clone(),
            reduced: options,
        };
        let mut repo =
            gix::ThreadSafeRepository::discover_opts(folder, Default::default(), mapping)
                .map_err(|e| e.to_string())?
                .to_thread_local();
        if !cfg!(unix) {
            // Without Unix permissions (Scarlet) every file reads as not
            // executable; keep the recorded modes, as Git does with
            // core.fileMode=false, instead of reporting or staging a change.
            let mut config = repo.config_snapshot_mut();
            config
                .set_value(&gix::config::tree::Core::FILE_MODE, "false")
                .map_err(|e| e.to_string())?;
            config.commit().map_err(|e| e.to_string())?;
        }
        Ok(Some(repo))
    }

    fn head(repo: &gix::Repository) -> Result<super::Head, String> {
        let head = repo.head().map_err(|e| e.to_string())?;
        Ok(match head.kind {
            gix::head::Kind::Symbolic(reference) => {
                super::Head::Branch(reference.name.shorten().to_string())
            }
            gix::head::Kind::Unborn(name) => super::Head::Unborn(name.shorten().to_string()),
            gix::head::Kind::Detached { target, .. } => {
                super::Head::Detached(target.to_hex_with_len(7).to_string())
            }
        })
    }

    pub fn read(folder: &Path, interrupt: Arc<AtomicBool>) -> Outcome {
        match snapshot(folder, interrupt) {
            Ok(Some(snapshot)) => Outcome::Ready(snapshot),
            Ok(None) => Outcome::NotRepository,
            Err(e) => Outcome::Failed(e),
        }
    }

    fn snapshot(folder: &Path, interrupt: Arc<AtomicBool>) -> Result<Option<Snapshot>, String> {
        let Some(repo) = open(folder)? else {
            return Ok(None);
        };
        // Bare repositories have no files to compare.
        let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
            return Ok(None);
        };
        // Where the open folder sits inside the work tree, compared on
        // resolved paths so `/tmp` and `/private/tmp` agree.
        let resolved =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let inside = resolved(folder)
            .strip_prefix(resolved(&workdir))
            .map(Path::to_path_buf)
            .map_err(|_| "the folder is not inside the repository's work tree".to_string())?;
        let mut snapshot = Snapshot {
            head: head(&repo)?,
            changes: BTreeMap::new(),
            untracked_folders: Vec::new(),
            total: 0,
            workdir: workdir.clone(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            hooks: has_hooks(&repo),
            in_progress: repo
                .state()
                .map(|state| in_progress_label(&state).to_string()),
            branches: super::branch::list(&repo)?,
        };
        // A path staged and also changed again counts once, like `git status`.
        let mut seen = std::collections::HashSet::new();
        let mut note = |relative: &Path,
                        change: Change,
                        folder_entry: bool,
                        staged: Option<Option<PathBuf>>,
                        snapshot: &mut Snapshot| {
            if seen.insert(relative.to_path_buf()) {
                snapshot.total += 1;
            }
            let list = match &staged {
                Some(_) => &mut snapshot.staged,
                None => &mut snapshot.unstaged,
            };
            if list.len() < MAX_CHANGES {
                list.push(Entry {
                    path: relative.to_path_buf(),
                    change,
                    source: staged.flatten(),
                    folder: folder_entry,
                });
            }
            let Ok(below) = relative.strip_prefix(&inside) else {
                return;
            };
            let path = folder.join(below);
            if folder_entry {
                snapshot.untracked_folders.push(path);
            } else if snapshot.changes.len() < MAX_CHANGES || snapshot.changes.contains_key(&path) {
                let entry = snapshot.changes.entry(path).or_insert(change);
                *entry = (*entry).max(change);
            }
        };
        let status = repo
            .status(gix::progress::Discard)
            .map_err(|e| e.to_string())?
            .untracked_files(gix::status::UntrackedFiles::Collapsed)
            .index_worktree_submodules(None)
            .index_worktree_rewrites(None)
            .should_interrupt_owned(interrupt)
            .into_iter(None)
            .map_err(|e| e.to_string())?;
        for item in status {
            let item = item.map_err(|e| e.to_string())?;
            match item {
                gix::status::Item::IndexWorktree(item) => {
                    let relative = gix::path::from_bstr(item.rela_path())
                        .map_err(|e| e.to_string())?
                        .into_owned();
                    let relative = relative.as_path();
                    match &item {
                        index_worktree::Item::DirectoryContents { entry, .. } => {
                            let folder_entry =
                                entry.disk_kind == Some(gix::dir::entry::Kind::Directory);
                            note(
                                relative,
                                Change::Untracked,
                                folder_entry,
                                None,
                                &mut snapshot,
                            );
                        }
                        _ => {
                            use index_as_worktree::EntryStatus;
                            let change = match &item {
                                index_worktree::Item::Modification { status, .. } => match status {
                                    EntryStatus::Conflict { .. } => Some(Change::Conflicted),
                                    EntryStatus::IntentToAdd => Some(Change::Added),
                                    EntryStatus::Change(index_as_worktree::Change::Removed) => {
                                        Some(Change::Deleted)
                                    }
                                    EntryStatus::Change(_) => Some(Change::Modified),
                                    EntryStatus::NeedsUpdate(_) => None,
                                },
                                index_worktree::Item::Rewrite { .. } => Some(Change::Renamed),
                                index_worktree::Item::DirectoryContents { .. } => None,
                            };
                            if let Some(change) = change {
                                note(relative, change, false, None, &mut snapshot);
                            }
                        }
                    }
                }
                gix::status::Item::TreeIndex(change) => {
                    use gix::diff::index::ChangeRef;
                    let path = |location: &gix::bstr::BStr| {
                        gix::path::from_bstr(location)
                            .map(|path| path.into_owned())
                            .map_err(|e| e.to_string())
                    };
                    let (location, change, source) = match &change {
                        ChangeRef::Addition { location, .. } => (location, Change::Added, None),
                        ChangeRef::Deletion { location, .. } => (location, Change::Deleted, None),
                        ChangeRef::Modification { location, .. } => {
                            (location, Change::Modified, None)
                        }
                        ChangeRef::Rewrite {
                            location,
                            source_location,
                            ..
                        } => (
                            location,
                            Change::Renamed,
                            Some(path(source_location.as_bstr())?),
                        ),
                    };
                    let relative = path(location.as_bstr())?;
                    note(&relative, change, false, Some(source), &mut snapshot);
                }
            }
        }
        snapshot.staged.sort_by(|a, b| a.path.cmp(&b.path));
        snapshot.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Some(snapshot))
    }

    /// Hooks `git commit` runs, in the configured hooks folder.
    fn has_hooks(repo: &gix::Repository) -> bool {
        // Every source counts here, the repository's own config included: this
        // only decides whether to say that hooks are not run.
        let configured = repo
            .config_snapshot()
            .string("core.hooksPath")
            .map(|value| value.to_string())
            .map(|value| match value.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME").map_or_else(
                    || PathBuf::from(&value),
                    |home| PathBuf::from(home).join(rest),
                ),
                None => PathBuf::from(value),
            });
        let folder = match configured {
            Some(path) if path.is_absolute() => path,
            Some(path) => repo.workdir().unwrap_or(repo.git_dir()).join(path),
            None => repo.common_dir().join("hooks"),
        };
        [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ]
        .iter()
        .any(|hook| folder.join(hook).is_file())
    }

    pub(super) fn in_progress_label(state: &gix::state::InProgress) -> &'static str {
        use gix::state::InProgress::*;
        match state {
            ApplyMailbox | ApplyMailboxRebase => "applying patches",
            Bisect => "bisecting",
            CherryPick | CherryPickSequence => "cherry-picking",
            Merge => "merging",
            Rebase | RebaseInteractive => "rebasing",
            Revert | RevertSequence => "reverting",
        }
    }
}

type Request = (u64, PathBuf);
type Answer = (u64, PathBuf, Outcome);

/// Reads repositories on a worker thread. Requests made while one runs
/// interrupt it; only the newest request is answered.
pub struct Watcher {
    requests: mpsc::Sender<Request>,
    answers: Mutex<mpsc::Receiver<Answer>>,
    interrupt: Arc<AtomicBool>,
}
impl Watcher {
    pub fn start() -> Result<Self, String> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, answers) = mpsc::channel();
        let interrupt = Arc::new(AtomicBool::new(false));
        let flag = interrupt.clone();
        std::thread::Builder::new()
            .name("kureha-git".into())
            // Scarlet's default thread stack (64 KiB) is too small for gix.
            .stack_size(STACK_SIZE)
            .spawn(move || {
                while let Ok(mut request) = inbox.recv() {
                    // Only the newest request matters.
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                    flag.store(false, Ordering::Relaxed);
                    let outcome = read(&request.1, flag.clone());
                    if flag.load(Ordering::Relaxed) {
                        continue; // Interrupted by a newer request, which follows.
                    }
                    if outbox.send((request.0, request.1, outcome)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Cannot start the Git worker: {e}"))?;
        Ok(Self {
            requests,
            answers: Mutex::new(answers),
            interrupt,
        })
    }
    /// Asks for `folder`'s state, interrupting a read in progress.
    pub fn request(&self, generation: u64, folder: PathBuf) {
        // Raise the flag before sending: the worker clears it when it starts
        // a read, so raising it afterwards could stop this very request.
        self.interrupt.store(true, Ordering::Relaxed);
        let _ = self.requests.send((generation, folder));
    }
    /// The newest answer since the last poll. Never blocks.
    pub fn poll(&self) -> Option<Answer> {
        self.answers.lock().unwrap().try_iter().last()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::process::Command;

    /// A scratch repository, removed on drop.
    pub struct Repo(pub PathBuf);
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl Repo {
        pub fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("kureha-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Self(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            // Kureha reads the developer's settings too; keep these tests
            // independent of them.
            repo.git(&["config", "user.name", "Test"]);
            repo.git(&["config", "user.email", "test@example.invalid"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo
        }
        /// Runs the `git` command line, isolated from the developer's settings.
        pub fn git(&self, args: &[&str]) -> String {
            self.git_in(&self.0, args)
        }
        pub fn git_in(&self, dir: &Path, args: &[&str]) -> String {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "protocol.file.allow=always",
                ])
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git is on PATH for these tests");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
        pub fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        pub fn read(&self, folder: &str) -> Snapshot {
            match read(&self.0.join(folder), Arc::new(AtomicBool::new(false))) {
                Outcome::Ready(snapshot) => snapshot,
                other => panic!("{other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::Repo, *};
    use std::time::Duration;

    fn changes(repo: &Repo, snapshot: &Snapshot) -> Vec<(String, Change)> {
        snapshot
            .changes
            .iter()
            .map(|(path, change)| {
                let relative = path.strip_prefix(&repo.0).unwrap();
                (relative.to_string_lossy().into_owned(), *change)
            })
            .collect()
    }

    #[test]
    fn changes_match_git_status_and_the_index_is_left_alone() {
        let repo = Repo::new("status");
        for file in [
            "keep.txt",
            "modify.txt",
            "stage.txt",
            "delete.txt",
            "rename-me.txt",
            "sub/deep.txt",
            "both.txt",
        ] {
            repo.write(file, &format!("content of {file}\n"));
        }
        repo.write(".gitignore", "ignored.log\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.write("modify.txt", "changed\n");
        repo.write("stage.txt", "staged\n");
        repo.git(&["add", "stage.txt"]);
        std::fs::remove_file(repo.0.join("delete.txt")).unwrap();
        repo.git(&["mv", "rename-me.txt", "renamed.txt"]);
        repo.write("untracked.txt", "new\n");
        repo.write("sub/new.txt", "new\n");
        repo.write("fresh/a.txt", "new\n");
        repo.write("fresh/b.txt", "new\n");
        repo.write("ignored.log", "log\n");
        repo.write("added.txt", "new\n");
        repo.git(&["add", "added.txt"]);
        repo.write("both.txt", "staged\n");
        repo.git(&["add", "both.txt"]);
        repo.write("both.txt", "staged and more\n");
        repo.write("sub/deep.txt", "changed\n");
        let index = repo.0.join(".git/index");
        let before = (
            std::fs::read(&index).unwrap(),
            std::fs::metadata(&index).unwrap().modified().unwrap(),
        );

        let snapshot = repo.read("");
        assert_eq!(snapshot.head, Head::Branch("main".into()));
        assert_eq!(
            changes(&repo, &snapshot),
            [
                ("added.txt".to_string(), Change::Added),
                ("both.txt".into(), Change::Modified),
                ("delete.txt".into(), Change::Deleted),
                ("modify.txt".into(), Change::Modified),
                ("renamed.txt".into(), Change::Renamed),
                ("stage.txt".into(), Change::Modified),
                ("sub/deep.txt".into(), Change::Modified),
                ("sub/new.txt".into(), Change::Untracked),
                ("untracked.txt".into(), Change::Untracked),
            ]
        );
        // A wholly untracked folder is one entry; its files are untracked.
        assert_eq!(snapshot.untracked_folders, [repo.0.join("fresh")]);
        assert_eq!(
            snapshot.change(&repo.0.join("fresh/a.txt")),
            Some(Change::Untracked)
        );
        assert_eq!(snapshot.change(&repo.0.join("keep.txt")), None);
        assert_eq!(snapshot.change(&repo.0.join("ignored.log")), None);
        assert_eq!(
            snapshot.folder_change(&repo.0.join("sub")),
            Some(Change::Modified)
        );
        assert_eq!(
            snapshot.folder_change(&repo.0.join("fresh")),
            Some(Change::Untracked)
        );
        assert_eq!(snapshot.total, 10);
        assert_eq!(snapshot.summary(), "main · 10 changes");
        let after = (
            std::fs::read(&index).unwrap(),
            std::fs::metadata(&index).unwrap().modified().unwrap(),
        );
        assert!(before == after, "the index must not be rewritten");
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[test]
    fn filters_from_the_repositorys_own_config_never_run() {
        let repo = Repo::new("filter");
        let marker = repo.0.with_extension("filter-ran");
        let _ = std::fs::remove_file(&marker);
        repo.write(".gitattributes", "* filter=kureha-test\n");
        repo.write("a.txt", "hello\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        let clean = format!("touch '{}'; cat", marker.display());
        repo.git(&["config", "filter.kureha-test.clean", &clean]);
        // Same size, newer time: only reading the content can tell, which is
        // where git would run the clean filter.
        std::thread::sleep(Duration::from_millis(1100));
        repo.write("a.txt", "jello\n");
        let snapshot = repo.read("");
        assert!(!marker.exists(), "the repository's clean filter ran");
        assert_eq!(
            changes(&repo, &snapshot),
            [("a.txt".to_string(), Change::Modified)]
        );
    }

    #[test]
    fn diff_drivers_from_the_repositorys_config_never_run() {
        let repo = Repo::new("textconv");
        let marker = repo.0.with_extension("textconv-ran");
        let _ = std::fs::remove_file(&marker);
        repo.write(".gitattributes", "* diff=kureha-test\n");
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        repo.write("old.txt", &body);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        let textconv = format!("touch '{}'; cat", marker.display());
        repo.git(&["config", "diff.kureha-test.textconv", &textconv]);
        repo.git(&["config", "status.renames", "copies"]);
        // A staged rename with an edit: finding it compares contents.
        repo.git(&["mv", "old.txt", "new.txt"]);
        repo.write("new.txt", &format!("{body}one more\n"));
        repo.git(&["add", "new.txt"]);
        let snapshot = repo.read("");
        assert!(!marker.exists(), "the repository's textconv ran");
        assert_eq!(
            snapshot.change(&repo.0.join("new.txt")),
            Some(Change::Renamed)
        );
    }

    #[test]
    fn a_folder_inside_the_repository_sees_its_own_changes_and_counts_all() {
        let repo = Repo::new("nested");
        repo.write("top.txt", "a\n");
        repo.write("sub/inner.txt", "a\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.write("top.txt", "b\n");
        repo.write("sub/inner.txt", "b\n");
        let snapshot = repo.read("sub");
        assert_eq!(
            snapshot.changes.keys().collect::<Vec<_>>(),
            [&repo.0.join("sub").join("inner.txt")]
        );
        assert_eq!(snapshot.total, 2);
    }

    #[test]
    fn heads_and_folders_outside_repositories() {
        let repo = Repo::new("heads");
        assert_eq!(repo.read("").head, Head::Unborn("main".into()));
        assert_eq!(repo.read("").summary(), "main (no commits)");
        repo.write("a.txt", "a\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.git(&["checkout", "-q", "--detach"]);
        let Head::Detached(id) = repo.read("").head else {
            panic!("expected a detached head");
        };
        assert!(
            id.len() == 7 && id.chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
        let elsewhere =
            std::env::temp_dir().join(format!("kureha-git-none-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            read(&elsewhere, Arc::new(AtomicBool::new(false))),
            Outcome::NotRepository
        );
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn the_watcher_answers_the_newest_request() {
        let repo = Repo::new("watcher");
        repo.write("a.txt", "a\n");
        let watcher = Watcher::start().unwrap();
        for generation in 1..=3 {
            watcher.request(generation, repo.0.clone());
        }
        let until = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some((generation, folder, outcome)) = watcher.poll()
                && generation == 3
            {
                assert_eq!(folder, repo.0);
                assert!(matches!(outcome, Outcome::Ready(s) if s.total == 1));
                break;
            }
            assert!(std::time::Instant::now() < until, "no answer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Times `read` on real repositories:
/// `KUREHA_GIT_TIMING=/path/a:/path/b cargo test --release --lib git_read_timing -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn git_read_timing() {
    let paths = std::env::var_os("KUREHA_GIT_TIMING").expect("set KUREHA_GIT_TIMING");
    for path in std::env::split_paths(&paths) {
        for round in 1..=3 {
            let started = std::time::Instant::now();
            let outcome = read(&path, Arc::new(AtomicBool::new(false)));
            let summary = match &outcome {
                Outcome::Ready(snapshot) => snapshot.summary(),
                other => format!("{other:?}"),
            };
            println!(
                "{} round {round}: {:>7.1} ms  {summary}",
                path.display(),
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
