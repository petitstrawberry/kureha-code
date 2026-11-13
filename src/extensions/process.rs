//! Programs an extension may run. A program must be declared in the
//! extension's `[capabilities] process` *and* allowed by the user; the
//! manifest alone is the author's request, not consent.
//!
//! A run is direct (never through a shell), finds the program only in
//! absolute `PATH` entries (never in the workspace), and is bounded in input,
//! output and time. Granting a program trusts the extension with everything
//! that program can do with arbitrary arguments, under the user's account.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_BYTES: usize = 4096;
/// Standard output beyond this fails the run; truncated output would corrupt
/// a formatter's result.
pub const MAX_STDOUT_BYTES: usize = 512 * 1024;
/// Standard error is diagnostic only: the rest is read and dropped.
pub const MAX_STDERR_BYTES: usize = 16 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest an environment may take to compute: a Nix dev shell can
/// build for minutes the first time.
pub const MAX_ENVIRONMENT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Runs allowed within one command or format call.
pub const MAX_RUNS_PER_CALL: u32 = 8;
/// After the program exits, how long its pipes may stay open (for example,
/// held by a background child) before the rest of its process group is killed.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Programs the user allowed each extension to run, by extension id.
/// Clones share one table, so a change applies to running workers at once.
#[derive(Clone, Debug, Default)]
pub struct Grants(Arc<RwLock<BTreeMap<String, BTreeSet<String>>>>);
impl Grants {
    pub fn new(table: BTreeMap<String, BTreeSet<String>>) -> Self {
        Self(Arc::new(RwLock::new(table)))
    }
    pub fn allows(&self, extension: &str, program: &str) -> bool {
        self.0
            .read()
            .unwrap()
            .get(extension)
            .is_some_and(|programs| programs.contains(program))
    }
    /// Replaces the whole table, as read from the settings file.
    pub fn replace(&self, table: BTreeMap<String, BTreeSet<String>>) {
        *self.0.write().unwrap() = table;
    }
}

static GRANTS: OnceLock<Grants> = OnceLock::new();
/// This editor's grants; the UI keeps them in sync with the settings file.
pub fn grants() -> &'static Grants {
    GRANTS.get_or_init(Grants::default)
}

/// The palette command that grants `program` to `extension`.
pub fn allow_command_label(extension: &str, program: &str) -> String {
    format!("Extensions: Allow {extension} to Run {program}")
}
/// The palette command that withdraws the grant.
pub fn revoke_command_label(extension: &str, program: &str) -> String {
    format!("Extensions: Stop {extension} Running {program}")
}
/// What to tell the user when a run is refused for lack of a grant.
pub fn permission_hint(extension: &str, program: &str) -> String {
    format!(
        "needs your permission to run `{program}`; run \"{}\" from the command palette",
        allow_command_label(extension, program)
    )
}

/// Finds `program` (a bare name) in the absolute entries of `path`.
/// Relative entries such as `.` or `node_modules/.bin` would resolve against
/// the child's working directory, inside the workspace, so they are skipped.
pub fn resolve(program: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    #[cfg(target_os = "scarlet")]
    let path = path
        .filter(|p| !p.is_empty())
        .or(Some(OsStr::new("/bin:/usr/bin")));
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// One run: an absolute program path, its arguments and where it runs.
#[derive(Clone, Debug)]
pub struct Run {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Piped to standard input, then closed. `None` gives an empty input.
    pub stdin: Option<Vec<u8>>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    /// Give the program Kureha's environment overlay (direnv's, say). Off
    /// when computing that environment, which must start from the base.
    pub overlay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The exit code, or `None` if a signal ended the program.
    pub status: Option<i32>,
    pub stdout: String,
    /// At most `MAX_STDERR_BYTES`, decoded leniently.
    pub stderr: String,
}

enum Stream {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// Reads `pipe` to the end, keeping at most `limit` bytes. With `strict`, more
/// than `limit` sets `overflow` and stops reading, so the run can be failed.
fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    limit: usize,
    strict: bool,
    overflow: Arc<AtomicBool>,
    done: mpsc::Sender<Stream>,
    wrap: fn(Vec<u8>) -> Stream,
) -> Result<(), String> {
    thread::Builder::new()
        .name("extension-process-output".into())
        .spawn(move || {
            let mut kept = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = limit.saturating_sub(kept.len());
                        kept.extend_from_slice(&buffer[..n.min(room)]);
                        if n > room && strict {
                            overflow.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = done.send(wrap(kept));
        })
        .map(drop)
        .map_err(|e| format!("cannot read program output: {e}"))
}

/// Kills the program and, on Unix, everything it started in its group.
fn kill(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // The child leads its own group (see `process_group(0)` below).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
/// After a normal exit: stop descendants that still hold the output pipes.
fn kill_group(child: &Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// Runs the program to completion. Fails on timeout, on standard output over
/// `MAX_STDOUT_BYTES`, or on standard output that is not UTF-8.
pub fn run(run: Run) -> Result<Output, String> {
    let name = run.program.file_name().map_or_else(
        || run.program.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    let mut command = Command::new(&run.program);
    if run.overlay {
        crate::environment::apply(&mut command);
    }
    command
        .args(&run.args)
        .current_dir(&run.cwd)
        .stdin(if run.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start `{name}`: {e}"))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let (done, outputs) = mpsc::channel();
    let started = (|| {
        if let Some(input) = run.stdin {
            let mut stdin = child.stdin.take().ok_or("missing standard input")?;
            // A separate writer: the program may fill its output pipes before
            // it reads all of a large input.
            thread::Builder::new()
                .name("extension-process-input".into())
                .spawn(move || {
                    let _ = stdin.write_all(&input);
                })
                .map_err(|e| format!("cannot write program input: {e}"))?;
        }
        let stdout = child.stdout.take().ok_or("missing standard output")?;
        let stderr = child.stderr.take().ok_or("missing standard error")?;
        spawn_reader(
            stdout,
            MAX_STDOUT_BYTES,
            true,
            overflow.clone(),
            done.clone(),
            Stream::Stdout,
        )?;
        spawn_reader(
            stderr,
            MAX_STDERR_BYTES,
            false,
            overflow.clone(),
            done,
            Stream::Stderr,
        )
    })();
    if let Err(e) = started {
        kill(&mut child);
        return Err(e);
    }
    let deadline = Instant::now() + run.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill(&mut child);
                return Err(format!("cannot wait for `{name}`: {e}"));
            }
        }
        if overflow.load(Ordering::Relaxed) {
            kill(&mut child);
            return Err(format!(
                "`{name}` printed more than {} KiB",
                MAX_STDOUT_BYTES / 1024
            ));
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            return Err(format!(
                "`{name}` did not finish within {} s and was stopped",
                run.timeout.as_secs_f32()
            ));
        }
        thread::sleep(Duration::from_millis(2));
    };
    let (mut stdout, mut stderr) = (None, None);
    let mut killed_group = false;
    while stdout.is_none() || stderr.is_none() {
        match outputs.recv_timeout(DRAIN_GRACE) {
            Ok(Stream::Stdout(bytes)) => stdout = Some(bytes),
            Ok(Stream::Stderr(bytes)) => stderr = Some(bytes),
            Err(mpsc::RecvTimeoutError::Timeout) if !killed_group => {
                kill_group(&child);
                killed_group = true;
            }
            Err(_) => return Err(format!("the output of `{name}` did not close")),
        }
    }
    if overflow.load(Ordering::Relaxed) {
        return Err(format!(
            "`{name}` printed more than {} KiB",
            MAX_STDOUT_BYTES / 1024
        ));
    }
    let stdout = String::from_utf8(stdout.unwrap_or_default())
        .map_err(|_| format!("the output of `{name}` is not UTF-8 text"))?;
    Ok(Output {
        status: status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn program(name: &str) -> PathBuf {
        resolve(name, std::env::var_os("PATH").as_deref())
            .unwrap_or_else(|| panic!("`{name}` is on PATH in the test environment"))
    }
    fn start(
        name: &str,
        args: &[&str],
        stdin: Option<&str>,
        timeout: Duration,
    ) -> Result<Output, String> {
        run(Run {
            program: program(name),
            args: args.iter().map(|a| a.to_string()).collect(),
            stdin: stdin.map(|s| s.as_bytes().to_vec()),
            cwd: std::env::temp_dir(),
            timeout,
            overlay: true,
        })
    }

    #[test]
    fn grants_are_per_extension_and_program_and_shared_by_clones() {
        let grants = Grants::default();
        let worker = grants.clone();
        assert!(!worker.allows("ex.fmt", "ruff"));
        grants.replace(BTreeMap::from([(
            "ex.fmt".into(),
            BTreeSet::from(["ruff".to_string()]),
        )]));
        assert!(worker.allows("ex.fmt", "ruff"));
        assert!(!worker.allows("ex.fmt", "sh"));
        assert!(!worker.allows("ex.other", "ruff"));
    }

    #[test]
    fn programs_are_found_only_in_absolute_path_entries() {
        let dir = std::env::temp_dir().join(format!("kureha-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let tool = dir.join("bin/kureha-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("bin/not-executable"), "").unwrap();
        let absolute = dir.join("bin").into_os_string();
        assert_eq!(resolve("kureha-fake-tool", Some(&absolute)), Some(tool));
        assert_eq!(resolve("not-executable", Some(&absolute)), None);
        // Relative entries (resolved against the child's workspace directory)
        // are never searched.
        let relative = format!("bin:.:{}", dir.join("missing").display());
        assert_eq!(
            resolve("kureha-fake-tool", Some(OsStr::new(&relative))),
            None
        );
        assert_eq!(resolve("kureha-fake-tool", None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn input_output_and_exit_status_round_trip() {
        let output = start("cat", &[], Some("héllo\n"), DEFAULT_TIMEOUT).unwrap();
        assert_eq!(
            output,
            Output {
                status: Some(0),
                stdout: "héllo\n".into(),
                stderr: String::new()
            }
        );
        let output = start(
            "sh",
            &["-c", "echo oops >&2; exit 3"],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap();
        assert_eq!((output.status, output.stderr.as_str()), (Some(3), "oops\n"));
        // Large input and output in both directions must not deadlock.
        let big = "x".repeat(300 * 1024);
        assert_eq!(
            start("cat", &[], Some(&big), DEFAULT_TIMEOUT)
                .unwrap()
                .stdout,
            big
        );
    }

    #[test]
    fn long_runs_are_killed_and_reaped() {
        let started = Instant::now();
        let error = start("sleep", &["30"], None, Duration::from_millis(50)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn oversized_or_binary_output_fails_instead_of_truncating() {
        let error = start("yes", &[], None, DEFAULT_TIMEOUT).unwrap_err();
        assert!(error.contains("more than"), "{error}");
        let error = start("printf", &["\\377"], None, DEFAULT_TIMEOUT).unwrap_err();
        assert!(error.contains("not UTF-8"), "{error}");
    }

    #[test]
    fn a_background_child_holding_the_output_does_not_hang_the_run() {
        let started = Instant::now();
        let output = start("sh", &["-c", "sleep 30 & echo done"], None, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(output.stdout, "done\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
