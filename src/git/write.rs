//! Staging and committing through the system Git command, on an ordered
//! worker. Git owns index/ref locking, hooks, filters, signing and conflicts.

use super::{STACK_SIZE, command};
use std::{
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Stage the work tree's version of each path (relative to the work tree):
    /// files, deleted files and untracked folders.
    Stage(Vec<PathBuf>),
    /// Put `HEAD`'s version of each path back into the index.
    Unstage(Vec<PathBuf>),
    /// Commit the index with this message.
    Commit(String),
    /// Create a branch at `HEAD`, and check it out if `switch`.
    CreateBranch { name: String, switch: bool },
    /// Switch to a branch; `keep` lists files (absolute) open with unsaved
    /// edits, which the switch must not change.
    Switch { name: String, keep: Vec<PathBuf> },
}

/// What an operation did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub message: String,
    /// Files (relative to the work tree) to reload after a checkout,
    /// including edits made by its hooks.
    pub changed: Vec<PathBuf>,
}
impl From<super::branch::Switched> for Done {
    fn from(switched: super::branch::Switched) -> Self {
        Self {
            message: switched.message,
            changed: switched.changed,
        }
    }
}
impl From<String> for Done {
    fn from(message: String) -> Self {
        Self {
            message,
            changed: Vec::new(),
        }
    }
}

/// Applies `operation` to the repository containing `folder`; path arguments
/// are relative to its work tree, even when the open folder is nested.
pub fn apply(folder: &Path, operation: &Operation) -> Result<Done, String> {
    let workdir = command::workdir(folder)?;
    match operation {
        Operation::Stage(paths) => stage(&workdir, paths).map(Done::from),
        Operation::Unstage(paths) => unstage(&workdir, paths).map(Done::from),
        Operation::Commit(message) => commit(&workdir, message).map(Done::from),
        Operation::CreateBranch { name, switch } => {
            super::branch::create(&workdir, name, *switch).map(Done::from)
        }
        Operation::Switch { name, keep } => {
            super::branch::switch(&workdir, name, keep).map(Done::from)
        }
    }
}

fn stage(workdir: &Path, paths: &[PathBuf]) -> Result<String, String> {
    // An empty selection must not turn into `git add -A` on the whole tree.
    if !paths.is_empty() {
        with_paths(workdir, &["add", "--all"], paths)?;
    }
    Ok("Staged changes".into())
}

fn unstage(workdir: &Path, paths: &[PathBuf]) -> Result<String, String> {
    if !paths.is_empty() {
        // With no explicit tree, reset also handles an unborn HEAD by
        // removing these entries from the index while keeping their files.
        with_paths(workdir, &["reset", "--quiet"], paths)?;
    }
    Ok("Unstaged changes".into())
}

/// Keep large selections below the OS argument limit, with literal path
/// bytes (including newlines and Unix non-UTF-8 names) in a NUL-delimited
/// file. The file is private and removed on success and failure alike.
fn with_paths(workdir: &Path, args: &[&str], paths: &[PathBuf]) -> Result<(), String> {
    struct PathspecFile(PathBuf);
    impl Drop for PathspecFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (pathspec, mut file) = loop {
        let path = std::env::temp_dir().join(format!(
            "kureha-git-pathspecs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => break (PathspecFile(path), file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot prepare Git paths: {e}")),
        }
    };
    {
        let mut writer = std::io::BufWriter::new(&mut file);
        for path in paths {
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.is_empty() || bytes.contains(&0) {
                return Err("Git paths must be nonempty and contain no NUL bytes".into());
            }
            writer
                .write_all(bytes)
                .and_then(|_| writer.write_all(&[0]))
                .map_err(|e| format!("Cannot prepare Git paths: {e}"))?;
        }
        writer
            .flush()
            .map_err(|e| format!("Cannot prepare Git paths: {e}"))?;
    }
    drop(file);
    let args = args.iter().copied().map(OsStr::new).chain([
        OsStr::new("--pathspec-from-file"),
        pathspec.0.as_os_str(),
        OsStr::new("--pathspec-file-nul"),
    ]);
    command::run(workdir, args).map(|_| ())
}

fn commit(workdir: &Path, message: &str) -> Result<String, String> {
    // Pass the original message: commit.cleanup and commit-msg hooks belong
    // to Git, as do signing and commits that finish a merge or cherry-pick.
    command::run(workdir, ["commit", "--message", message])?;
    Ok("Committed changes".into())
}

type Request = (u64, PathBuf, Operation);
type Answer = (u64, Operation, Result<Done, String>);

/// Applies operations on a worker thread, one at a time and in order; none
/// is skipped or interrupted.
pub struct Writer {
    requests: mpsc::Sender<Request>,
    answers: Mutex<mpsc::Receiver<Answer>>,
}
impl Writer {
    pub fn start() -> Result<Self, String> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, answers) = mpsc::channel();
        std::thread::Builder::new()
            .name("kureha-git-write".into())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                for (id, folder, operation) in inbox {
                    let result = apply(&folder, &operation);
                    if outbox.send((id, operation, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Cannot start the Git writer: {e}"))?;
        Ok(Self {
            requests,
            answers: Mutex::new(answers),
        })
    }
    pub fn submit(&self, id: u64, folder: PathBuf, operation: Operation) {
        let _ = self.requests.send((id, folder, operation));
    }
    /// Answers since the last poll, in order. Never blocks.
    pub fn poll(&self) -> Vec<Answer> {
        self.answers.lock().unwrap().try_iter().collect()
    }
}

#[cfg(all(test, not(target_os = "scarlet")))]
mod tests {
    use super::*;
    use crate::git::{Change, testing::Repo};

    fn stage_all(repo: &Repo) -> String {
        let snapshot = repo.read("");
        let paths = snapshot.unstaged.iter().map(|e| e.path.clone()).collect();
        apply(&repo.0, &Operation::Stage(paths)).unwrap().message
    }

    /// What `git add` would store for `path`: mode and blob id.
    fn expected(repo: &Repo, path: &str, mode: &str) -> String {
        let id = if mode == "120000" {
            let target = std::fs::read_link(repo.0.join(path)).unwrap();
            let link = repo.0.with_extension("link-target");
            std::fs::write(&link, target.to_str().unwrap()).unwrap();
            let id = repo.git(&["hash-object", "-w", link.to_str().unwrap()]);
            let _ = std::fs::remove_file(link);
            id
        } else {
            repo.git(&["hash-object", "-w", "--path", path, path])
        };
        format!("{mode} {} 0\t{path}", id.trim())
    }

    #[cfg(unix)]
    #[test]
    fn staging_and_committing_match_git() {
        use std::os::unix::fs::PermissionsExt;
        let repo = Repo::new("write");
        repo.write(".gitattributes", "*.txt text eol=lf\n");
        repo.write("a.txt", "a\n");
        repo.write("script.sh", "echo hi\n");
        repo.write("sub/deep.txt", "deep\n");
        repo.write("gone.txt", "gone\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        // A submodule's commit, without the submodule itself.
        let head = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},lib", head.trim()),
        ]);
        repo.git(&["commit", "-q", "-m", "gitlink"]);
        // As an uninitialised submodule looks.
        std::fs::create_dir(repo.0.join("lib")).unwrap();

        repo.write("a.txt", "a\r\nchanged\r\n");
        repo.write("new.txt", "new\n");
        repo.write("fresh/x.txt", "x\n");
        repo.write("fresh/nested/y.txt", "y\n");
        repo.write("fresh/skip.log", "log\n");
        repo.write(".gitignore", "*.log\n");
        std::fs::remove_file(repo.0.join("sub/deep.txt")).unwrap();
        let script = repo.0.join("script.sh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", repo.0.join("link")).unwrap();

        stage_all(&repo);
        assert_eq!(
            repo.git(&["status", "--porcelain"]),
            "A  .gitignore\nM  a.txt\nA  fresh/nested/y.txt\nA  fresh/x.txt\n\
             A  link\nA  new.txt\nM  script.sh\nD  sub/deep.txt\n"
        );
        let staged = repo.git(&[
            "ls-files",
            "-s",
            "a.txt",
            "script.sh",
            "link",
            "fresh",
            "lib",
        ]);
        let gitlink = format!("160000 {} 0\tlib", head.trim());
        assert_eq!(
            staged.lines().collect::<Vec<_>>(),
            [
                expected(&repo, "a.txt", "100644"),
                expected(&repo, "fresh/nested/y.txt", "100644"),
                expected(&repo, "fresh/x.txt", "100644"),
                gitlink.clone(),
                expected(&repo, "link", "120000"),
                expected(&repo, "script.sh", "100755"),
            ]
        );

        let answer = apply(
            &repo.0,
            &Operation::Commit("Subject  \n\n\nBody\n\n".into()),
        )
        .unwrap()
        .message;
        assert!(answer.starts_with("Committed "), "{answer}");
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        assert_eq!(
            repo.git(&["write-tree"]),
            repo.git(&["rev-parse", "HEAD^{tree}"])
        );
        assert_eq!(
            repo.git(&["log", "-1", "--format=%B%an <%ae>"]),
            "Subject\n\nBody\nTest <test@example.invalid>\n"
        );
        assert_eq!(
            repo.git(&["rev-parse", "HEAD~1"]).trim(),
            repo.git(&["rev-parse", "main~1"]).trim()
        );
        assert!(
            repo.git(&["ls-tree", "HEAD", "lib"]).contains(
                &gitlink
                    .replace(" 0\t", "\t")
                    .replacen("160000 ", "160000 commit ", 1)
            )
        );
        repo.git(&["fsck", "--no-dangling"]);
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[test]
    fn unstaging_restores_head_in_the_index() {
        let repo = Repo::new("unstage");
        repo.write("a.txt", "a\n");
        repo.write("b.txt", "b\n");
        repo.write("old.txt", "some longer content to rename\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo.write("a.txt", "changed\n");
        repo.write("new.txt", "new\n");
        repo.git(&["add", "a.txt", "new.txt"]);
        repo.git(&["rm", "-q", "b.txt"]);
        repo.git(&["mv", "old.txt", "renamed.txt"]);
        let snapshot = repo.read("");
        let mut paths = Vec::new();
        for entry in &snapshot.staged {
            paths.push(entry.path.clone());
            paths.extend(entry.source.clone());
        }
        assert!(
            snapshot.staged.iter().any(|e| e.change == Change::Renamed
                && e.source.as_deref() == Some(Path::new("old.txt")))
        );
        apply(&repo.0, &Operation::Unstage(paths)).unwrap();
        assert_eq!(
            repo.git(&["status", "--porcelain"]),
            " M a.txt\n D b.txt\n D old.txt\n?? new.txt\n?? renamed.txt\n"
        );
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "");
    }

    #[test]
    fn first_commit_on_an_unborn_branch_and_in_a_linked_worktree() {
        let repo = Repo::new("unborn");
        repo.write("a.txt", "a\n");
        stage_all(&repo);
        apply(&repo.0, &Operation::Unstage(vec!["a.txt".into()])).unwrap();
        assert_eq!(repo.git(&["ls-files"]), "");
        assert_eq!(
            std::fs::read_to_string(repo.0.join("a.txt")).unwrap(),
            "a\n"
        );
        stage_all(&repo);
        apply(&repo.0, &Operation::Commit("first".into())).unwrap();
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]).trim(), "1");
        assert_eq!(repo.git(&["status", "--porcelain"]), "");

        let linked = Repo(repo.0.with_extension("linked"));
        repo.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.0.to_str().unwrap(),
        ]);
        std::fs::write(linked.0.join("a.txt"), "side\n").unwrap();
        apply(&linked.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        apply(&linked.0, &Operation::Commit("on side".into())).unwrap();
        assert_eq!(repo.git_in(&linked.0, &["status", "--porcelain"]), "");
        assert_eq!(repo.git(&["log", "-1", "--format=%s", "side"]), "on side\n");
        assert_eq!(repo.git(&["log", "-1", "--format=%s", "main"]), "first\n");
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        repo.git(&["worktree", "remove", linked.0.to_str().unwrap()]);
    }

    #[test]
    fn git_errors_preserve_head_index_entries_and_files() {
        let repo = Repo::new("write-git-errors");
        repo.write("a.txt", "a\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        let before = repo.git(&["ls-files", "--stage"]);
        let head = repo.git(&["rev-parse", "HEAD"]);
        let error = apply(&repo.0, &Operation::Commit("nothing".into())).unwrap_err();
        assert!(error.contains("nothing to commit"), "{error}");
        repo.write("a.txt", "b\n");
        std::fs::write(repo.0.join(".git/index.lock"), "").unwrap();
        let locked = apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap_err();
        assert!(locked.contains("index.lock"), "{locked}");
        assert_eq!(repo.git(&["ls-files", "--stage"]), before);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(repo.0.join("a.txt")).unwrap(),
            "b\n"
        );
        std::fs::remove_file(repo.0.join(".git/index.lock")).unwrap();
        // An invalid pathspec must not stage the valid path beside it.
        assert!(
            apply(
                &repo.0,
                &Operation::Stage(vec!["a.txt".into(), "missing".into()])
            )
            .is_err()
        );
        assert_eq!(repo.git(&["ls-files", "--stage"]), before);
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        let staged = repo.git(&["ls-files", "--stage"]);
        let error = apply(&repo.0, &Operation::Commit(" \n\n".into())).unwrap_err();
        assert!(error.contains("empty commit message"), "{error}");
        assert_ne!(before, staged);
        assert_eq!(repo.git(&["ls-files", "--stage"]), staged);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[cfg(unix)]
    fn script(repo: &Repo, path: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        repo.write(path, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(repo.0.join(path), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn commit_hooks_run_and_failed_pre_commit_preserves_the_commit_and_index() {
        let repo = Repo::new("write-hooks");
        repo.git(&["config", "core.hooksPath", ".git/custom-hooks"]);
        script(
            &repo,
            ".git/custom-hooks/pre-commit",
            "printf 'ran\\n' > .git/pre-commit-ran",
        );
        script(
            &repo,
            ".git/custom-hooks/commit-msg",
            "printf '\\nHook-Trailer: yes\\n' >> \"$1\"",
        );
        script(
            &repo,
            ".git/custom-hooks/post-commit",
            "printf 'ran\\n' > .git/post-commit-ran",
        );
        repo.write("a.txt", "first\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        apply(&repo.0, &Operation::Commit("subject".into())).unwrap();
        assert!(repo.0.join(".git/pre-commit-ran").exists());
        assert!(repo.0.join(".git/post-commit-ran").exists());
        assert!(
            repo.git(&["log", "-1", "--format=%B"])
                .contains("Hook-Trailer: yes")
        );
        repo.write("a.txt", "second\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        script(
            &repo,
            ".git/custom-hooks/pre-commit",
            "echo 'rejected by fixture hook' >&2\nexit 1",
        );
        let head = repo.git(&["rev-parse", "HEAD"]);
        let staged = repo.git(&["ls-files", "--stage"]);
        let error = apply(&repo.0, &Operation::Commit("rejected".into())).unwrap_err();
        assert!(error.contains("rejected by fixture hook"), "{error}");
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(repo.git(&["ls-files", "--stage"]), staged);
        assert_eq!(
            std::fs::read_to_string(repo.0.join("a.txt")).unwrap(),
            "second\n"
        );
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn repository_clean_filters_run_and_failures_leave_the_index_alone() {
        let repo = Repo::new("write-filters");
        repo.git(&["config", "filter.upper.clean", "tr a-z A-Z"]);
        repo.git(&["config", "filter.upper.required", "true"]);
        repo.write(".gitattributes", "*.txt filter=upper\n");
        repo.write("a.txt", "lower case\n");
        apply(
            &repo.0,
            &Operation::Stage(vec![".gitattributes".into(), "a.txt".into()]),
        )
        .unwrap();
        assert_eq!(repo.git(&["show", ":a.txt"]), "LOWER CASE\n");
        apply(&repo.0, &Operation::Commit("filtered".into())).unwrap();
        assert_eq!(repo.git(&["show", "HEAD:a.txt"]), "LOWER CASE\n");
        let index = repo.git(&["ls-files", "--stage"]);
        let head = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["config", "filter.upper.clean", "exit 1"]);
        repo.write("a.txt", "changed\n");
        let error = apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap_err();
        assert!(error.contains("filter"), "{error}");
        assert_eq!(repo.git(&["ls-files", "--stage"]), index);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn git_uses_the_configured_signer_and_propagates_signing_failures() {
        let repo = Repo::new("write-signing");
        repo.write("a.txt", "first\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        // A deterministic signer fixture exercises Git's signing protocol;
        // no developer key or external keyring is needed by the test.
        script(
            &repo,
            ".git/test-signer",
            "cat > .git/signing-input\nprintf '%s\\n' '[GNUPG:] SIG_CREATED D 1 10 00 0 TEST' >&2\nprintf '%s\\n' '-----BEGIN PGP SIGNATURE-----' 'fixture-signature' '-----END PGP SIGNATURE-----'",
        );
        repo.git(&["config", "gpg.format", "openpgp"]);
        repo.git(&[
            "config",
            "gpg.program",
            repo.0.join(".git/test-signer").to_str().unwrap(),
        ]);
        repo.git(&["config", "user.signingkey", "fixture-key"]);
        repo.git(&["config", "commit.gpgsign", "true"]);
        apply(&repo.0, &Operation::Commit("signed".into())).unwrap();
        let object = repo.git(&["cat-file", "commit", "HEAD"]);
        assert!(
            object.contains("gpgsig -----BEGIN PGP SIGNATURE-----\n fixture-signature\n"),
            "{object}"
        );
        assert!(
            std::fs::read_to_string(repo.0.join(".git/signing-input"))
                .unwrap()
                .contains("signed\n")
        );
        repo.write("a.txt", "second\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        let head = repo.git(&["rev-parse", "HEAD"]);
        let staged = repo.git(&["ls-files", "--stage"]);
        script(
            &repo,
            ".git/test-signer",
            "cat > .git/signing-input\necho 'fixture signing failed' >&2\nexit 1",
        );
        let error = apply(&repo.0, &Operation::Commit("rejected signature".into())).unwrap_err();
        assert!(error.contains("failed to sign"), "{error}");
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(repo.git(&["ls-files", "--stage"]), staged);
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[test]
    fn git_handles_conflicts_and_commits_a_resolved_merge() {
        let repo = Repo::new("write-merge");
        repo.write("a.txt", "base\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.git(&["switch", "-q", "-c", "side"]);
        repo.write("a.txt", "side\n");
        repo.git(&["commit", "-q", "-am", "side"]);
        repo.git(&["switch", "-q", "main"]);
        repo.write("a.txt", "main\n");
        repo.git(&["commit", "-q", "-am", "main"]);
        assert!(
            !command::output(&repo.0, ["merge", "--no-edit", "side"])
                .unwrap()
                .status
                .success()
        );
        let head = repo.git(&["rev-parse", "HEAD"]);
        let conflicts = repo.git(&["ls-files", "--unmerged"]);
        assert!(!conflicts.is_empty());
        assert!(apply(&repo.0, &Operation::Commit("unresolved".into())).is_err());
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(repo.git(&["ls-files", "--unmerged"]), conflicts);
        // Resolving to our existing content still needs a merge commit,
        // even though the final tree is identical to its first parent's.
        repo.write("a.txt", "main\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        apply(&repo.0, &Operation::Commit("resolved merge".into())).unwrap();
        assert_eq!(
            repo.git(&["rev-list", "--parents", "-n", "1", "HEAD"])
                .split_whitespace()
                .count(),
            3
        );
        assert_eq!(repo.git(&["diff", "HEAD^", "HEAD", "--name-only"]), "");
        assert!(!repo.0.join(".git/MERGE_HEAD").exists());
    }

    #[test]
    fn commit_cleanup_is_left_to_git_configuration() {
        let repo = Repo::new("write-cleanup");
        repo.git(&["config", "commit.cleanup", "verbatim"]);
        repo.write("a.txt", "a\n");
        apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        let message = "Subject  \n\n\nBody  \n";
        apply(&repo.0, &Operation::Commit(message.into())).unwrap();
        let object = repo.git(&["cat-file", "commit", "HEAD"]);
        assert_eq!(object.split_once("\n\n").unwrap().1, message);
    }

    #[test]
    fn empty_selections_are_noops_and_large_selections_fit() {
        let repo = Repo::new("write-large-selection");
        let name = "x".repeat(220);
        repo.write(&name, "selected\n");
        repo.write("other.txt", "not selected\n");
        apply(&repo.0, &Operation::Stage(vec![])).unwrap();
        assert_eq!(repo.git(&["ls-files"]), "");
        // More than 2 MiB of path arguments would exceed common ARG_MAX
        // limits; duplicate paths keep the fixture small on disk.
        let paths = vec![PathBuf::from(&name); 12_000];
        apply(&repo.0, &Operation::Stage(paths.clone())).unwrap();
        assert_eq!(repo.git(&["ls-files"]), format!("{name}\n"));
        apply(&repo.0, &Operation::Unstage(vec![])).unwrap();
        assert_eq!(repo.git(&["ls-files"]), format!("{name}\n"));
        apply(&repo.0, &Operation::Unstage(paths)).unwrap();
        assert_eq!(repo.git(&["ls-files"]), "");
        assert!(repo.0.join(name).exists());
    }

    #[cfg(unix)]
    #[test]
    fn staging_uses_literal_raw_paths_from_the_worktree_root() {
        #[cfg(not(target_os = "macos"))]
        use std::os::unix::ffi::OsStringExt;
        let repo = Repo::new("write-literal-paths");
        let mut paths: Vec<PathBuf> = [
            "[glob].txt",
            ":(top)literal",
            "--option",
            "line\nbreak",
            "\"quoted",
            "trailing ",
            "日本語.txt",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        // APFS rejects invalid UTF-8 names at creation time.
        #[cfg(not(target_os = "macos"))]
        paths.push(std::ffi::OsString::from_vec(b"non-utf8-\xff.txt".to_vec()).into());
        for path in &paths {
            std::fs::write(repo.0.join(path), "selected\n").unwrap();
        }
        repo.write("g.txt", "a glob would select this too\n");
        repo.write("unselected.txt", "not selected\n");
        let nested = repo.0.join("open/nested");
        std::fs::create_dir_all(&nested).unwrap();
        apply(&nested, &Operation::Stage(paths.clone())).unwrap();
        let index = command::run(&repo.0, ["ls-files", "-z"]).unwrap();
        let mut actual: Vec<_> = index
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| command::path(p).unwrap())
            .collect();
        paths.sort();
        actual.sort();
        assert_eq!(actual, paths);
        apply(&nested, &Operation::Unstage(paths.clone())).unwrap();
        assert!(
            command::run(&repo.0, ["ls-files", "-z"])
                .unwrap()
                .is_empty()
        );
        for path in &paths {
            assert_eq!(std::fs::read(repo.0.join(path)).unwrap(), b"selected\n");
        }
    }

    #[test]
    fn split_and_sparse_indexes_are_handled_by_git() {
        let repo = Repo::new("write-split-sparse");
        repo.write("included/a.txt", "a\n");
        repo.write("outside/b.txt", "b\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "initial"]);
        repo.git(&["update-index", "--split-index"]);
        repo.write("included/a.txt", "split\n");
        apply(&repo.0, &Operation::Stage(vec!["included/a.txt".into()])).unwrap();
        apply(&repo.0, &Operation::Commit("split index".into())).unwrap();
        assert_eq!(repo.git(&["show", "HEAD:included/a.txt"]), "split\n");
        repo.git(&["update-index", "--no-split-index"]);
        repo.git(&["sparse-checkout", "init", "--cone", "--sparse-index"]);
        repo.git(&["sparse-checkout", "set", "included"]);
        assert!(!repo.0.join("outside/b.txt").exists());
        repo.write("included/a.txt", "sparse\n");
        apply(&repo.0, &Operation::Stage(vec!["included/a.txt".into()])).unwrap();
        apply(&repo.0, &Operation::Unstage(vec!["included/a.txt".into()])).unwrap();
        assert_eq!(repo.git(&["show", ":included/a.txt"]), "split\n");
        apply(&repo.0, &Operation::Stage(vec!["included/a.txt".into()])).unwrap();
        apply(&repo.0, &Operation::Commit("sparse index".into())).unwrap();
        assert_eq!(repo.git(&["show", "HEAD:included/a.txt"]), "sparse\n");
        assert_eq!(repo.git(&["show", "HEAD:outside/b.txt"]), "b\n");
    }
}
