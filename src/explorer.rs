//! Directory enumeration is shallow and bounded; callers run `scan` on workers.
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub const ENTRIES_PER_DIRECTORY: usize = 512;
pub const MAX_CACHED_ENTRIES: usize = 4096;
pub const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}

#[derive(Debug)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub fn scan(path: &Path) -> io::Result<Listing> {
    let mut entries = Vec::new();
    let mut truncated = false;
    // Read at most one extra entry to establish truncation. Never recurse or
    // follow directory symlinks, even when a project contains cycles.
    for (index, entry) in fs::read_dir(path)?.enumerate() {
        if index == ENTRIES_PER_DIRECTORY {
            truncated = true;
            break;
        }
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() || kind.is_file() || kind.is_symlink() {
            entries.push(Entry {
                path: entry.path(),
                directory: kind.is_dir(),
            });
        }
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    Ok(Listing { entries, truncated })
}

#[derive(Default)]
struct Directory {
    expanded: bool,
    loading: bool,
    listing: Option<Listing>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: String,
    pub path: Option<PathBuf>,
    pub label: String,
    pub depth: usize,
    pub directory: bool,
    pub expanded: bool,
}

#[derive(Default)]
pub struct Explorer {
    pub root: Option<PathBuf>,
    pub generation: u64,
    directories: BTreeMap<PathBuf, Directory>,
}

impl Explorer {
    /// Keep the current tree visible until the replacement root is readable.
    pub fn begin_root_request(&mut self) -> u64 {
        self.generation += 1;
        for directory in self.directories.values_mut() {
            directory.loading = false;
        }
        self.generation
    }
    pub fn reset(&mut self) -> u64 {
        self.generation += 1;
        self.root = None;
        self.directories.clear();
        self.generation
    }
    pub fn set_root(&mut self, generation: u64, path: PathBuf, listing: Listing) -> bool {
        if self.generation != generation {
            return false;
        }
        self.directories.clear();
        self.root = Some(path.clone());
        self.directories.insert(
            path.clone(),
            Directory {
                expanded: true,
                ..Directory::default()
            },
        );
        self.complete(generation, &path, Ok(listing));
        true
    }
    pub fn toggle(&mut self, path: &Path, depth: usize) -> Result<bool, String> {
        let count: usize = self
            .directories
            .values()
            .filter_map(|d| d.listing.as_ref())
            .map(|l| l.entries.len())
            .sum();
        let directory = self.directories.entry(path.into()).or_default();
        directory.expanded = !directory.expanded;
        if !directory.expanded || directory.loading || directory.listing.is_some() {
            return Ok(false);
        }
        if depth >= MAX_DEPTH || count >= MAX_CACHED_ENTRIES {
            directory.error = Some("Explorer limit reached; open this folder as the root".into());
            return Ok(false);
        }
        directory.error = None;
        directory.loading = true;
        Ok(true)
    }
    pub fn complete(
        &mut self,
        generation: u64,
        path: &Path,
        result: Result<Listing, String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let count: usize = self
            .directories
            .iter()
            .filter(|(p, _)| p.as_path() != path)
            .filter_map(|(_, d)| d.listing.as_ref())
            .map(|l| l.entries.len())
            .sum();
        let Some(directory) = self.directories.get_mut(path) else {
            return false;
        };
        directory.loading = false;
        match result {
            Ok(mut listing) => {
                let remaining = MAX_CACHED_ENTRIES.saturating_sub(count);
                if listing.entries.len() > remaining {
                    listing.entries.truncate(remaining);
                    listing.truncated = true;
                }
                directory.listing = Some(listing);
                directory.error = None;
            }
            Err(error) => directory.error = Some(error),
        }
        true
    }
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(root) = &self.root {
            self.append(root, 0, true, &mut rows);
        }
        rows
    }
    fn append(&self, path: &Path, depth: usize, is_directory: bool, rows: &mut Vec<Row>) {
        let directory = self.directories.get(path);
        let expanded = directory.is_some_and(|d| d.expanded);
        rows.push(Row {
            key: format!("{}:entry", path.display()),
            path: Some(path.into()),
            label: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            depth,
            directory: is_directory,
            expanded,
        });
        if !expanded {
            return;
        }
        let directory = directory.unwrap();
        if let Some(listing) = &directory.listing {
            for entry in &listing.entries {
                self.append(&entry.path, depth + 1, entry.directory, rows);
            }
        }
        let message = if directory.loading {
            Some("Loading…".into())
        } else if let Some(error) = &directory.error {
            Some(error.clone())
        } else if directory.listing.as_ref().is_some_and(|l| l.truncated) {
            Some("Listing limited — use Open File for other entries".into())
        } else if directory
            .listing
            .as_ref()
            .is_some_and(|l| l.entries.is_empty())
        {
            Some("Empty folder".into())
        } else {
            None
        };
        if let Some(label) = message {
            rows.push(Row {
                key: format!("{}:status", path.display()),
                path: None,
                label,
                depth: depth + 1,
                directory: false,
                expanded: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_is_lazy_and_old_root_results_are_rejected() {
        let mut explorer = Explorer::default();
        let generation = explorer.reset();
        assert!(explorer.set_root(
            generation,
            "/project".into(),
            Listing {
                entries: vec![Entry {
                    path: "/project/src".into(),
                    directory: true
                }],
                truncated: false
            }
        ));
        assert_eq!(explorer.rows().len(), 2);
        assert!(explorer.toggle(Path::new("/project/src"), 1).unwrap());
        assert!(explorer.rows().last().unwrap().label.contains("Loading"));
        explorer.reset();
        assert!(!explorer.complete(
            generation,
            Path::new("/project/src"),
            Ok(Listing {
                entries: vec![],
                truncated: false
            })
        ));
        assert!(explorer.rows().is_empty());
    }
    #[test]
    fn enumeration_caps_large_directories_without_scanning_children() {
        let root = std::env::temp_dir().join(format!("kureha-tree-{}", std::process::id()));
        fs::create_dir_all(root.join("nested/deeper")).unwrap();
        for index in 0..ENTRIES_PER_DIRECTORY + 5 {
            fs::write(root.join(format!("{index:04}.txt")), "").unwrap();
        }
        let listing = scan(&root).unwrap();
        assert!(listing.truncated);
        assert!(listing.entries.len() <= ENTRIES_PER_DIRECTORY);
        assert!(
            listing
                .entries
                .iter()
                .all(|entry| entry.path.parent() == Some(root.as_path()))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
