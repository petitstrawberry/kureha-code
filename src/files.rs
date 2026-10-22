use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

pub fn read(path: &Path) -> io::Result<String> {
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Choose a regular text file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other(
            "Files larger than 16 MiB are not supported yet",
        ));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("This file is not UTF-8 text"))
}

/// Stage and sync beside the destination before replacing it. Resolve symlinks
/// so saving through one does not replace the link itself.
pub fn save(path: &Path, text: &str, crlf: bool) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(meta) if !meta.is_file() => {
            return Err(io::Error::other("The destination is not a regular file"));
        }
        Ok(_) => path.to_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(error) => return Err(error),
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".kureha-save-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(target_os = "scarlet")]
    let mut file = {
        let permissions = scarlet_fs::metadata(&target).ok().map(|m| m.permissions());
        scarlet_fs::create_new_with_permissions(&temporary, permissions.as_ref())?
    };
    #[cfg(not(target_os = "scarlet"))]
    let mut file = {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&temporary)?
    };
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(temporary.clone());
    #[cfg(not(target_os = "scarlet"))]
    if let Ok(metadata) = fs::metadata(&target) {
        file.set_permissions(metadata.permissions())?;
    }
    if crlf {
        file.write_all(text.replace('\n', "\r\n").as_bytes())?;
    } else {
        file.write_all(text.as_bytes())?;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_save_preserves_crlf_and_failed_save_keeps_destination() {
        let root = std::env::temp_dir().join(format!("kureha-save-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("source.rs");
        save(&path, "let 名前 = 1;\n", true).unwrap();
        assert_eq!(read(&path).unwrap(), "let 名前 = 1;\r\n");
        save(&path, "let 名前 = 2;\n", true).unwrap();
        assert_eq!(read(&path).unwrap(), "let 名前 = 2;\r\n");
        let directory = root.join("folder");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("keep"), "untouched").unwrap();
        assert!(save(&directory, "replacement", false).is_err());
        assert_eq!(
            fs::read_to_string(directory.join("keep")).unwrap(),
            "untouched"
        );
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kureha-save-")
        }));
        fs::remove_dir_all(root).unwrap();
    }
}
