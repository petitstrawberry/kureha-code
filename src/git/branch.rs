//! Local branches, read and changed through the system Git command.
//!
//! Git owns checkout, locks, hooks and filters. Before switching, compare the
//! trees to protect open documents with unsaved edits and report the paths
//! the UI needs to reload.

use super::command;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// `path` with its folder resolved (`/tmp` and `/private/tmp` agree), so
/// paths of files that do not exist yet compare too.
pub fn resolved(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .unwrap_or_else(|_| resolved(parent))
            .join(name),
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

/// Local branches, sorted by name. NUL delimiters preserve spaces, newlines
/// and non-UTF-8 bytes in linked work tree paths.
pub(super) fn list(workdir: &Path) -> Result<Vec<Branch>, String> {
    let bytes = command::run(
        workdir,
        [
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:strip=2)%00%(HEAD)%00%(worktreepath)%00",
            "refs/heads/",
        ],
    )?;
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut branches = Vec::new();
    while let Some(name) = fields.next() {
        // for-each-ref appends a newline after each formatted record.
        let name = name.strip_prefix(b"\n").unwrap_or(name);
        if name.is_empty() {
            break;
        }
        let current = fields.next().ok_or("Missing Git branch HEAD field")? == b"*";
        let elsewhere = fields.next().ok_or("Missing Git branch worktree field")?;
        branches.push(Branch {
            name: String::from_utf8_lossy(name).into_owned(),
            current,
            elsewhere: if current || elsewhere.is_empty() {
                None
            } else {
                Some(command::path(elsewhere)?)
            },
        });
    }
    Ok(branches)
}

/// `refs/heads/<name>`, if `name` is a valid literal branch name.
///
/// This also runs while typing in the branch picker, so use Git's refname
/// rules without starting a child process for each keystroke. The eventual
/// `git branch` or `git switch` still validates the name itself.
pub fn full_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let invalid = name.is_empty()
        || name.starts_with('-')
        || name == "HEAD"
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || b"~^:?*[\\".contains(&byte))
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"));
    if invalid {
        Err(format!("`{name}` is not a valid branch name"))
    } else {
        Ok(format!("refs/heads/{name}"))
    }
}

/// Creates a local branch, optionally checking it out. Git handles unborn
/// branches too (`switch -c` can rename the branch before its first commit).
pub(super) fn create(workdir: &Path, name: &str, switch: bool) -> Result<Switched, String> {
    full_name(name)?;
    let name = name.trim();
    if switch {
        let before = worktree_files(workdir)?;
        command::run(workdir, ["switch", "-c", name])?;
        Ok(Switched {
            message: format!("Created and switched to {name}"),
            changed: changed_files(workdir, before),
        })
    } else {
        command::run(workdir, ["branch", "--", name])?;
        Ok(Switched {
            message: format!("Created {name}"),
            changed: Vec::new(),
        })
    }
}

/// Paths (relative to the work tree) that a switch changed, and what it did.
pub struct Switched {
    pub message: String,
    pub changed: Vec<PathBuf>,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
    link: Option<PathBuf>,
    #[cfg(unix)]
    changed: (i64, i64),
}

/// Track disk changes made by checkout hooks as well as by the tree switch.
/// Metadata avoids reading every blob or running clean filters to compare
/// content; include non-ignored untracked files that can also be open.
fn worktree_files(workdir: &Path) -> Result<BTreeMap<PathBuf, Option<FileStamp>>, String> {
    let bytes = command::run(
        workdir,
        [
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = command::path(path)?;
            let absolute = workdir.join(&path);
            let stamp = std::fs::symlink_metadata(&absolute)
                .ok()
                .map(|metadata| FileStamp {
                    modified: metadata.modified().ok(),
                    len: metadata.len(),
                    link: std::fs::read_link(&absolute).ok(),
                    #[cfg(unix)]
                    changed: {
                        use std::os::unix::fs::MetadataExt;
                        (metadata.ctime(), metadata.ctime_nsec())
                    },
                });
            Ok((path, stamp))
        })
        .collect()
}

fn changed_files(workdir: &Path, before: BTreeMap<PathBuf, Option<FileStamp>>) -> Vec<PathBuf> {
    let Ok(after) = worktree_files(workdir) else {
        // The checkout already succeeded. If inspection fails, reload all
        // known documents rather than reporting that the operation failed.
        return before.into_keys().collect();
    };
    let mut changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

fn changed_paths(workdir: &Path, target: &str) -> Result<Vec<PathBuf>, String> {
    // Resolve exact refs before comparing; tags named like branches and
    // revision-like branch names must not select a different tree.
    let target = command::run(
        workdir,
        ["rev-parse", "--verify", &format!("{target}^{{tree}}")],
    )?;
    let target = std::str::from_utf8(&target)
        .map_err(|e| e.to_string())?
        .trim();
    let head = command::output(workdir, ["rev-parse", "--verify", "--quiet", "HEAD^{tree}"])?;
    let bytes = if head.status.success() {
        let head = std::str::from_utf8(&head.stdout)
            .map_err(|e| e.to_string())?
            .trim();
        command::run(
            workdir,
            [
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                head,
                target,
                "--",
            ],
        )?
    } else if head.status.code() == Some(1) {
        // No HEAD tree on an unborn branch: all target entries are new.
        command::run(workdir, ["ls-tree", "-r", "--name-only", "-z", target])?
    } else {
        return command::checked(head).map(|_| Vec::new());
    };
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(command::path)
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Switches to a local branch. `keep` lists absolute paths of unsaved
/// documents; Git cannot see those edits, so protect them before checkout.
/// Other checkout decisions (including carrying local edits) belong to Git.
pub(super) fn switch(workdir: &Path, name: &str, keep: &[PathBuf]) -> Result<Switched, String> {
    let full = full_name(name)?;
    let name = name.trim();
    let mut changed = changed_paths(workdir, &full)?;
    let keep: Vec<PathBuf> = keep
        .iter()
        .flat_map(|path| [Some(resolved(path)), std::fs::canonicalize(path).ok()])
        .flatten()
        .collect();
    for path in &changed {
        let absolute = resolved(&workdir.join(path));
        // Include ancestor collisions: a file may replace a directory that
        // contains an unsaved document, or the reverse.
        if keep
            .iter()
            .any(|open| open.starts_with(&absolute) || absolute.starts_with(open))
        {
            return Err(format!(
                "Save or close {} first: the switch changes it",
                path.display()
            ));
        }
    }
    let before = worktree_files(workdir)?;
    command::run(workdir, ["switch", "--no-guess", "--", name])?;
    changed.extend(changed_files(workdir, before));
    changed.sort();
    changed.dedup();
    Ok(Switched {
        message: format!("Switched to {name}"),
        changed,
    })
}

#[cfg(all(test, unix, not(target_os = "scarlet")))]
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
        repo.write("unchanged.txt", "unchanged\n");
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
    fn git_errors_leave_the_tree_index_and_head_alone() {
        let repo = branches("refuse-switch");
        let index = repo.0.join(".git/index");
        let state = |repo: &Repo| {
            (
                snapshot(&repo.0),
                repo.git(&["ls-files", "--stage"]),
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
        refuse(&repo, Vec::new(), "would be overwritten");
        repo.git(&["add", "a.txt"]);
        refuse(&repo, Vec::new(), "would be overwritten");
        repo.git(&["reset", "-q", "--hard"]);
        // Untracked files the switch would overwrite.
        repo.write("new.txt", "mine\n");
        refuse(&repo, Vec::new(), "would be overwritten");
        std::fs::remove_file(repo.0.join("new.txt")).unwrap();
        // An open document with unsaved edits that the switch changes.
        refuse(&repo, vec![repo.0.join("a.txt")], "Save or close a.txt");
        // Checked out in another work tree.
        let linked = Repo(repo.0.with_extension("linked"));
        repo.git(&["worktree", "add", "-q", linked.0.to_str().unwrap(), "side"]);
        refuse(&repo, Vec::new(), "worktree");
        repo.git(&["worktree", "remove", "--force", linked.0.to_str().unwrap()]);
        // Git treats switching to the current branch as a successful no-op.
        assert!(switch(&repo, "main").unwrap().is_empty());
        let before = state(&repo);
        assert!(switch(&repo, "missing").is_err());
        assert_eq!(before, state(&repo));
        std::fs::write(index.with_extension("lock"), "owned by another Git process").unwrap();
        assert!(switch(&repo, "side").unwrap_err().contains("index.lock"));
        assert_eq!(before, state(&repo));
        std::fs::remove_file(index.with_extension("lock")).unwrap();
        // Git rejects switching during a merge itself.
        let head = repo.git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.0.join(".git/MERGE_HEAD"), &head).unwrap();
        refuse(&repo, Vec::new(), "merging");
        std::fs::remove_file(repo.0.join(".git/MERGE_HEAD")).unwrap();
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
        assert!(create("topic", false).is_err());
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
        let names: Vec<_> = list(&repo.0)
            .unwrap()
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

    #[test]
    fn unborn_and_linked_worktree_branches() {
        let repo = Repo::new("branch-unborn-linked");
        assert!(list(&repo.0).unwrap().is_empty());
        apply(
            &repo.0,
            &Operation::CreateBranch {
                name: "first".into(),
                switch: true,
            },
        )
        .unwrap();
        assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/first\n");
        repo.write("a.txt", "first\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "first"]);
        repo.git(&["switch", "--orphan", "empty"]);
        let error = apply(
            &repo.0,
            &Operation::Switch {
                name: "first".into(),
                keep: vec![repo.0.join("a.txt")],
            },
        )
        .unwrap_err();
        assert!(error.contains("Save or close a.txt"), "{error}");
        assert_eq!(switch(&repo, "first").unwrap(), [PathBuf::from("a.txt")]);

        let linked = Repo(repo.0.with_extension("linked\n\"quoted"));
        repo.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.0.to_str().unwrap(),
        ]);
        let other = list(&repo.0)
            .unwrap()
            .into_iter()
            .find(|b| b.name == "linked")
            .unwrap();
        assert!(!other.current);
        assert_eq!(
            other.elsewhere,
            Some(std::fs::canonicalize(&linked.0).unwrap())
        );
        let here = list(&linked.0)
            .unwrap()
            .into_iter()
            .find(|b| b.name == "linked")
            .unwrap();
        assert!(here.current);
        assert!(here.elsewhere.is_none());
        apply(
            &linked.0,
            &Operation::CreateBranch {
                name: "third".into(),
                switch: false,
            },
        )
        .unwrap();
        apply(
            &linked.0,
            &Operation::Switch {
                name: "third".into(),
                keep: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            repo.git_in(&linked.0, &["symbolic-ref", "HEAD"]),
            "refs/heads/third\n"
        );
        assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/first\n");
        repo.git(&["worktree", "remove", linked.0.to_str().unwrap()]);
    }

    #[test]
    fn unsaved_documents_and_symlink_aliases_are_protected() {
        let repo = branches("switch-unsaved");
        std::os::unix::fs::symlink("a.txt", repo.0.join("document-alias")).unwrap();
        std::os::unix::fs::symlink(&repo.0, repo.0.join("folder-alias")).unwrap();
        // Existing files, files that do not exist yet, a file-to-directory
        // change, and buffers opened through file or directory symlinks.
        for path in [
            "a.txt",
            "new.txt",
            "swap/inner.txt",
            "document-alias",
            "folder-alias/a.txt",
        ] {
            let before = snapshot(&repo.0);
            let error = apply(
                &repo.0,
                &Operation::Switch {
                    name: "side".into(),
                    keep: vec![repo.0.join(path)],
                },
            )
            .unwrap_err();
            assert!(error.contains("Save or close"), "{path}: {error}");
            assert_eq!(snapshot(&repo.0), before);
            assert_eq!(repo.git(&["symbolic-ref", "HEAD"]), "refs/heads/main\n");
        }
        // An unchanged file can retain both local changes and buffer edits.
        repo.write("unchanged.txt", "local edits\n");
        repo.git(&["add", "unchanged.txt"]);
        let done = apply(
            &repo.0,
            &Operation::Switch {
                name: "side".into(),
                keep: vec![repo.0.join("unchanged.txt")],
            },
        )
        .unwrap();
        assert!(!done.changed.contains(&PathBuf::from("unchanged.txt")));
        assert_eq!(
            std::fs::read_to_string(repo.0.join("unchanged.txt")).unwrap(),
            "local edits\n"
        );
        assert_eq!(
            repo.git(&["diff", "--cached", "--name-only"]),
            "unchanged.txt\n"
        );
    }

    #[test]
    fn ignored_files_follow_git_checkout_semantics() {
        let repo = branches("switch-ignored");
        repo.write(".git/info/exclude", "new.txt\n");
        repo.write("new.txt", "ignored file\n");
        switch(&repo, "side").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.0.join("new.txt")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn checkout_hooks_and_filters_run_and_hook_edits_are_reloaded() {
        use std::os::unix::fs::PermissionsExt;
        let repo = Repo::new("switch-hooks-filters");
        repo.git(&["config", "filter.case.clean", "tr a-z A-Z"]);
        repo.git(&["config", "filter.case.smudge", "tr A-Z a-z"]);
        repo.write(".gitattributes", "*.txt filter=case\n");
        repo.write("a.txt", "main\n");
        repo.write("hook.txt", "before\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "main"]);
        repo.git(&["switch", "-q", "-c", "side"]);
        repo.write("a.txt", "side\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "side"]);
        repo.git(&["switch", "-q", "main"]);
        repo.git(&["config", "core.hooksPath", ".git/custom-hooks"]);
        repo.write(".git/custom-hooks/post-checkout", "#!/bin/sh\nprintf 'hook %s\\n' \"$3\" >> hook.txt\nprintf 'created by hook\\n' > hook-created.txt\n");
        std::fs::set_permissions(
            repo.0.join(".git/custom-hooks/post-checkout"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let changed = switch(&repo, "side").unwrap();
        assert!(changed.contains(&PathBuf::from("a.txt")));
        assert!(changed.contains(&PathBuf::from("hook.txt")));
        assert!(changed.contains(&PathBuf::from("hook-created.txt")));
        assert_eq!(
            std::fs::read_to_string(repo.0.join("a.txt")).unwrap(),
            "side\n"
        );
        assert_eq!(repo.git(&["show", "HEAD:a.txt"]), "SIDE\n");
        let done = apply(
            &repo.0,
            &Operation::CreateBranch {
                name: "created".into(),
                switch: true,
            },
        )
        .unwrap();
        assert!(done.changed.contains(&PathBuf::from("hook.txt")));
        assert_eq!(
            std::fs::read_to_string(repo.0.join("hook.txt")).unwrap(),
            "before\nhook 1\nhook 1\n"
        );
    }

    #[test]
    fn changed_paths_preserve_literal_names() {
        #[cfg(not(target_os = "macos"))]
        use std::os::unix::ffi::OsStringExt;
        let repo = Repo::new("switch-literal-paths");
        let mut paths: Vec<PathBuf> = [
            "[glob].txt",
            ":(top)literal",
            "-option",
            "line\nbreak",
            "日本語.txt",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        // APFS rejects invalid UTF-8 names at creation time.
        #[cfg(not(target_os = "macos"))]
        paths.push(std::ffi::OsString::from_vec(b"non-utf8-\xff.txt".to_vec()).into());
        for path in &paths {
            std::fs::write(repo.0.join(path), "main\n").unwrap();
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "main"]);
        repo.git(&["switch", "-q", "-c", "side"]);
        for path in &paths {
            std::fs::write(repo.0.join(path), "side\n").unwrap();
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "side"]);
        repo.git(&["switch", "-q", "main"]);
        let error = apply(
            &repo.0,
            &Operation::Switch {
                name: "side".into(),
                keep: vec![repo.0.join(paths.last().unwrap())],
            },
        )
        .unwrap_err();
        assert!(error.contains("Save or close"), "{error}");
        paths.sort();
        assert_eq!(switch(&repo, "side").unwrap(), paths);
        for path in &paths {
            assert_eq!(std::fs::read(repo.0.join(path)).unwrap(), b"side\n");
        }
    }

    #[test]
    fn branch_name_validation_matches_git_without_spawning_in_the_picker() {
        let repo = Repo::new("branch-validation");
        for name in [
            "",
            "-x",
            "HEAD",
            "a..b",
            "a b",
            "a\tb",
            "a\u{7f}b",
            ".hidden",
            "a/.b",
            "a.lock",
            "a.lock/b",
            "a/",
            "/a",
            "a//b",
            "a.",
            "a./b",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            "@{1}",
            "@",
            "topic",
            "feature/x",
            "日本語",
            " padded ",
        ] {
            let git =
                command::output(&repo.0, ["check-ref-format", "--branch", name.trim()]).unwrap();
            assert_eq!(full_name(name).is_ok(), git.status.success(), "{name:?}");
        }
        assert!(full_name("nul\0byte").is_err());
    }
}
