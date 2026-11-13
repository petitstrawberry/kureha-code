//! Local branches: listing them, creating one, and switching the work tree
//! to one, as `git switch` does.
//!
//! A switch only runs on a clean work tree (nothing staged or changed in
//! tracked files), checked again under `index.lock`. Everything is validated
//! before the first write: the target's blobs, files the switch would put
//! where an untracked or ignored file is, open documents with unsaved edits
//! that the switch would change, and a branch checked out in another work
//! tree. Then files are removed, written through gix's checkout (which
//! refuses `.git` and `..` paths and writing through symbolic links), the
//! index is rebuilt from the target tree, and `HEAD` moves only if it still
//! points where it did.

use std::path::{Path, PathBuf};

/// `path` with its folder resolved (`/tmp` and `/private/tmp` agree), so
/// paths of files that do not exist yet compare too.
pub fn resolved(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// A local branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// `main`, `feature/x`.
    pub name: String,
    /// Checked out here.
    pub current: bool,
    /// Checked out in another work tree, which a switch must not take over.
    pub elsewhere: Option<PathBuf>,
}

/// Local branches, sorted by name.
pub(super) fn list(repo: &gix::Repository) -> Result<Vec<Branch>, String> {
    let err = |e: gix::Error| e.to_string();
    let current = repo.head_name().map_err(err)?;
    let mut elsewhere = std::collections::BTreeMap::new();
    let here = std::fs::canonicalize(repo.git_dir()).ok();
    if let Ok(worktrees) = repo.worktrees_including_main() {
        for worktree in worktrees.flatten() {
            if std::fs::canonicalize(worktree.git_dir()).ok() == here {
                continue;
            }
            if let (Ok(Some(name)), Some(dir)) = (worktree.head_name(), worktree.workdir()) {
                elsewhere.insert(name, dir.to_path_buf());
            }
        }
    }
    let references = repo.references().map_err(err)?;
    let mut branches = Vec::new();
    for reference in references.local_branches().map_err(err)? {
        let reference = reference.map_err(|e| e.to_string())?;
        let full = reference.name().to_owned();
        branches.push(Branch {
            name: full.shorten().to_string(),
            current: current.as_ref() == Some(&full),
            elsewhere: elsewhere.get(&full).cloned(),
        });
    }
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(branches)
}

/// `refs/heads/<name>`, if `name` is a valid branch name.
pub fn full_name(name: &str) -> Result<gix::refs::FullName, String> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') || name == "HEAD" {
        return Err(format!("`{name}` is not a valid branch name"));
    }
    gix::refs::FullName::try_from(format!("refs/heads/{name}"))
        .map_err(|_| format!("`{name}` is not a valid branch name"))
}

fn head_target(repo: &gix::Repository) -> Result<gix::refs::Target, String> {
    let head = repo.find_reference("HEAD").map_err(|e| e.to_string())?;
    Ok(head.inner.target)
}

/// Moves `HEAD` to `branch` if it still points at `expected`, and records
/// it in `HEAD`'s reflog as Git does (`git switch -` reads it back).
fn move_head(
    repo: &gix::Repository,
    expected: gix::refs::Target,
    branch: &gix::refs::FullName,
    from: &str,
) -> Result<(), String> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    let old = repo.head_id().map(|id| id.detach()).ok();
    let message = format!("checkout: moving from {from} to {}", branch.shorten());
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.clone().into(),
            },
            expected: PreviousValue::MustExistAndMatch(expected),
            new: gix::refs::Target::Symbolic(branch.clone()),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e: gix::validate::reference::name::Error| e.to_string())?,
        deref: false,
    })
    .map_err(|e| e.to_string())?;
    // gix writes no reflog entry when HEAD stays symbolic.
    let new = repo.head_id().map_err(|e| e.to_string())?.detach();
    append_head_reflog(repo, old, new, &message)
}

fn append_head_reflog(
    repo: &gix::Repository,
    old: Option<gix::ObjectId>,
    new: gix::ObjectId,
    message: &str,
) -> Result<(), String> {
    use std::io::Write;
    if repo.config_snapshot().boolean("core.logAllRefUpdates") == Some(false) {
        return Ok(());
    }
    let Some(Ok(committer)) = repo.committer() else {
        return Ok(());
    };
    let old = old.unwrap_or_else(|| gix::ObjectId::null(repo.object_hash()));
    let mut line = format!("{old} {new} ").into_bytes();
    committer.write_to(&mut line).map_err(|e| e.to_string())?;
    line.extend_from_slice(format!("\t{message}\n").as_bytes());
    let path = repo.git_dir().join("logs").join("HEAD");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(&line))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn head_label(repo: &gix::Repository) -> String {
    match repo.head_name() {
        Ok(Some(name)) => name.shorten().to_string(),
        _ => repo
            .head_id()
            .map(|id| id.to_hex_with_len(7).to_string())
            .unwrap_or_else(|_| "HEAD".into()),
    }
}

/// Creates `name` at `HEAD`'s commit and, if `switch`, checks it out: the
/// files are the same, so only `HEAD` moves.
pub(super) fn create(repo: &gix::Repository, name: &str, switch: bool) -> Result<String, String> {
    let full = full_name(name)?;
    if repo
        .try_find_reference(full.as_ref())
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("Branch {} already exists", full.shorten()));
    }
    let commit = repo
        .head_id()
        .map_err(|_| "Commit something first: the branch needs a commit to start from".to_string())?
        .detach();
    let from = head_label(repo);
    let expected = head_target(repo)?;
    repo.reference(
        full.as_ref(),
        commit,
        gix::refs::transaction::PreviousValue::MustNotExist,
        format!("branch: Created from {from}"),
    )
    .map_err(|e| e.to_string())?;
    if switch {
        move_head(repo, expected, &full, &from)?;
        Ok(format!("Created and switched to {}", full.shorten()))
    } else {
        Ok(format!("Created {}", full.shorten()))
    }
}

/// Paths (relative to the work tree) that a switch changed, and what it did.
pub struct Switched {
    pub message: String,
    pub changed: Vec<PathBuf>,
}

/// Switches the work tree to `name`. `keep` lists files (absolute) open
/// with unsaved edits; the switch refuses to change any of them.
#[cfg(not(target_os = "scarlet"))]
pub(super) fn switch(
    repo: &gix::Repository,
    name: &str,
    keep: &[PathBuf],
) -> Result<Switched, String> {
    use gix::index::entry::{Flags, Mode, Stage, Stat};
    let err = |e: gix::Error| e.to_string();
    let full = full_name(name)?;
    let short = full.shorten().to_string();
    if repo.head_name().map_err(err)?.as_ref() == Some(&full) {
        return Err(format!("Already on {short}"));
    }
    if let Some(state) = repo.state() {
        return Err(format!(
            "Git is {}; finish it with git",
            super::imp::in_progress_label(&state)
        ));
    }
    let workdir = repo
        .workdir()
        .ok_or("The repository has no work tree")?
        .to_path_buf();
    if let Some(other) = list(repo)?
        .into_iter()
        .find(|branch| branch.name == short)
        .and_then(|branch| branch.elsewhere)
    {
        return Err(format!("{short} is checked out in {}", other.display()));
    }
    // Filters defined only in the repository's config would not run here, so
    // the written files would differ from what `git switch` writes.
    super::write::refuse_repository_filters(repo)?;
    let target = repo
        .find_reference(full.as_ref())
        .map_err(|e| e.to_string())?
        .peel_to_commit()
        .map_err(|e| e.to_string())?;
    let new_tree = target.tree().map_err(err)?;
    let old_tree = repo
        .find_tree(repo.head_tree_id_or_empty().map_err(err)?)
        .map_err(err)?;

    let mut locked = super::write::lock_index(repo)?;
    if dirty(repo)? {
        return Err(
            "Commit or unstage the changes first: switching needs a clean work tree".into(),
        );
    }
    let changes = repo
        .diff_tree_to_tree(
            &old_tree,
            &new_tree,
            gix::diff::Options::default().with_rewrites(None),
        )
        .map_err(err)?;
    use gix::object::tree::diff::ChangeDetached as C;
    let mut removed = Vec::new();
    let mut written = Vec::new();
    for change in &changes {
        let (location, old, new) = match change {
            C::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (location, None, Some((*entry_mode, *id))),
            C::Deletion {
                location,
                entry_mode,
                ..
            } => (location, Some(*entry_mode), None),
            C::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                id,
                ..
            } => (
                location,
                Some(*previous_entry_mode),
                Some((*entry_mode, *id)),
            ),
            C::Rewrite { .. } => return Err("Unexpected rename while switching".into()),
        };
        let path = gix::path::from_bstr(gix::bstr::ByteSlice::as_bstr(location.as_slice()))
            .map_err(|e| e.to_string())?
            .into_owned();
        if old.is_some_and(|mode| !mode.is_tree()) && new.is_none_or(|(mode, _)| mode.is_tree()) {
            removed.push((path.clone(), old.unwrap()));
        }
        if let Some((mode, id)) = new.filter(|(mode, _)| !mode.is_tree()) {
            if old.is_some_and(|old| !old.is_tree())
                && old != Some(mode)
                && (old.unwrap().is_commit() || mode.is_commit())
            {
                return Err(format!(
                    "{} changes between a submodule and a file; switch with git",
                    path.display()
                ));
            }
            written.push((path, mode, id));
        }
    }
    // Validate before the first write.
    let touched: Vec<&PathBuf> = removed
        .iter()
        .map(|(path, _)| path)
        .chain(written.iter().map(|(path, ..)| path))
        .collect();
    let keep: Vec<PathBuf> = keep.iter().map(|path| resolved(path)).collect();
    if let Some(open) = touched
        .iter()
        .find(|path| keep.contains(&resolved(&workdir.join(path))))
    {
        return Err(format!(
            "Save or close {} first: the switch changes it",
            open.display()
        ));
    }
    // On a case-insensitive file system, `Case.txt` on disk answers for
    // `case.txt`; a path the switch removes is no collision.
    let ignore_case = repo
        .filesystem_options()
        .map(|fs| fs.ignore_case)
        .unwrap_or(false);
    let fold = |path: &Path| path.to_string_lossy().to_lowercase();
    let removed_here = |path: &Path| {
        removed
            .iter()
            .any(|(gone, _)| gone == path || (ignore_case && fold(gone) == fold(path)))
    };
    let tracked = |path: &Path| -> Result<bool, String> {
        Ok(old_tree
            .lookup_entry_by_path(path)
            .map_err(err)?
            .is_some_and(|entry| !entry.mode().is_tree()))
    };
    for (path, mode, id) in &written {
        if !mode.is_commit() {
            repo.find_object(*id).map_err(err)?;
        }
        // Something on disk that the old tree does not track would be lost.
        let mut probe = PathBuf::new();
        for component in path.components() {
            probe.push(component);
            let on_disk = std::fs::symlink_metadata(workdir.join(&probe));
            let Ok(metadata) = on_disk else { break };
            let last = probe == *path;
            // A folder where a file goes is fine if the switch empties it.
            let emptied =
                last && metadata.is_dir() && only_removed(&workdir, &probe, &removed_here);
            if (last || !metadata.is_dir())
                && !emptied
                && !tracked(&probe)?
                && !removed_here(&probe)
            {
                return Err(format!(
                    "{} would be overwritten by the switch; move it away first",
                    probe.display()
                ));
            }
            if !metadata.is_dir() {
                break;
            }
        }
    }

    // Remove first, so a case-only rename or a file turning into a folder
    // never deletes what was just written.
    for (path, mode) in &removed {
        if mode.is_commit() {
            continue; // A submodule's folder stays, as Git leaves it.
        }
        let full_path = workdir.join(path);
        match std::fs::remove_file(&full_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        let mut parent = full_path.parent();
        while let Some(dir) = parent.filter(|dir| *dir != workdir) {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
            parent = dir.parent();
        }
    }
    // Write the target's files through gix's checkout.
    let mut index = gix::index::State::new(repo.object_hash());
    for (path, mode, id) in &written {
        use gix::objs::tree::EntryKind;
        let mode = match mode.kind() {
            EntryKind::Blob => Mode::FILE,
            EntryKind::BlobExecutable => Mode::FILE_EXECUTABLE,
            EntryKind::Link => Mode::SYMLINK,
            EntryKind::Commit => Mode::COMMIT,
            EntryKind::Tree => continue,
        };
        let name = path.to_str().ok_or("Only UTF-8 paths are supported")?;
        index.dangerously_push_entry(Stat::default(), *id, Flags::empty(), mode, name.into());
    }
    index.sort_entries();
    // `.gitattributes` from the files being written (the target's), else from
    // the work tree (unchanged between the branches).
    let mut options = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMappingThenWorktree)
        .map_err(err)?;
    options.overwrite_existing = true;
    options.destination_is_initially_empty = false;
    let outcome = gix::worktree::state::checkout(
        &mut index,
        workdir.clone(),
        repo.objects.clone().into_arc().map_err(|e| e.to_string())?,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &std::sync::atomic::AtomicBool::new(false),
        options,
    )
    .map_err(err)?;
    if let Some(error) = outcome.errors.first() {
        return Err(format!("{}: {}", error.path, error.error));
    }
    if let Some(collision) = outcome.collisions.first() {
        return Err(format!("{} collides with another file", collision.path));
    }
    // The index is the target tree, with stat data where it is known.
    let mut new_index = repo.index_from_tree(&new_tree.id).map_err(err)?;
    let old = &locked.index;
    let written_stats: std::collections::HashMap<&gix::bstr::BStr, Stat> = index
        .entries()
        .iter()
        .map(|entry| (entry.path(&index), entry.stat))
        .collect();
    let paths = new_index.path_backing().to_owned();
    for entry in new_index.entries_mut() {
        let path = entry.path_in(&paths);
        if let Some(stat) = written_stats.get(path) {
            entry.stat = *stat;
        } else if let Some(previous) = old.entry_by_path_and_stage(path, Stage::Unconflicted)
            && previous.id == entry.id
            && previous.mode == entry.mode
        {
            entry.stat = previous.stat;
        }
    }
    locked.index =
        gix::index::File::from_state(new_index.into(), locked.index.path().to_path_buf());
    let from = head_label(repo);
    let expected = head_target(repo)?;
    locked.write()?;
    move_head(repo, expected, &full, &from)?;
    let mut changed: Vec<PathBuf> = touched.into_iter().cloned().collect();
    changed.sort();
    changed.dedup();
    Ok(Switched {
        message: format!("Switched to {short}"),
        changed,
    })
}

/// Whether everything below `folder` (relative to `workdir`) is a file the
/// switch removes, so removing them leaves nothing behind.
#[cfg(not(target_os = "scarlet"))]
fn only_removed(workdir: &Path, folder: &Path, removed: &dyn Fn(&Path) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(workdir.join(folder)) else {
        return false;
    };
    entries.flatten().all(|entry| {
        let path = folder.join(entry.file_name());
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => only_removed(workdir, &path, removed),
            Ok(_) => removed(&path),
            Err(_) => false,
        }
    })
}

/// Anything staged or changed in tracked files, conflicts and `git add -N`
/// included. Untracked files do not count; the switch checks those it would
/// overwrite.
#[cfg(not(target_os = "scarlet"))]
fn dirty(repo: &gix::Repository) -> Result<bool, String> {
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(|e| e.to_string())?
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_submodules(None)
        .index_worktree_rewrites(None)
        .into_iter(None)
        .map_err(|e| e.to_string())?;
    for item in iter {
        let item = item.map_err(|e| e.to_string())?;
        if let gix::status::Item::IndexWorktree(gix::status::index_worktree::Item::Modification {
            status,
            ..
        }) = &item
            && matches!(
                status,
                gix::status::plumbing::index_as_worktree::EntryStatus::NeedsUpdate(_)
            )
        {
            continue;
        }
        if matches!(
            item,
            gix::status::Item::IndexWorktree(
                gix::status::index_worktree::Item::DirectoryContents { .. }
            )
        ) {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Switching rewrites files through gix's checkout, which needs a backend
/// Scarlet does not have yet.
#[cfg(target_os = "scarlet")]
pub(super) fn switch(
    _repo: &gix::Repository,
    _name: &str,
    _keep: &[PathBuf],
) -> Result<Switched, String> {
    Err("Switching branches is not available on Scarlet yet".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::git::{
        testing::Repo,
        write::{Operation, apply},
    };
    use std::collections::BTreeMap;

    /// Files and links below `root` (outside `.git`): content or link target,
    /// and whether the file is executable.
    fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, bool)> {
        use std::os::unix::fs::PermissionsExt;
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, bool)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if name == ".git" {
                    continue;
                }
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                if metadata.is_symlink() {
                    let target = std::fs::read_link(&path).unwrap();
                    out.insert(name, (target.into_os_string().into_encoded_bytes(), false));
                } else if metadata.is_dir() {
                    walk(root, &path, out);
                } else {
                    let exec = metadata.permissions().mode() & 0o111 != 0;
                    out.insert(name, (std::fs::read(&path).unwrap(), exec));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn switch(repo: &Repo, name: &str) -> Result<Vec<PathBuf>, String> {
        apply(
            &repo.0,
            &Operation::Switch {
                name: name.into(),
                keep: Vec::new(),
            },
        )
        .map(|done| done.changed)
    }

    /// `main` and `side`, differing in every way a switch has to handle.
    fn branches(name: &str) -> Repo {
        use std::os::unix::fs::PermissionsExt;
        let repo = Repo::new(name);
        repo.write(".gitattributes", "crlf.txt text eol=crlf\n");
        repo.write("a.txt", "a\n");
        repo.write("keep.txt", "keep\n");
        repo.write("crlf.txt", "one\ntwo\n");
        repo.write("tool.sh", "echo hi\n");
        std::fs::set_permissions(
            repo.0.join("tool.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        repo.write("swap", "a file on main\n");
        repo.write("Case.txt", "case\n");
        std::os::unix::fs::symlink("a.txt", repo.0.join("link")).unwrap();
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "main"]);
        repo.git(&["switch", "-q", "-c", "side"]);
        repo.write("a.txt", "a on side\n");
        repo.git(&["rm", "-q", "keep.txt", "swap"]);
        repo.write("swap/inner.txt", "a folder on side\n");
        repo.write("new.txt", "new\n");
        repo.write("crlf.txt", "one\ntwo\nthree\n");
        std::fs::set_permissions(
            repo.0.join("tool.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::remove_file(repo.0.join("link")).unwrap();
        std::os::unix::fs::symlink("new.txt", repo.0.join("link")).unwrap();
        repo.git(&["mv", "Case.txt", "case-tmp"]);
        repo.git(&["mv", "case-tmp", "case.txt"]);
        repo.git(&["add", "-A"]);
        let head = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},lib", head.trim()),
        ]);
        repo.git(&["commit", "-q", "-m", "side"]);
        repo.git(&["switch", "-q", "main"]);
        repo
    }

    #[test]
    fn switching_writes_what_git_checkout_writes() {
        let repo = branches("switch");
        let clone = repo.0.with_extension("clone");
        let _ = std::fs::remove_dir_all(&clone);
        repo.git(&[
            "clone",
            "-q",
            repo.0.to_str().unwrap(),
            clone.to_str().unwrap(),
        ]);
        let reference = |branch: &str| {
            repo.git_in(&clone, &["checkout", "-q", "-f", branch]);
            snapshot(&clone)
        };
        let side = reference("origin/side");
        let main = reference("main");

        let changed = switch(&repo, "side").unwrap();
        assert!(
            changed.contains(&PathBuf::from("a.txt"))
                && changed.contains(&PathBuf::from("swap/inner.txt"))
        );
        assert_eq!(snapshot(&repo.0), side);
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/side\n");
        let reflog = repo.git(&["reflog", "-2", "--format=%gs", "HEAD"]);
        assert_eq!(
            reflog,
            "checkout: moving from main to side\ncheckout: moving from side to main\n"
        );
        // `git switch -` finds the previous branch from it.
        assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "@{-1}"]), "main\n");
        let crlf = std::fs::read(repo.0.join("crlf.txt")).unwrap();
        assert_eq!(crlf, b"one\r\ntwo\r\nthree\r\n");

        switch(&repo, "main").unwrap();
        assert_eq!(snapshot(&repo.0), main);
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/main\n");
        repo.git(&["fsck", "--no-dangling"]);
        let _ = std::fs::remove_dir_all(&clone);
    }

    #[test]
    fn refusals_leave_the_tree_index_and_head_alone() {
        let repo = branches("refuse-switch");
        let index = repo.0.join(".git/index");
        let state = |repo: &Repo| {
            (
                snapshot(&repo.0),
                std::fs::read(&index).unwrap(),
                repo.git(&["symbolic-ref", "HEAD"]),
            )
        };
        let refuse = |repo: &Repo, keep: Vec<PathBuf>, expect: &str| {
            let before = state(repo);
            let error = apply(
                &repo.0,
                &Operation::Switch {
                    name: "side".into(),
                    keep,
                },
            )
            .unwrap_err();
            assert!(error.contains(expect), "{error}");
            assert!(
                before == state(repo),
                "a refused switch changed something: {error}"
            );
            assert!(!repo.0.join(".git/index.lock").exists());
        };
        // A tracked change, staged or not.
        repo.write("a.txt", "edited\n");
        refuse(&repo, Vec::new(), "clean work tree");
        repo.git(&["add", "a.txt"]);
        refuse(&repo, Vec::new(), "clean work tree");
        repo.git(&["reset", "-q", "--hard"]);
        // Untracked and ignored files the switch would overwrite.
        repo.write("new.txt", "mine\n");
        refuse(&repo, Vec::new(), "new.txt would be overwritten");
        std::fs::remove_file(repo.0.join("new.txt")).unwrap();
        repo.write(".git/info/exclude", "new.txt\n");
        repo.write("new.txt", "ignored but mine\n");
        refuse(&repo, Vec::new(), "new.txt would be overwritten");
        std::fs::remove_file(repo.0.join("new.txt")).unwrap();
        // An open document with unsaved edits that the switch changes.
        refuse(&repo, vec![repo.0.join("a.txt")], "Save or close a.txt");
        // Checked out in another work tree.
        let linked = repo.0.with_extension("linked");
        let _ = std::fs::remove_dir_all(&linked);
        repo.git(&["worktree", "add", "-q", linked.to_str().unwrap(), "side"]);
        refuse(&repo, Vec::new(), "is checked out in");
        repo.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
        // Already there; a merge in progress.
        let error = switch(&repo, "main").unwrap_err();
        assert_eq!(error, "Already on main");
        let head = repo.git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.0.join(".git/MERGE_HEAD"), &head).unwrap();
        refuse(&repo, Vec::new(), "merging");
        std::fs::remove_file(repo.0.join(".git/MERGE_HEAD")).unwrap();
        // A file the switch does not touch may have unsaved edits.
        repo.write("untouched.txt", "u\n");
        repo.git(&["add", "untouched.txt"]);
        repo.git(&["commit", "-q", "-m", "untouched"]);
        switch(&repo, "side")
            .map(|_| ())
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = std::fs::remove_dir_all(&linked);
    }

    #[test]
    fn creating_branches() {
        let repo = Repo::new("create");
        let create = |name: &str, switch: bool| {
            apply(
                &repo.0,
                &Operation::CreateBranch {
                    name: name.into(),
                    switch,
                },
            )
            .map(|done| done.message)
        };
        assert!(
            create("topic", true)
                .unwrap_err()
                .starts_with("Commit something first")
        );
        repo.write("a.txt", "a\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        assert_eq!(create("topic", false).unwrap(), "Created topic");
        assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/main\n");
        assert_eq!(
            create("feature/x", true).unwrap(),
            "Created and switched to feature/x"
        );
        assert_eq!(
            repo.git(&["symbolic-ref", "HEAD"]),
            "refs/heads/feature/x\n"
        );
        assert_eq!(
            repo.git(&["rev-parse", "feature/x"]),
            repo.git(&["rev-parse", "main"])
        );
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
        for bad in ["", "-x", "a..b", "a b", "HEAD", "x.lock", "topic"] {
            assert!(create(bad, false).is_err(), "{bad:?} was accepted");
        }
        let names: Vec<_> = repo
            .read("")
            .branches
            .into_iter()
            .map(|b| (b.name, b.current))
            .collect();
        assert_eq!(
            names,
            [
                ("feature/x".into(), true),
                ("main".into(), false),
                ("topic".into(), false)
            ]
        );
    }
}
