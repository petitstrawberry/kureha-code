//! The single process boundary for Git. Paths are arguments, never shell text.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) const MISSING: &str = "Git is not installed or is not on PATH";
pub(super) const SCARLET: &str = "Git is not available on Scarlet yet";

pub(super) fn availability() -> Result<(), String> {
    if cfg!(target_os = "scarlet") {
        return Err(SCARLET.into());
    }
    static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
    AVAILABLE.get_or_init(|| probe(OsStr::new("git"))).clone()
}

fn probe(program: &OsStr) -> Result<(), String> {
    let output = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MISSING.into()
            } else {
                format!("Cannot run Git: {e}")
            }
        })?;
    checked(output).map(|_| ())
}

fn command<I, S>(folder: &Path, args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    availability()?;
    let mut command = Command::new("git");
    command
        .args(["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"])
        .args(args)
        .current_dir(folder)
        .stdin(Stdio::null())
        // Background status must not contend with an explicit Git operation.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    // An editor launched from a hook or another repository must still operate
    // on the folder selected in the workbench.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        command.env_remove(key);
    }
    // Fixtures use repository-local config and must not run the developer's
    // global hooks, filters, signing or fsmonitor settings.
    #[cfg(test)]
    command.env("GIT_CONFIG_NOSYSTEM", "1").env(
        "GIT_CONFIG_GLOBAL",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
    );
    Ok(command)
}

pub(super) fn output<I, S>(folder: &Path, args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    command(folder, args)?
        .output()
        .map_err(|e| format!("Cannot run Git: {e}"))
}

pub(super) fn run<I, S>(folder: &Path, args: I) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    checked(output(folder, args)?)
}

pub(super) fn checked(output: Output) -> Result<Vec<u8>, String> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if message.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            message
        };
        Err(if message.is_empty() {
            format!("Git exited with {}", output.status)
        } else {
            message
        })
    }
}

/// Drain both pipes while waiting, so a large status cannot deadlock. A newer
/// request terminates and reaps the old process (and its helpers on Unix).
pub(super) fn read<I, S>(folder: &Path, args: I, interrupt: &AtomicBool) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if interrupt.load(Ordering::Relaxed) {
        return Err("Git read interrupted".into());
    }
    let mut command = command(folder, args)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(all(unix, not(target_os = "scarlet")))]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run Git: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    std::thread::scope(|scope| {
        use std::io::Read;
        let drain = |mut pipe: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        };
        let out = scope.spawn(move || drain(Box::new(stdout)));
        let err = scope.spawn(move || drain(Box::new(stderr)));
        let mut exit_status = None;
        let status = loop {
            if interrupt.load(Ordering::Relaxed) {
                #[cfg(all(unix, not(target_os = "scarlet")))]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break Err("Git read interrupted".to_string());
            }
            if exit_status.is_none() {
                match child.try_wait() {
                    Ok(status) => exit_status = status,
                    Err(e) => {
                        #[cfg(all(unix, not(target_os = "scarlet")))]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(format!("Cannot wait for Git: {e}"));
                    }
                }
            }
            // A helper can outlive Git and retain its output pipes. Continue
            // accepting cancellation until those pipes close too.
            if let Some(status) = exit_status
                && out.is_finished()
                && err.is_finished()
            {
                break Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let stdout = out
            .join()
            .map_err(|_| "Cannot read Git output")?
            .map_err(|e| e.to_string())?;
        let stderr = err
            .join()
            .map_err(|_| "Cannot read Git errors")?
            .map_err(|e| e.to_string())?;
        checked(Output {
            status: status?,
            stdout,
            stderr,
        })
    })
}

pub(super) fn path(bytes: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|e| e.to_string())
    }
}

pub(super) fn workdir(folder: &Path) -> Result<PathBuf, String> {
    let output = run(folder, ["rev-parse", "--show-toplevel"])?;
    // Remove the record terminator, not whitespace that belongs to the path.
    path(output.strip_suffix(b"\n").unwrap_or(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_disables_git() {
        let missing = std::env::temp_dir().join("kureha-git-executable-does-not-exist");
        assert_eq!(probe(missing.as_os_str()).unwrap_err(), MISSING);
    }

    #[test]
    fn cancelled_reads_do_not_start_git() {
        let interrupt = AtomicBool::new(true);
        assert_eq!(
            read(Path::new("."), ["status"], &interrupt).unwrap_err(),
            "Git read interrupted"
        );
    }

    #[cfg(all(unix, not(target_os = "scarlet")))]
    #[test]
    fn cancellation_reaps_git_and_its_helpers() {
        use std::os::unix::fs::PermissionsExt;
        let repo = crate::git::testing::Repo::new("cancel-process");
        let helper = repo.0.join(".git/slow-fsmonitor");
        std::fs::write(&helper, "#!/bin/sh\ntouch .git/monitor-started\nsleep 30\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        repo.git(&["config", "core.fsmonitor", helper.to_str().unwrap()]);
        let interrupt = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let handle =
                scope.spawn(|| read(&repo.0, ["status", "--porcelain=v2", "-z"], &interrupt));
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !repo.0.join(".git/monitor-started").exists() && std::time::Instant::now() < until
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            interrupt.store(true, Ordering::Relaxed);
            assert_eq!(handle.join().unwrap().unwrap_err(), "Git read interrupted");
            assert!(repo.0.join(".git/monitor-started").exists());
        });
    }

    #[cfg(all(unix, not(target_os = "scarlet")))]
    #[test]
    fn cancellation_still_works_after_git_exits_with_open_helper_pipes() {
        use std::os::unix::fs::PermissionsExt;
        let repo = crate::git::testing::Repo::new("cancel-exited-process");
        let helper = repo.0.join(".git/background-fsmonitor");
        std::fs::write(&helper, "#!/bin/sh\nprintf '%s' \"$PPID\" > .git/monitor-parent\nsleep 30 >&2 &\nprintf 'token\\0/\\0'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        repo.git(&["config", "core.fsmonitor", helper.to_str().unwrap()]);
        let interrupt = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let handle =
                scope.spawn(|| read(&repo.0, ["status", "--porcelain=v2", "-z"], &interrupt));
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut git_exited = false;
            while std::time::Instant::now() < until {
                if let Ok(pid) = std::fs::read_to_string(repo.0.join(".git/monitor-parent"))
                    && let Ok(pid) = pid.parse::<i32>()
                    && unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    git_exited = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            interrupt.store(true, Ordering::Relaxed);
            assert_eq!(handle.join().unwrap().unwrap_err(), "Git read interrupted");
            assert!(
                git_exited,
                "Git must have exited while its helper retains stderr"
            );
        });
    }
}
