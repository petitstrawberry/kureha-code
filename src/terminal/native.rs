//! Native PTY ABI adapted from Scarlet's MIT-licensed terminal (pty.rs and
//! terminal/main.rs), Copyright Scarlet contributors. Uses published git crates;
//! no sibling source or legacy scarlet_std runtime is linked into this app.
use scarlet_os::{
    Handle,
    ipc::{ProcessControl, send_process_control, send_process_control_to_group},
    process::{WAIT_NOHANG, waitpid},
};
use scarlet_sys::{Syscall, syscall0, syscall1, syscall3, syscall5};
use std::{ffi::CString, io, path::Path, thread, time::Duration};

pub(crate) struct Pty {
    master: Option<Handle>,
    pid: u32,
    reaped: bool,
}
fn error(value: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("native PTY: {value:?}"))
}
fn control(handle: &Handle, command: u32, value: usize) -> io::Result<i32> {
    // SAFETY: callers use only fixed scalar PTY/TTY operations, never pointers.
    unsafe { handle.control(command, value) }.map_err(error)
}
impl Pty {
    pub fn open(cwd: &Path, size: (u16, u16)) -> io::Result<Self> {
        // Validate cwd before clone; the child must use no allocator or locks.
        if !cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "terminal working directory is not a directory",
            ));
        }
        let cwd = CString::new(cwd.as_os_str().as_encoded_bytes())?;
        let master = Handle::open("/dev/ptmx", 0x2 | 0x100).map_err(error)?;
        control(&master, 0x5350_0002, 0)?; // unlock slave
        let number = control(&master, 0x5350_0001, 0)?;
        let slave = Handle::open(&format!("/dev/pts/{number}"), 0x2 | 0x100).map_err(error)?;
        control(
            &master,
            0x5354_0005,
            ((size.0 as usize) << 16) | size.1 as usize,
        )?;
        master.set_nonblocking(true).map_err(error)?;
        let mut environment: Vec<CString> = std::env::vars_os()
            .filter(|(key, _)| key != "TERM" && key != "COLORTERM")
            .map(|(key, value)| {
                let mut bytes = key.as_encoded_bytes().to_vec();
                bytes.push(b'=');
                bytes.extend(value.as_encoded_bytes());
                CString::new(bytes)
            })
            .chain([
                CString::new("TERM=xterm-256color"),
                CString::new("COLORTERM=truecolor"),
            ])
            .collect::<Result<_, _>>()?;
        // Graphical service launches can have no PATH; shells still need the
        // distribution’s command directories. Preserve any explicit user PATH.
        if std::env::var_os("PATH").is_none_or(|path| path.is_empty()) {
            environment.retain(|entry| !entry.as_bytes().starts_with(b"PATH="));
            environment.push(CString::new("PATH=/bin:/usr/bin").unwrap());
        }
        let mut envp: Vec<*const u8> = environment
            .iter()
            .map(|value| value.as_ptr().cast())
            .collect();
        envp.push(std::ptr::null());
        let argv = [c"/bin/sh".as_ptr().cast::<u8>(), std::ptr::null()];
        let master_raw = master.as_raw() as usize;
        let slave_raw = slave.as_raw() as usize;
        // SAFETY: flags=0 copies VM, file table, cwd and stack into a separate
        // process. Unlike a general fork wrapper, the child below never reads
        // allocator state, allocates, drops Rust owners, takes locks, unwinds or
        // returns. All CString/pointer storage is prepared and immutable here.
        // Only raw scalar/borrowed-buffer syscalls occur before exec/ExitGroup.
        let pid = unsafe { syscall5(Syscall::Clone, 0, 0, 0, 0, 0) } as i32;
        if pid < 0 {
            return Err(io::Error::other("native PTY process clone failed"));
        }
        if pid == 0 {
            // SAFETY: these are the child's independent handle table and session;
            // pointers address immutable prepared NUL-terminated buffers. Closing
            // inherited handles does not affect parent owners. No Rust Drop runs.
            unsafe {
                syscall1(Syscall::HandleClose, master_raw);
                if syscall0(Syscall::CreateSession) == usize::MAX {
                    child_exit();
                }
                if syscall3(Syscall::HandleControl, slave_raw, 0x5354_0018, 0) == usize::MAX {
                    child_exit();
                }
                let pgid = syscall1(Syscall::GetProcessGroupId, 0);
                if pgid == usize::MAX
                    || syscall3(Syscall::HandleControl, slave_raw, 0x5354_000E, pgid) == usize::MAX
                {
                    child_exit();
                }
                // The source must survive closing stdio, even if the application
                // started with one of 0,1,2 closed. Duplicate it until >=3.
                let mut source = syscall1(Syscall::HandleDuplicate, slave_raw);
                while source < 3 {
                    source = syscall1(Syscall::HandleDuplicate, slave_raw);
                }
                if source == usize::MAX {
                    child_exit();
                }
                for target in 0..3 {
                    syscall1(Syscall::HandleClose, target);
                    if syscall1(Syscall::HandleDuplicate, source) != target {
                        child_exit();
                    }
                }
                syscall1(Syscall::HandleClose, source);
                if slave_raw >= 3 {
                    syscall1(Syscall::HandleClose, slave_raw);
                }
                if syscall1(Syscall::VfsChangeDirectory, cwd.as_ptr() as usize) == usize::MAX {
                    child_exit();
                }
                syscall3(
                    Syscall::Execve,
                    c"/bin/sh".as_ptr() as usize,
                    argv.as_ptr() as usize,
                    envp.as_ptr() as usize,
                );
                child_exit();
            }
        }
        drop(slave);
        Ok(Self {
            master: Some(master),
            pid: pid as u32,
            reaped: false,
        })
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.master
            .as_ref()
            .unwrap()
            .as_stream()
            .map_err(error)?
            .read(bytes)
            .map_err(stream_error)
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.master
            .as_ref()
            .unwrap()
            .as_stream()
            .map_err(error)?
            .write(bytes)
            .map_err(stream_error)
    }
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        control(
            self.master.as_ref().unwrap(),
            0x5354_0005,
            ((cols as usize) << 16) | rows as usize,
        )
        .map(|_| ())
    }
    pub fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        let (pid, status) = waitpid(self.pid as i32, WAIT_NOHANG);
        if pid < 0 {
            return Err(io::Error::other("native shell wait failed"));
        }
        if pid == 0 {
            return Ok(None);
        }
        self.reaped = true;
        Ok(Some(Some(status)))
    }
}
unsafe fn child_exit() -> ! {
    // SAFETY: terminate only the copied child process; no borrowed parent memory.
    unsafe {
        syscall1(Syscall::ExitGroup, 127);
    }
    loop {
        std::hint::spin_loop();
    }
}
fn stream_error(value: scarlet_os::handle::capability::StreamError) -> io::Error {
    use scarlet_os::handle::capability::StreamError;
    match value {
        StreamError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        StreamError::Interrupted => io::ErrorKind::Interrupted.into(),
        other => error(other),
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        let foreground = self
            .master
            .as_ref()
            .and_then(|m| control(m, 0x5354_000D, 0).ok())
            .filter(|p| *p > 0);
        if let Some(pgid) = foreground {
            let _ = send_process_control_to_group(Some(pgid as u32), ProcessControl::Hangup);
        }
        let _ = send_process_control_to_group(Some(self.pid), ProcessControl::Hangup);
        self.master.take();
        if !self.reaped {
            let _ = send_process_control(self.pid, ProcessControl::Terminate);
            for _ in 0..10 {
                let (pid, _) = waitpid(self.pid as i32, WAIT_NOHANG);
                if pid != 0 {
                    self.reaped = true;
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        if let Some(pgid) = foreground {
            let _ = send_process_control_to_group(Some(pgid as u32), ProcessControl::Kill);
        }
        let _ = send_process_control_to_group(Some(self.pid), ProcessControl::Kill);
        if !self.reaped {
            let _ = send_process_control(self.pid, ProcessControl::Kill);
            let _ = waitpid(self.pid as i32, 0);
        }
    }
}
