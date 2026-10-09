use anyhow::{Result, ensure};
use std::io::IsTerminal;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const MAX_PAINT: usize = 16 * 1024 * 1024;
pub struct Output {
    fd: OwnedFd,
    flags: i32,
    pending: Vec<u8>,
    offset: usize,
}
impl Output {
    pub fn new() -> Result<Self> {
        ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "Sheepdog requires a local interactive terminal"
        );
        let raw = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 0) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            fd,
            flags,
            pending: vec![],
            offset: 0,
        })
    }
    pub fn idle(&self) -> bool {
        self.offset == self.pending.len()
    }
    pub fn queue(&mut self, bytes: Vec<u8>) -> Result<()> {
        ensure!(self.idle(), "paint queue busy");
        ensure!(bytes.len() <= MAX_PAINT, "paint budget exceeded");
        self.pending = bytes;
        self.offset = 0;
        Ok(())
    }
    pub fn flush(&mut self) -> Result<()> {
        if self.idle() {
            return Ok(());
        }
        let bytes = &self.pending[self.offset..];
        let n = unsafe { libc::write(self.fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(e.into());
        }
        ensure!(n != 0, "TTY write returned zero");
        self.offset += n as usize;
        if self.idle() {
            self.pending.clear();
            self.offset = 0;
        }
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.flags) };
    }
}
pub struct Terminal;
impl Terminal {
    pub fn enter(output: &mut Output) -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let guard = Self;
        output.queue(b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h".to_vec())?;
        output.flush()?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        // Discard our unfinished paint, not input. Cleanup cannot wait for a
        // blocked display consumer; the kernel termios restore is out of band.
        let bytes=b"\x1b[?2026l\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1000l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l";
        unsafe {
            libc::tcflush(1, libc::TCOFLUSH);
            libc::write(1, bytes.as_ptr().cast(), bytes.len());
        }
    }
}
