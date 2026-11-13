//! One file's changes, using Git's unified patch output with three lines of
//! context. Git handles attributes, clean filters, and work tree line endings.

use super::command;
use std::{
    ffi::OsString,
    path::{Component, Path},
};

/// Bytes on either side; larger files are not compared.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Lines kept for one change; counts include omitted lines.
pub const MAX_LINES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// `HEAD` to the index: what a commit would record.
    Staged,
    /// The index to the work tree: what staging would add.
    Unstaged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `@@ -1,3 +1,4 @@`
    Hunk,
    Context,
    Removed,
    Added,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// 1-based line numbers on each side, where the line exists.
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub added: usize,
    pub removed: usize,
    /// Why some or all lines are not shown (binary, too large).
    pub note: Option<String>,
}

/// Reads the change of `path` (relative to the work tree) in the repository
/// containing `folder`.
pub fn read(folder: &Path, path: &Path, side: Side) -> Result<FileDiff, String> {
    // Check Git availability before filesystem operations, including on Scarlet.
    let workdir = command::workdir(folder)?;
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("The diff path must be relative to the work tree".into());
    }
    let path: std::path::PathBuf = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if path.as_os_str().is_empty() {
        return Err("Select a file to compare".into());
    }
    match side {
        Side::Staged => staged(&workdir, &path),
        Side::Unstaged => unstaged(&workdir, &path),
    }
}

fn diff_args() -> Vec<OsString> {
    [
        "-c",
        "diff.suppressBlankEmpty=false",
        "diff",
        "--no-color",
        // An external diff driver can replace the patch with arbitrary output.
        // Textconv also changes the source line numbers and binary semantics.
        // Keep both disabled for this parser; Git's normal clean conversion and
        // attributes (including text/binary selection) still apply.
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--ignore-submodules=none",
        "--submodule=short",
        "--unified=3",
        "--inter-hunk-context=0",
        "--output-indicator-new=+",
        "--output-indicator-old=-",
        "--output-indicator-context= ",
        "--line-prefix=",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn note(text: impl Into<String>) -> FileDiff {
    FileDiff {
        note: Some(text.into()),
        ..FileDiff::default()
    }
}

fn too_large() -> FileDiff {
    note(format!(
        "Larger than {} MiB; not compared",
        MAX_BYTES / (1024 * 1024)
    ))
}

/// Check object sizes without loading their contents. Gitlinks name commits,
/// possibly absent from this repository; their patches contain just an ID.
fn large_blob(folder: &Path, mode: &[u8], id: &[u8]) -> Result<bool, String> {
    if mode == b"160000" || id.iter().all(|b| *b == b'0') {
        return Ok(false);
    }
    let id = std::str::from_utf8(id).map_err(|e| e.to_string())?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid Git object ID".into());
    }
    let size = command::run(folder, ["cat-file", "-s", id])?;
    let size = std::str::from_utf8(&size)
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    Ok(size > MAX_BYTES as u64)
}

fn staged(folder: &Path, path: &Path) -> Result<FileDiff, String> {
    let mut args = diff_args();
    args.extend(
        [
            "--cached",
            "--no-patch",
            "--raw",
            "--no-abbrev",
            "-z",
            "--find-renames",
        ]
        .map(OsString::from),
    );
    // Restricting this first command to the destination would hide its rename
    // source, turning a rename into a whole-file addition. Raw -z records keep
    // both paths losslessly, including tabs, newlines and non-UTF-8 bytes.
    let raw = command::run(folder, args)?;
    let mut records = raw.split(|b| *b == 0);
    while let Some(header) = records.next().filter(|h| !h.is_empty()) {
        let fields: Vec<_> = header
            .strip_prefix(b":")
            .ok_or("Invalid Git raw diff header")?
            .split(|b| *b == b' ')
            .collect();
        let [old_mode, new_mode, old_id, new_id, status] = fields.as_slice() else {
            return Err("Invalid Git raw diff header".into());
        };
        let first = command::path(records.next().ok_or("Missing Git diff path")?)?;
        let renamed = status.starts_with(b"R") || status.starts_with(b"C");
        let target = if renamed {
            command::path(records.next().ok_or("Missing Git rename destination")?)?
        } else {
            first.clone()
        };
        if target != path {
            continue;
        }
        if *status == b"U" {
            return Ok(note("Unmerged file"));
        }
        if large_blob(folder, old_mode, old_id)? || large_blob(folder, new_mode, new_id)? {
            return Ok(too_large());
        }
        let mut args = diff_args();
        args.extend(["--cached", "--find-renames", "--"].map(OsString::from));
        if renamed {
            args.push(first.into_os_string());
        }
        args.push(path.as_os_str().to_owned());
        return parse_patch(&command::run(folder, args)?);
    }
    Ok(FileDiff::default())
}

fn unstaged(folder: &Path, path: &Path) -> Result<FileDiff, String> {
    let entries = command::run(
        folder,
        [
            "ls-files".as_ref(),
            "--stage".as_ref(),
            "--full-name".as_ref(),
            "-z".as_ref(),
            "--".as_ref(),
            path.as_os_str(),
        ],
    )?;
    let mut indexed = false;
    for entry in entries.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or("Invalid Git index entry")?;
        if command::path(&entry[tab + 1..])? != path {
            continue;
        }
        let fields: Vec<_> = entry[..tab].split(|b| *b == b' ').collect();
        let [mode, id, stage] = fields.as_slice() else {
            return Err("Invalid Git index entry".into());
        };
        // A conflicted file compares against our side. If ours is absent,
        // compare the remaining work tree file against an empty file instead.
        if *stage == b"0" || *stage == b"2" {
            indexed = true;
            if large_blob(folder, mode, id)? {
                return Ok(too_large());
            }
        }
    }
    let metadata = match std::fs::symlink_metadata(folder.join(path)) {
        Ok(metadata) => Some(metadata),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if let Some(metadata) = &metadata {
        if metadata.is_file() && metadata.len() > MAX_BYTES as u64 {
            return Ok(too_large());
        }
        if !metadata.is_file() && !metadata.is_symlink() {
            // A tracked directory can be a submodule; an untracked directory
            // must not become an unbounded recursive --no-index comparison.
            if !metadata.is_dir() || !indexed {
                return Ok(note("Select a regular file to compare"));
            }
        }
    }
    let mut args = diff_args();
    args.push("--no-renames".into());
    if indexed {
        // This also gives ordinary tracked files a normal two-sided patch,
        // rather than the combined (@@@) format Git uses for conflicts.
        args.extend(["--ours", "--"].map(OsString::from));
        args.push(path.as_os_str().to_owned());
        parse_patch(&command::run(folder, args)?)
    } else if metadata.is_some() {
        // No temporary index, staging, or object writes just to view a new file.
        // Git recognizes /dev/null on Windows too and applies path attributes.
        args.extend(["--no-index", "--", "/dev/null"].map(OsString::from));
        args.push(path.as_os_str().to_owned());
        let output = command::output(folder, args)?;
        // --no-index reports a difference using exit status 1.
        if output.status.success() || output.status.code() == Some(1) {
            parse_patch(&output.stdout)
        } else {
            Err(format!(
                "Git diff failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    } else {
        Ok(FileDiff::default())
    }
}

#[derive(Clone, Copy)]
struct Range {
    next: u32,
    left: u32,
}

fn range(text: &str, sign: char) -> Option<Range> {
    let text = text.strip_prefix(sign)?;
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    let next: u32 = start.parse().ok()?;
    let left: u32 = count.parse().ok()?;
    next.checked_add(left)?;
    Some(Range { next, left })
}

fn keep(out: &mut FileDiff, kind: Kind, old: Option<u32>, new: Option<u32>, text: &[u8]) {
    if out.lines.len() < MAX_LINES {
        out.lines.push(Line {
            kind,
            old,
            new,
            text: String::from_utf8_lossy(text.strip_suffix(b"\r").unwrap_or(text)).into_owned(),
        });
    } else {
        out.note = Some(format!("Showing the first {MAX_LINES} lines"));
    }
}

/// Parse only hunk bodies; header paths are deliberately never unquoted or
/// split on whitespace. Hunk counts distinguish content such as `--- file`
/// from metadata and let us count changes even after reaching the line cap.
fn parse_patch(patch: &[u8]) -> Result<FileDiff, String> {
    let mut out = FileDiff::default();
    let mut hunk: Option<(Range, Range)> = None;
    let (mut old_bytes, mut new_bytes) = (0, 0);
    let mut lines = patch.split(|b| *b == b'\n').peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(b"\\") {
            // "No newline at end of file" is metadata, with no line number.
            continue;
        }
        if let Some((old, new)) = &mut hunk {
            let (kind, old_line, new_line) = match line.first() {
                Some(b' ') if old.left > 0 && new.left > 0 => {
                    (Kind::Context, Some(old.next), Some(new.next))
                }
                Some(b'-') if old.left > 0 => (Kind::Removed, Some(old.next), None),
                Some(b'+') if new.left > 0 => (Kind::Added, None, Some(new.next)),
                _ => return Err("Incomplete Git diff hunk".into()),
            };
            let bytes =
                line.len() - usize::from(lines.peek().is_some_and(|next| next.starts_with(b"\\")));
            if old_line.is_some() {
                old.next += 1;
                old.left -= 1;
                old_bytes += bytes;
            }
            if new_line.is_some() {
                new.next += 1;
                new.left -= 1;
                new_bytes += bytes;
            }
            // Clean filters can expand a small work tree file. Bound displayed
            // content too, even when the on-disk size passed the initial check.
            if old_bytes > MAX_BYTES || new_bytes > MAX_BYTES {
                return Ok(too_large());
            }
            out.added += usize::from(kind == Kind::Added);
            out.removed += usize::from(kind == Kind::Removed);
            keep(&mut out, kind, old_line, new_line, &line[1..]);
            if old.left == 0 && new.left == 0 {
                hunk = None;
            }
        } else if let Some(header) = line.strip_prefix(b"@@ ") {
            // Function names after the ranges may contain non-UTF-8 bytes.
            let mut fields = header.splitn(4, |b| *b == b' ');
            let old = fields
                .next()
                .and_then(|s| range(std::str::from_utf8(s).ok()?, '-'));
            let new = fields
                .next()
                .and_then(|s| range(std::str::from_utf8(s).ok()?, '+'));
            let (Some(old), Some(new), Some(b"@@")) = (old, new, fields.next()) else {
                return Err("Invalid Git diff hunk header".into());
            };
            let text = format!(
                "@@ -{},{} +{},{} @@",
                old.next, old.left, new.next, new.left
            );
            keep(&mut out, Kind::Hunk, None, None, text.as_bytes());
            if old.left != 0 || new.left != 0 {
                hunk = Some((old, new));
            }
        } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            out.note = Some("Binary file".into());
        } else if line.starts_with(b"@@@") {
            return Err("Unexpected combined Git diff".into());
        }
    }
    if hunk.is_some() {
        return Err("Incomplete Git diff hunk".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::Repo;

    fn render(diff: &FileDiff) -> String {
        diff.lines
            .iter()
            .map(|line| {
                let mark = match line.kind {
                    Kind::Hunk => "",
                    Kind::Context => " ",
                    Kind::Removed => "-",
                    Kind::Added => "+",
                };
                format!("{mark}{}\n", line.text)
            })
            .collect()
    }

    #[test]
    fn hunks_carry_three_lines_of_context_and_merge_when_close() {
        let repo = Repo::new("diff-context");
        let before: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        repo.write("a", &before);
        repo.git(&["add", "."]);
        let after = before
            .replace("line 2\n", "line two\n")
            .replace("line 8\n", "line eight\n")
            .replace("line 18\n", "")
            + "line 21\n";
        repo.write("a", &after);
        let diff = read(&repo.0, Path::new("a"), Side::Unstaged).unwrap();
        assert_eq!(
            render(&diff),
            "@@ -1,11 +1,11 @@\n line 1\n-line 2\n+line two\n line 3\n line 4\n line 5\n line 6\n line 7\n-line 8\n+line eight\n line 9\n line 10\n line 11\n\
             @@ -15,6 +15,6 @@\n line 15\n line 16\n line 17\n-line 18\n line 19\n line 20\n+line 21\n"
        );
        assert_eq!((diff.added, diff.removed), (3, 3));
        let added = diff.lines.iter().find(|l| l.kind == Kind::Added).unwrap();
        assert_eq!((added.old, added.new), (None, Some(2)));
        let removed = diff.lines.iter().find(|l| l.text == "line 18").unwrap();
        assert_eq!((removed.old, removed.new), (Some(18), None));
    }

    #[test]
    fn reads_both_sides_in_git_form() {
        let repo = Repo::new("diff-git-form");
        repo.write(".gitattributes", "*.txt text eol=lf\n");
        repo.write("a.txt", "one\ntwo\nthree\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        let a = Path::new("a.txt");
        repo.write("a.txt", "one\r\ntwo\r\nthree\r\n");
        assert_eq!(
            read(&repo.0, a, Side::Unstaged).unwrap(),
            FileDiff::default()
        );
        repo.write("a.txt", "one\r\n2\r\nthree\r\n");
        let expected = "@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n";
        assert_eq!(render(&read(&repo.0, a, Side::Unstaged).unwrap()), expected);
        assert_eq!(read(&repo.0, a, Side::Staged).unwrap(), FileDiff::default());
        repo.git(&["add", "a.txt"]);
        assert_eq!(render(&read(&repo.0, a, Side::Staged).unwrap()), expected);
        assert_eq!(
            read(&repo.0, a, Side::Unstaged).unwrap(),
            FileDiff::default()
        );
        repo.write("new.txt", "n\r\n");
        assert_eq!(
            render(&read(&repo.0, Path::new("new.txt"), Side::Unstaged).unwrap()),
            "@@ -0,0 +1,1 @@\n+n\n"
        );
        std::fs::remove_file(repo.0.join("a.txt")).unwrap();
        assert_eq!(
            render(&read(&repo.0, a, Side::Unstaged).unwrap()),
            "@@ -1,3 +0,0 @@\n-one\n-2\n-three\n"
        );
        repo.git(&["add", "a.txt"]);
        assert_eq!(read(&repo.0, a, Side::Staged).unwrap().removed, 3);
    }

    #[test]
    fn parses_headers_only_outside_hunks_and_preserves_line_numbers() {
        let patch = b"diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1,3 +1,3 @@ function \xff\n same\r\n--- file\n+++ file\n \xff\n@@ -8 +8 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let diff = parse_patch(patch).unwrap();
        assert_eq!((diff.added, diff.removed), (2, 2));
        assert_eq!(diff.lines[1].text, "same");
        assert_eq!(diff.lines[2].text, "-- file");
        assert_eq!(diff.lines[3].text, "++ file");
        assert_eq!(diff.lines[4].text, "\u{fffd}");
        assert_eq!((diff.lines[4].old, diff.lines[4].new), (Some(3), Some(3)));
        assert_eq!((diff.lines[7].old, diff.lines[7].new), (None, Some(8)));
        assert!(parse_patch(b"@@ -1,2 +1 @@\n-a\n+b\n").is_err());
        assert!(parse_patch(b"@@ -4294967295 +1 @@\n-a\n+b\n").is_err());
    }

    #[test]
    fn limits_retained_lines_but_counts_the_whole_diff() {
        let count = MAX_LINES + 5;
        let patch = format!("@@ -0,0 +1,{count} @@\n{}", "+a\n".repeat(count));
        let diff = parse_patch(patch.as_bytes()).unwrap();
        assert_eq!(diff.lines.len(), MAX_LINES);
        assert_eq!((diff.added, diff.removed), (count, 0));
        assert_eq!(
            diff.note,
            Some(format!("Showing the first {MAX_LINES} lines"))
        );
        assert_eq!(diff.lines.last().unwrap().new, Some((MAX_LINES - 1) as u32));
    }

    #[test]
    fn untracked_empty_binary_and_attribute_selected_binary_files() {
        let repo = Repo::new("diff-binary");
        repo.write("empty", "");
        assert_eq!(
            read(&repo.0, Path::new("empty"), Side::Unstaged).unwrap(),
            FileDiff::default()
        );
        repo.write("nul", "a\0b");
        repo.write(".gitattributes", "*.bin -diff\n");
        repo.write("ascii.bin", "plain text\n");
        for path in ["nul", "ascii.bin"] {
            let diff = read(&repo.0, Path::new(path), Side::Unstaged).unwrap();
            assert_eq!(diff.note.as_deref(), Some("Binary file"));
            assert!(diff.lines.is_empty());
        }
        repo.git(&["add", "."]);
        for path in ["nul", "ascii.bin"] {
            assert_eq!(
                read(&repo.0, Path::new(path), Side::Staged)
                    .unwrap()
                    .note
                    .as_deref(),
                Some("Binary file")
            );
        }
    }

    #[test]
    fn checks_both_blob_and_worktree_sizes_before_generating_patches() {
        let repo = Repo::new("diff-size");
        let path = Path::new("large");
        let file = std::fs::File::create(repo.0.join(path)).unwrap();
        file.set_len((MAX_BYTES + 1) as u64).unwrap();
        assert_eq!(read(&repo.0, path, Side::Unstaged).unwrap(), too_large());
        repo.git(&["add", "large"]);
        assert_eq!(read(&repo.0, path, Side::Staged).unwrap(), too_large());
        repo.git(&["commit", "-qm", "large"]);
        repo.write("large", "small\n");
        assert_eq!(read(&repo.0, path, Side::Unstaged).unwrap(), too_large());
        repo.git(&["add", "large"]);
        assert_eq!(read(&repo.0, path, Side::Staged).unwrap(), too_large());
        let patch = format!("@@ -0,0 +1 @@\n+{}\n", "a".repeat(MAX_BYTES + 1));
        assert_eq!(parse_patch(patch.as_bytes()).unwrap(), too_large());
        let patch = format!(
            "@@ -0,0 +1 @@\n+{}\n\\ No newline at end of file\n",
            "a".repeat(MAX_BYTES)
        );
        assert!(parse_patch(patch.as_bytes()).unwrap().note.is_none());
    }

    #[test]
    fn paths_are_literal_and_relative_to_the_repository_even_from_a_subfolder() {
        let repo = Repo::new("diff-paths");
        repo.write("nested/keep", "keep\n");
        let names = [
            "space name",
            "tab\tand\nnewline",
            ":(glob)*",
            "-option",
            "star*?[x]",
        ];
        for name in names {
            repo.write(name, "old\n");
            assert_eq!(
                read(&repo.0, Path::new(name), Side::Unstaged)
                    .unwrap()
                    .added,
                1
            );
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        for name in names {
            repo.write(name, "new\n");
            let diff = read(&repo.0.join("nested"), Path::new(name), Side::Unstaged).unwrap();
            assert_eq!(render(&diff), "@@ -1,1 +1,1 @@\n-old\n+new\n");
        }
        repo.git(&["add", "."]);
        for name in names {
            assert_eq!(
                read(&repo.0, Path::new(name), Side::Staged)
                    .unwrap()
                    .removed,
                1
            );
        }
    }

    #[test]
    fn staged_rename_compares_with_the_source_even_with_unusual_names() {
        let repo = Repo::new("diff-rename");
        let old = "old file\nname";
        let new = ":(glob)new*\tname";
        let before: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        repo.write(old, &before);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        repo.git(&["--literal-pathspecs", "mv", "--", old, new]);
        assert_eq!(
            read(&repo.0, Path::new(new), Side::Staged).unwrap(),
            FileDiff::default()
        );
        repo.write(new, &before.replace("line 10\n", "line ten\n"));
        repo.git(&["add", "."]);
        let diff = read(&repo.0, Path::new(new), Side::Staged).unwrap();
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(
            diff.lines
                .iter()
                .find(|l| l.kind == Kind::Added)
                .unwrap()
                .new,
            Some(10)
        );
    }

    // APFS requires valid Unicode file names; Linux filesystems support these.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn reads_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let repo = Repo::new("diff-non-utf8-paths");
        let path = Path::new(std::ffi::OsStr::from_bytes(b"file-\xff"));
        std::fs::write(repo.0.join(path), "before\n").unwrap();
        assert_eq!(read(&repo.0, path, Side::Unstaged).unwrap().added, 1);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        std::fs::write(repo.0.join(path), "after\n").unwrap();
        assert_eq!(read(&repo.0, path, Side::Unstaged).unwrap().removed, 1);
        repo.git(&["add", "."]);
        assert_eq!(read(&repo.0, path, Side::Staged).unwrap().removed, 1);
    }

    #[cfg(unix)]
    #[test]
    fn reads_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;
        let repo = Repo::new("diff-symlinks");
        symlink("missing", repo.0.join("link")).unwrap();
        let link = Path::new("link");
        assert_eq!(
            render(&read(&repo.0, link, Side::Unstaged).unwrap()),
            "@@ -0,0 +1,1 @@\n+missing\n"
        );
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        std::fs::remove_file(repo.0.join("link")).unwrap();
        symlink("somewhere-else", repo.0.join("link")).unwrap();
        assert_eq!(
            render(&read(&repo.0, link, Side::Unstaged).unwrap()),
            "@@ -1,1 +1,1 @@\n-missing\n+somewhere-else\n"
        );
    }

    #[test]
    fn unchanged_staged_file_ignores_other_patches() {
        let repo = Repo::new("diff-staged-selection");
        repo.write("same", "unchanged\n");
        repo.write("modified", "before\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        repo.write("modified", "after\n");
        repo.git(&["add", "."]);
        assert_eq!(
            read(&repo.0, Path::new("same"), Side::Staged).unwrap(),
            FileDiff::default()
        );
    }

    #[test]
    fn conflicts_compare_with_ours_without_combined_hunks() {
        for deleted in [false, true] {
            let repo = Repo::new(if deleted {
                "diff-conflict-deleted"
            } else {
                "diff-conflict"
            });
            repo.write("file", "base\n");
            repo.git(&["add", "."]);
            repo.git(&["commit", "-qm", "base"]);
            repo.git(&["checkout", "-qb", "topic"]);
            repo.write("file", "theirs\n");
            repo.git(&["commit", "-qam", "theirs"]);
            repo.git(&["checkout", "-q", "main"]);
            if deleted {
                repo.git(&["rm", "-q", "file"]);
            } else {
                repo.write("file", "ours\n");
            }
            repo.git(&["commit", "-qam", "ours"]);
            assert_eq!(
                command::output(&repo.0, ["merge", "--no-edit", "topic"])
                    .unwrap()
                    .status
                    .code(),
                Some(1)
            );
            let diff = read(&repo.0, Path::new("file"), Side::Unstaged).unwrap();
            assert_eq!(diff.removed, 0);
            assert_eq!(diff.added, if deleted { 1 } else { 4 });
            assert!(diff.note.is_none());
            assert_eq!(
                read(&repo.0, Path::new("file"), Side::Staged)
                    .unwrap()
                    .note
                    .as_deref(),
                Some("Unmerged file")
            );
        }
    }

    #[test]
    fn gitlinks_do_not_require_the_commit_object_in_the_parent_repository() {
        let repo = Repo::new("diff-gitlink");
        let id = "1234567890123456789012345678901234567890";
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{id},module"),
        ]);
        assert_eq!(
            render(&read(&repo.0, Path::new("module"), Side::Staged).unwrap()),
            format!("@@ -0,0 +1,1 @@\n+Subproject commit {id}\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn clean_filters_apply_but_external_diff_and_textconv_cannot_replace_patches() {
        let repo = Repo::new("diff-drivers");
        repo.write(".gitattributes", "*.txt filter=upper diff=custom\n");
        repo.git(&["config", "filter.upper.clean", "tr a-z A-Z"]);
        repo.git(&[
            "config",
            "diff.custom.command",
            "touch external-ran; exit 1",
        ]);
        repo.git(&["config", "diff.custom.textconv", "touch textconv-ran; cat"]);
        repo.git(&["config", "color.diff", "always"]);
        repo.git(&["config", "diff.suppressBlankEmpty", "true"]);
        repo.git(&["config", "diff.context", "0"]);
        repo.write("a.txt", "before\n\ncontext\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "init"]);
        repo.write("a.txt", "BEFORE\n\nCONTEXT\n");
        assert_eq!(
            read(&repo.0, Path::new("a.txt"), Side::Unstaged).unwrap(),
            FileDiff::default()
        );
        repo.write("a.txt", "after\n\ncontext\n");
        let diff = read(&repo.0, Path::new("a.txt"), Side::Unstaged).unwrap();
        assert_eq!(
            render(&diff),
            "@@ -1,3 +1,3 @@\n-BEFORE\n+AFTER\n \n CONTEXT\n"
        );
        repo.git(&["add", "."]);
        assert_eq!(
            read(&repo.0, Path::new("a.txt"), Side::Staged).unwrap(),
            diff
        );
        assert!(!repo.0.join("external-ran").exists());
        assert!(!repo.0.join("textconv-ran").exists());
    }
}
