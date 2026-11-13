//! Git state for the workbench, read by the installed `git` command on a
//! worker thread. Repository settings, including filters and fsmonitor, follow
//! Git's normal behavior. Git integration is disabled on Scarlet and when the
//! executable is unavailable. Background reads do not refresh the index.

pub mod branch;
mod command;
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
    /// A merge, rebase or similar is in progress.
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
    /// No Git backend is available; stop background polling.
    Unavailable(String),
    NotRepository,
    Ready(Snapshot),
    Failed(String),
}

/// Reads the repository containing `folder`. Stops early, with an error, when
/// `interrupt` is set.
pub fn read(folder: &Path, interrupt: Arc<AtomicBool>) -> Outcome {
    if let Err(reason) = command::availability() {
        return Outcome::Unavailable(reason);
    }
    // Skip discovery processes for ordinary folders. Linked worktrees use a
    // .git file; Git itself resolves and validates it below.
    if !folder.ancestors().any(|dir| dir.join(".git").exists()) {
        return Outcome::NotRepository;
    }
    match snapshot(folder, &interrupt) {
        Ok(Some(snapshot)) => Outcome::Ready(snapshot),
        Ok(None) => Outcome::NotRepository,
        Err(error) => Outcome::Failed(error),
    }
}

fn snapshot(folder: &Path, interrupt: &AtomicBool) -> Result<Option<Snapshot>, String> {
    if command::read(folder, ["rev-parse", "--is-inside-work-tree"], interrupt)? != b"true\n" {
        return Ok(None);
    }
    let workdir = command::workdir(folder)?;
    let output = command::read(
        &workdir,
        [
            "status",
            "--porcelain=v2",
            "--branch",
            "--no-ahead-behind",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
            "--renames",
        ],
        interrupt,
    )?;
    let mut snapshot = parse_status(folder, &workdir, &output)?;
    if interrupt.load(Ordering::Relaxed) {
        return Err("Git read interrupted".into());
    }
    snapshot.branches = branch::list(&workdir)?;
    let git_dir = command::read(&workdir, ["rev-parse", "--absolute-git-dir"], interrupt)?;
    let git_dir = command::path(git_dir.strip_suffix(b"\n").unwrap_or(&git_dir))?;
    snapshot.in_progress = [
        ("rebase-merge", "rebasing"),
        ("rebase-apply", "rebasing or applying patches"),
        ("MERGE_HEAD", "merging"),
        ("CHERRY_PICK_HEAD", "cherry-picking"),
        ("REVERT_HEAD", "reverting"),
        ("BISECT_LOG", "bisecting"),
    ]
    .into_iter()
    .find_map(|(file, label)| git_dir.join(file).exists().then(|| label.into()));
    Ok(Some(snapshot))
}

/// Porcelain v2 uses NUL records and fixed field counts; filenames can contain
/// spaces, newlines and (on Unix) arbitrary bytes.
fn parse_status(folder: &Path, workdir: &Path, output: &[u8]) -> Result<Snapshot, String> {
    let resolved = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let inside = resolved(folder)
        .strip_prefix(resolved(workdir))
        .map(Path::to_path_buf)
        .map_err(|_| "The folder is not inside the repository's work tree")?;
    let mut snapshot = Snapshot {
        head: Head::Unborn(String::new()),
        changes: BTreeMap::new(),
        untracked_folders: Vec::new(),
        total: 0,
        workdir: workdir.to_path_buf(),
        staged: Vec::new(),
        unstaged: Vec::new(),
        in_progress: None,
        branches: Vec::new(),
    };
    let mut head = None;
    let mut oid = None;
    let mut seen = std::collections::HashSet::new();
    let mut records = output
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(value) = record.strip_prefix(b"# branch.head ") {
            head = Some(String::from_utf8_lossy(value).into_owned());
            continue;
        }
        if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            oid = Some(String::from_utf8_lossy(value).into_owned());
            continue;
        }
        if record.starts_with(b"# ") || record.starts_with(b"! ") {
            continue;
        }
        let (path, source, xy, conflict, is_folder) = match record.first() {
            Some(b'?') if record.starts_with(b"? ") => {
                let raw = &record[2..];
                let folder = raw.ends_with(b"/");
                (
                    command::path(raw.strip_suffix(b"/").unwrap_or(raw))?,
                    None,
                    *b".?",
                    false,
                    folder,
                )
            }
            Some(kind @ (b'1' | b'2' | b'u')) => {
                let count = match kind {
                    b'1' => 9,
                    b'2' => 10,
                    _ => 11,
                };
                let fields: Vec<_> = record.splitn(count, |b| *b == b' ').collect();
                if fields.len() != count || fields[1].len() != 2 || fields[count - 1].is_empty() {
                    return Err("Invalid Git status record".into());
                }
                let source = if *kind == b'2' {
                    Some(command::path(
                        records.next().ok_or("Missing Git rename source")?,
                    )?)
                } else {
                    None
                };
                (
                    command::path(fields[count - 1])?,
                    source,
                    [fields[1][0], fields[1][1]],
                    *kind == b'u',
                    false,
                )
            }
            _ => return Err("Invalid Git status record".into()),
        };
        // `git rm --cached` can report the same path as both a staged
        // deletion and an untracked file. Count it once, as the UI always has.
        if seen.insert(path.clone()) {
            snapshot.total += 1;
        }
        let staged = if conflict { None } else { status_change(xy[0]) };
        let unstaged = if conflict {
            Some(Change::Conflicted)
        } else {
            status_change(xy[1])
        };
        for (change, list, source) in [
            (staged, &mut snapshot.staged, source.clone()),
            (
                unstaged,
                &mut snapshot.unstaged,
                if matches!(xy[1], b'R' | b'C') {
                    source
                } else {
                    None
                },
            ),
        ] {
            if let Some(change) = change {
                if list.len() < MAX_CHANGES {
                    list.push(Entry {
                        path: path.clone(),
                        change,
                        source,
                        folder: is_folder,
                    });
                }
            }
        }
        let Some(change) = staged.max(unstaged) else {
            continue;
        };
        // When the opened folder is inside a collapsed untracked directory,
        // every file under that opened folder still needs the untracked mark.
        if is_folder && inside.starts_with(&path) {
            if snapshot.untracked_folders.len() < MAX_CHANGES {
                snapshot.untracked_folders.push(folder.to_path_buf());
            }
        } else if let Ok(relative) = path.strip_prefix(&inside) {
            let full = folder.join(relative);
            if is_folder {
                if snapshot.untracked_folders.len() < MAX_CHANGES {
                    snapshot.untracked_folders.push(full);
                }
            } else if snapshot.changes.len() < MAX_CHANGES || snapshot.changes.contains_key(&full) {
                let mark = snapshot.changes.entry(full).or_insert(change);
                *mark = (*mark).max(change);
            }
        }
    }
    let head = head.ok_or("Git did not report HEAD")?;
    let oid = oid.ok_or("Git did not report the HEAD object")?;
    snapshot.head = if head == "(detached)" {
        Head::Detached(oid.chars().take(7).collect())
    } else if oid == "(initial)" {
        Head::Unborn(head)
    } else {
        Head::Branch(head)
    };
    snapshot.staged.sort_by(|a, b| a.path.cmp(&b.path));
    snapshot.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(snapshot)
}

fn status_change(code: u8) -> Option<Change> {
    match code {
        b'A' => Some(Change::Added),
        b'D' => Some(Change::Deleted),
        b'M' | b'T' => Some(Change::Modified),
        b'R' | b'C' => Some(Change::Renamed),
        b'U' => Some(Change::Conflicted),
        b'?' => Some(Change::Untracked),
        _ => None,
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

impl Drop for Watcher {
    fn drop(&mut self) {
        self.interrupt.store(true, Ordering::Relaxed);
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
    fn status_uses_repository_filters_like_git() {
        let repo = Repo::new("filter");
        let marker = repo.0.join(".git/filter-ran");
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
        assert!(marker.exists(), "git status should honor the clean filter");
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
    fn an_open_folder_inside_an_untracked_directory_is_marked() {
        let repo = Repo::new("untracked-inside");
        repo.write("fresh/nested/a.txt", "new\n");
        let snapshot = repo.read("fresh/nested");
        assert_eq!(snapshot.total, 1);
        assert_eq!(
            snapshot.change(&repo.0.join("fresh/nested/a.txt")),
            Some(Change::Untracked)
        );
    }

    #[test]
    fn staged_deletion_and_untracked_copy_count_once() {
        let repo = Repo::new("deleted-and-untracked");
        repo.write("a.txt", "keep on disk\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "initial"]);
        repo.git(&["rm", "--cached", "a.txt"]);
        let snapshot = repo.read("");
        assert_eq!(snapshot.total, 1);
        assert_eq!(snapshot.staged[0].change, Change::Deleted);
        assert_eq!(snapshot.unstaged[0].change, Change::Untracked);
        assert_eq!(
            snapshot.change(&repo.0.join("a.txt")),
            Some(Change::Deleted)
        );
    }

    #[cfg(unix)]
    #[test]
    fn status_preserves_unusual_path_bytes_and_rename_sources() {
        use std::os::unix::ffi::OsStrExt;
        let repo = Repo::new("status paths\nwith newline");
        repo.write("old\nname.txt", "rename me\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "initial"]);
        repo.git(&["mv", "old\nname.txt", "new\t name.txt"]);
        let name = std::ffi::OsStr::new("日本語.txt");
        std::fs::write(repo.0.join(name), "new\n").unwrap();
        repo.write(":(glob)*.txt", "literal\n");
        let snapshot = repo.read("");
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.staged[0].path, Path::new("new\t name.txt"));
        assert_eq!(
            snapshot.staged[0].source.as_deref(),
            Some(Path::new("old\nname.txt"))
        );
        assert_eq!(snapshot.change(&repo.0.join(name)), Some(Change::Untracked));
        // APFS rejects non-UTF-8 filenames; exercise the NUL parser directly
        // for byte sequences that filesystems such as ext4 can store.
        let raw = b"# branch.head main\0# branch.oid (initial)\0? non-utf8-\xff.txt\0";
        let parsed = parse_status(&repo.0, &repo.0, raw).unwrap();
        assert_eq!(
            parsed.unstaged[0].path.as_os_str().as_bytes(),
            b"non-utf8-\xff.txt"
        );
    }

    #[test]
    fn merge_conflicts_are_reported_once_and_can_be_resolved() {
        let repo = Repo::new("status-conflict");
        repo.write("a.txt", "base\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "base"]);
        repo.git(&["switch", "-c", "side"]);
        repo.write("a.txt", "side\n");
        repo.git(&["commit", "-qam", "side"]);
        repo.git(&["switch", "main"]);
        repo.write("a.txt", "main\n");
        repo.git(&["commit", "-qam", "main"]);
        assert!(
            !command::output(&repo.0, ["merge", "--no-edit", "side"])
                .unwrap()
                .status
                .success()
        );
        let snapshot = repo.read("");
        assert_eq!(snapshot.total, 1);
        assert_eq!(snapshot.in_progress.as_deref(), Some("merging"));
        assert_eq!(snapshot.unstaged[0].change, Change::Conflicted);
        repo.write("a.txt", "resolved\n");
        write::apply(&repo.0, &write::Operation::Stage(vec!["a.txt".into()])).unwrap();
        write::apply(&repo.0, &write::Operation::Commit("resolve merge".into())).unwrap();
        assert_eq!(repo.read("").total, 0);
        assert_eq!(
            repo.git(&["rev-list", "--parents", "-1", "HEAD"])
                .split_whitespace()
                .count(),
            3
        );
    }

    #[test]
    fn missing_git_disables_the_backend() {
        const CHILD: &str = "KUREHA_TEST_GIT_MISSING";
        if std::env::var_os(CHILD).is_some() {
            let folder = Path::new(".");
            assert_eq!(
                read(folder, Arc::new(AtomicBool::new(false))),
                Outcome::Unavailable(command::MISSING.into())
            );
            assert_eq!(
                write::apply(folder, &write::Operation::Commit("test".into())).unwrap_err(),
                command::MISSING
            );
            assert_eq!(
                diff::read(folder, Path::new("Cargo.toml"), diff::Side::Unstaged).unwrap_err(),
                command::MISSING
            );
            return;
        }
        // Change PATH only in a fresh test process, so parallel tests and the
        // process-wide Git availability cache remain independent.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::tests::missing_git_disables_the_backend",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
