//! Platform PTY ownership. No platform operation in this module runs on the UI.
#[cfg(all(unix, not(target_os = "scarlet")))]
#[path = "unix.rs"]
mod platform;
#[cfg(target_os = "scarlet")]
#[path = "native.rs"]
mod platform;
#[cfg(any(unix, target_os = "scarlet"))]
pub(super) use platform::Pty;

#[cfg(not(any(unix, target_os = "scarlet")))]
pub(super) struct Pty;
#[cfg(not(any(unix, target_os = "scarlet")))]
impl Pty {
    pub fn open(_: &std::path::Path, _: (u16, u16)) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "PTY terminal is supported on macOS, Unix and Scarlet; Windows ConPTY is not implemented",
        ))
    }
    pub fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        unreachable!()
    }
    pub fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        unreachable!()
    }
    pub fn resize(&self, _: u16, _: u16) -> std::io::Result<()> {
        unreachable!()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<Option<i32>>> {
        unreachable!()
    }
}
