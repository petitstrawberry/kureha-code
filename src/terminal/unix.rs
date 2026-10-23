use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

pub(crate) struct Pty {
    master: Option<File>,
    child: Child,
    reaped: bool,
}
impl Pty {
    pub fn open(cwd: &Path, size: (u16, u16)) -> io::Result<Self> {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut winsize = libc::winsize {
            ws_col: size.0,
            ws_row: size.1,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: output descriptors and winsize have the openpty ABI/layout;
        // null name/termios request defaults. Ownership transfers below once.
        if unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsize,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        // Keep the master out of this child and any concurrently spawned child.
        for fd in [master_fd, slave_fd] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let shell = std::env::var_os("SHELL")
            .filter(|s| !s.is_empty() && Path::new(s).is_absolute())
            .unwrap_or_else(|| "/bin/sh".into());
        let spawn = |shell: &std::ffi::OsStr| -> io::Result<Child> {
            let mut command = Command::new(shell);
            command
                .arg("-i")
                .current_dir(cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave.try_clone()?));
            // SAFETY: child uses only async-signal-safe syscalls before exec;
            // stdio is already duplicated. This gives real job control and ^C.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::tcsetpgrp(0, libc::getpid()) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.spawn()
        };
        let child = match spawn(&shell) {
            Err(error) if error.kind() == io::ErrorKind::NotFound && shell != "/bin/sh" => {
                spawn(std::ffi::OsStr::new("/bin/sh"))?
            }
            result => result?,
        };
        Ok(Self {
            master: Some(master),
            child,
            reaped: false,
        })
    }
    #[cfg(test)]
    pub fn pid_for_test(&self) -> libc::pid_t {
        self.child.id() as libc::pid_t
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self.master.as_mut().unwrap().read(bytes) {
            // Linux reports EIO for the normal slave-side close.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            result => result,
        }
    }
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.master.as_mut().unwrap().write(bytes)
    }
    pub fn resize(&self, columns: u16, rows: u16) -> io::Result<()> {
        let size = libc::winsize {
            ws_col: columns,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the master is live and size is a valid winsize record.
        if unsafe {
            libc::ioctl(
                self.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &size,
            )
        } < 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        self.child.try_wait().map(|exit| {
            exit.map(|exit| {
                self.reaped = true;
                exit.code()
            })
        })
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        // Read the foreground group before closing the controlling PTY, so a
        // foreground job in its own group cannot outlive the pane.
        let foreground = self
            .master
            .as_ref()
            .map(|m| unsafe { libc::tcgetpgrp(m.as_raw_fd()) })
            .unwrap_or(-1);
        let pid = self.child.id() as libc::pid_t;
        let signal = |value| unsafe {
            if foreground > 0 && foreground != pid {
                libc::kill(-foreground, value);
            }
            libc::kill(-pid, value);
        };
        signal(libc::SIGHUP);
        self.master.take();
        if !self.reaped {
            for _ in 0..10 {
                if self.child.try_wait().ok().flatten().is_some() {
                    self.reaped = true;
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        signal(libc::SIGKILL);
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
