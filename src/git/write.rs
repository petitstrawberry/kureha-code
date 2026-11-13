//! Staging and committing with gix, on a worker that runs requests in order.
//!
//! Every change to the index takes `index.lock` first and reads the index
//! again under it, then writes through the lock, as Git does: a `git` command
//! running at the same time either makes Kureha fail or fails itself, and
//! neither overwrites the other. A commit moves `HEAD` only if it still points
//! where it did.
//!
//! Not supported, and refused rather than done differently from Git:
//! repositories that define their own filters in `.git/config` (Kureha never
//! runs those, so staged content would differ), signed commits, commits during
//! a merge or rebase, conflicts, split and sparse indexes. Hooks are not run.

use super::{STACK_SIZE, imp};
use gix::{
    bstr::ByteSlice,
    index::entry::{Flags, Mode, Stage, Stat},
};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
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
}

/// Applies `operation` to the repository containing `folder`; the answer
/// says what was done.
pub fn apply(folder: &Path, operation: &Operation) -> Result<String, String> {
    let repo = imp::open(folder)?.ok_or("Not a Git repository")?;
    match operation {
        Operation::Stage(paths) => stage(&repo, paths),
        Operation::Unstage(paths) => unstage(&repo, paths),
        Operation::Commit(message) => commit(&repo, message),
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn rela(path: &Path) -> Result<&str, String> {
    path.to_str()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| format!("{}: only UTF-8 paths are supported", path.display()))
}

/// The index, read again while holding its lock.
struct Locked {
    lock: gix::lock::File,
    index: gix::index::File,
}

fn lock_index(repo: &gix::Repository) -> Result<Locked, String> {
    let path = repo.index_path();
    let lock = gix::lock::File::acquire_to_update_resource(
        &path,
        gix::lock::acquire::Fail::Immediately,
        None,
        0,
    )
    .map_err(|e| {
        let mut lock = path.clone().into_os_string();
        lock.push(".lock");
        if Path::new(&lock).exists() {
            "The index is locked: another Git command is running, or one left index.lock behind"
                .to_string()
        } else {
            format!("Cannot lock the index: {e:?}")
        }
    })?;
    let index =
        gix::index::File::at_or_default(&path, repo.object_hash(), false, Default::default())
            .map_err(err)?;
    if index.link().is_some() {
        return Err("Split indexes are not supported; use git".into());
    }
    if index.is_sparse() {
        return Err("Sparse indexes are not supported; use git".into());
    }
    Ok(Locked { lock, index })
}

impl Locked {
    fn write(mut self) -> Result<(), String> {
        use std::io::Write;
        // gix writes the tree cache as it was read, which would no longer
        // match the entries; Git rebuilds it when needed.
        self.index.remove_tree();
        let mut out = std::io::BufWriter::new(&mut self.lock);
        self.index
            .write_to(&mut out, Default::default())
            .map_err(err)?;
        out.flush().map_err(err)?;
        drop(out);
        self.lock.commit().map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

/// Filters defined in the repository's own config never run in Kureha, so
/// staging there would record different content than `git add`.
fn refuse_repository_filters(repo: &gix::Repository) -> Result<(), String> {
    let config = repo.config_snapshot();
    let defined = config
        .plumbing()
        .sections_by_name("filter")
        .into_iter()
        .flatten()
        .any(|section| {
            section.meta().source.kind() == gix::config::source::Kind::Repository
                && ["clean", "process"]
                    .iter()
                    .any(|key| section.contains_value_name(key))
        });
    if defined {
        Err("This repository defines Git filters in its own config, which Kureha does not run; stage with git".into())
    } else {
        Ok(())
    }
}

/// Untracked files below `folder`, honouring ignore rules and leaving nested
/// repositories out.
fn untracked_below(repo: &gix::Repository, folder: &str) -> Result<Vec<PathBuf>, String> {
    use gix::status::index_worktree::Item;
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(err)?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(None)
        .index_worktree_rewrites(None)
        .into_index_worktree_iter(vec![format!(":(literal){folder}").into()])
        .map_err(err)?;
    let mut files = Vec::new();
    for item in iter {
        if let Item::DirectoryContents { entry, .. } = item.map_err(err)?
            && entry.status == gix::dir::entry::Status::Untracked
            && matches!(
                entry.disk_kind,
                Some(gix::dir::entry::Kind::File | gix::dir::entry::Kind::Symlink)
            )
        {
            files.push(
                gix::path::from_bstr(entry.rela_path.as_bstr())
                    .map_err(err)?
                    .into_owned(),
            );
        }
    }
    Ok(files)
}

fn stage(repo: &gix::Repository, paths: &[PathBuf]) -> Result<String, String> {
    refuse_repository_filters(repo)?;
    let workdir = repo
        .workdir()
        .ok_or("The repository has no work tree")?
        .to_path_buf();
    let mut locked = lock_index(repo)?;
    let mut files = Vec::new();
    for path in paths {
        let metadata = std::fs::symlink_metadata(workdir.join(path));
        if metadata.is_ok_and(|m| m.is_dir()) {
            files.extend(untracked_below(repo, rela(path)?)?);
        } else {
            files.push(path.clone());
        }
    }
    let capabilities = repo.filesystem_options().map_err(err)?;
    let (mut pipeline, _) = repo.filter_pipeline(None).map_err(err)?;
    let mut staged = 0;
    for path in &files {
        let name = rela(path)?;
        let full = workdir.join(path);
        let metadata = match gix::index::fs::Metadata::from_path_no_follow(&full) {
            Ok(metadata) => Some(metadata),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let Some(metadata) = metadata else {
            // Deleted in the work tree: the deletion is staged.
            locked
                .index
                .remove_entries(|_, entry_path, _| entry_path == name);
            staged += 1;
            continue;
        };
        if metadata.is_dir() {
            continue; // A submodule or nested repository.
        }
        let existing = locked
            .index
            .entry_by_path_and_stage(name.into(), Stage::Unconflicted)
            .map(|entry| entry.mode);
        let (data, mode) = if metadata.is_symlink() && capabilities.symlink {
            let target = std::fs::read_link(&full).map_err(|e| format!("{name}: {e}"))?;
            (
                gix::path::into_bstr(target).map_err(err)?.to_vec(),
                Mode::SYMLINK,
            )
        } else {
            let file = std::fs::File::open(&full).map_err(|e| format!("{name}: {e}"))?;
            let data = super::diff::read_converted(&mut pipeline, file, path, &locked.index)?;
            let executable = if capabilities.executable_bit {
                metadata.is_executable()
            } else {
                existing == Some(Mode::FILE_EXECUTABLE)
            };
            let mode = if executable {
                Mode::FILE_EXECUTABLE
            } else {
                Mode::FILE
            };
            (data, mode)
        };
        let id = repo.write_blob(&data).map_err(err)?.detach();
        let stat = Stat::from_fs(&metadata).unwrap_or_default();
        let index = &mut locked.index;
        // Staging a conflicted path resolves it.
        index.remove_entries(|_, entry_path, entry| {
            entry_path == name && entry.stage() != Stage::Unconflicted
        });
        match index.entry_mut_by_path_and_stage(name.into(), Stage::Unconflicted) {
            Some(entry) => {
                entry.id = id;
                entry.mode = mode;
                entry.stat = stat;
                entry.flags.remove(Flags::INTENT_TO_ADD);
            }
            None => {
                index.dangerously_push_entry(stat, id, Flags::empty(), mode, name.into());
                index.sort_entries();
            }
        }
        staged += 1;
    }
    locked.write()?;
    Ok(match staged {
        1 => "Staged 1 file".into(),
        n => format!("Staged {n} files"),
    })
}

fn unstage(repo: &gix::Repository, paths: &[PathBuf]) -> Result<String, String> {
    let mut locked = lock_index(repo)?;
    let tree = repo.head_tree_id_or_empty().map_err(err)?;
    let tree = repo.find_tree(tree).map_err(err)?;
    let mut count = 0;
    for path in paths {
        let name = rela(path)?;
        let head = tree.lookup_entry_by_path(path).map_err(err)?;
        let index = &mut locked.index;
        let Some(head) = head else {
            // Added since `HEAD`: no longer staged, the file stays.
            index.remove_entries(|_, entry_path, entry| {
                entry_path == name && entry.stage() == Stage::Unconflicted
            });
            count += 1;
            continue;
        };
        use gix::objs::tree::EntryKind;
        let mode = match head.mode().kind() {
            EntryKind::Blob => Mode::FILE,
            EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
            EntryKind::Link => Mode::SYMLINK,
            EntryKind::Commit => Mode::COMMIT,
            EntryKind::Tree => continue,
        };
        let id = head.object_id();
        // No stat: the next status compares the content.
        match index.entry_mut_by_path_and_stage(name.into(), Stage::Unconflicted) {
            Some(entry) => {
                entry.id = id;
                entry.mode = mode;
                entry.stat = Stat::default();
                entry.flags.remove(Flags::INTENT_TO_ADD);
            }
            None => {
                index.dangerously_push_entry(
                    Stat::default(),
                    id,
                    Flags::empty(),
                    mode,
                    name.into(),
                );
                index.sort_entries();
            }
        }
        count += 1;
    }
    locked.write()?;
    Ok(match count {
        1 => "Unstaged 1 file".into(),
        n => format!("Unstaged {n} files"),
    })
}

/// Git's `whitespace` clean-up (what `git commit -m` does): trailing spaces,
/// leading and trailing blank lines and repeated blank lines go.
pub fn clean_message(message: &str) -> Option<String> {
    let mut out = String::new();
    let mut blank = false;
    for line in message.lines().map(str::trim_end) {
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    (!out.is_empty()).then_some(out)
}

fn commit(repo: &gix::Repository, message: &str) -> Result<String, String> {
    let message = clean_message(message).ok_or("Enter a commit message")?;
    if let Some(state) = repo.state() {
        return Err(format!(
            "Git is {}; finish it with git",
            imp::in_progress_label(&state)
        ));
    }
    if repo.config_snapshot().boolean("commit.gpgsign") == Some(true) {
        return Err(
            "commit.gpgsign is set and Kureha does not sign commits; commit with git".into(),
        );
    }
    let identity = "Set user.name and user.email in your Git configuration";
    repo.committer().ok_or(identity)?.map_err(err)?;
    repo.author().ok_or(identity)?.map_err(err)?;
    // Held until the commit is written, so the index cannot change meanwhile.
    let locked = lock_index(repo)?;
    let index = &locked.index;
    if index
        .entries()
        .iter()
        .any(|e| e.stage() != Stage::Unconflicted)
    {
        return Err("Resolve the conflicts and stage the files first".into());
    }
    let mut editor = repo
        .edit_tree(gix::ObjectId::empty_tree(repo.object_hash()))
        .map_err(err)?;
    for entry in index.entries() {
        if entry.flags.contains(Flags::INTENT_TO_ADD) {
            continue; // `git add -N`: not part of a commit yet.
        }
        use gix::objs::tree::EntryKind;
        let kind = match entry.mode {
            Mode::FILE => EntryKind::Blob,
            Mode::FILE_EXECUTABLE => EntryKind::BlobExecutable,
            Mode::SYMLINK => EntryKind::Link,
            Mode::COMMIT => EntryKind::Commit,
            _ => return Err("The index has entries Kureha cannot commit; use git".into()),
        };
        let path = entry.path(index).to_str().map_err(err)?;
        editor.upsert(path, kind, entry.id).map_err(err)?;
    }
    let tree = editor.write().map_err(err)?.detach();
    let head_tree = repo.head_tree_id_or_empty().map_err(err)?.detach();
    if tree == head_tree {
        return Err("Nothing is staged".into());
    }
    let parent = repo.head_id().ok().map(|id| id.detach());
    let id = repo
        .commit("HEAD", message.as_str(), tree, parent)
        .map_err(err)?;
    // Released without writing: the index already matches the commit.
    drop(locked);
    Ok(format!("Committed {}", id.to_hex_with_len(7)))
}

type Request = (u64, PathBuf, Operation);
type Answer = (u64, Operation, Result<String, String>);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Change, testing::Repo};

    fn stage_all(repo: &Repo) -> String {
        let snapshot = repo.read("");
        let paths = snapshot.unstaged.iter().map(|e| e.path.clone()).collect();
        apply(&repo.0, &Operation::Stage(paths)).unwrap()
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

        assert_eq!(stage_all(&repo), "Staged 8 files");
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
        .unwrap();
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
        apply(&repo.0, &Operation::Commit("first".into())).unwrap();
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]).trim(), "1");
        assert_eq!(repo.git(&["status", "--porcelain"]), "");

        let linked = repo.0.with_extension("linked");
        let _ = std::fs::remove_dir_all(&linked);
        repo.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().unwrap(),
        ]);
        std::fs::write(linked.join("a.txt"), "side\n").unwrap();
        apply(&linked, &Operation::Stage(vec!["a.txt".into()])).unwrap();
        apply(&linked, &Operation::Commit("on side".into())).unwrap();
        assert_eq!(repo.git_in(&linked, &["status", "--porcelain"]), "");
        assert_eq!(repo.git(&["log", "-1", "--format=%s", "side"]), "on side\n");
        assert_eq!(repo.git(&["log", "-1", "--format=%s", "main"]), "first\n");
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        let _ = std::fs::remove_dir_all(&linked);
    }

    #[test]
    fn refusals_leave_the_repository_alone() {
        let repo = Repo::new("refuse");
        repo.write("a.txt", "a\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        let index = repo.0.join(".git/index");
        let before = std::fs::read(&index).unwrap();
        let head = repo.git(&["rev-parse", "HEAD"]);

        assert_eq!(
            apply(&repo.0, &Operation::Commit("nothing".into())).unwrap_err(),
            "Nothing is staged"
        );
        assert_eq!(
            apply(&repo.0, &Operation::Commit(" \n\n".into())).unwrap_err(),
            "Enter a commit message"
        );
        repo.write("a.txt", "b\n");
        std::fs::write(repo.0.join(".git/index.lock"), "").unwrap();
        let locked = apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap_err();
        assert!(locked.starts_with("The index is locked"), "{locked}");
        std::fs::remove_file(repo.0.join(".git/index.lock")).unwrap();

        repo.git(&["config", "filter.mine.clean", "cat"]);
        let filters = apply(&repo.0, &Operation::Stage(vec!["a.txt".into()])).unwrap_err();
        assert!(filters.contains("defines Git filters"), "{filters}");
        repo.git(&["config", "--remove-section", "filter.mine"]);

        repo.git(&["add", "a.txt"]);
        let staged = std::fs::read(&index).unwrap();
        std::fs::write(repo.0.join(".git/MERGE_HEAD"), &head).unwrap();
        let merging = apply(&repo.0, &Operation::Commit("merge".into())).unwrap_err();
        assert_eq!(merging, "Git is merging; finish it with git");
        std::fs::remove_file(repo.0.join(".git/MERGE_HEAD")).unwrap();
        repo.git(&["config", "commit.gpgsign", "true"]);
        let signing = apply(&repo.0, &Operation::Commit("signed".into())).unwrap_err();
        assert!(signing.starts_with("commit.gpgsign"), "{signing}");

        assert_ne!(before, staged);
        assert_eq!(std::fs::read(&index).unwrap(), staged);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
        assert!(!repo.0.join(".git/index.lock").exists());
    }

    #[test]
    fn messages_are_cleaned_like_git_commit_m() {
        // Leading spaces stay, as in Git.
        assert_eq!(
            clean_message("\n  a  \n\n\n b\n\n").as_deref(),
            Some("  a\n\n b\n")
        );
        assert_eq!(clean_message(" \n \n"), None);
        assert_eq!(clean_message("#1 fix").as_deref(), Some("#1 fix\n"));
    }
}
