//! Raw pty plumbing for the OSC 52 proxy (`stashee --osc52-proxy`):
//! spawn a command on its own pty, keep its window size in step with
//! ours, and put our side into raw mode. The workspace forbids
//! `unsafe_code`; every libc call the proxy needs lives here instead,
//! behind small safe functions. Keep this crate minimal and boring.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// Open a pty sized like our stdin and spawn `argv` on its slave side,
/// as a session leader with the slave as controlling terminal — that
/// is what makes resizes reach the child as `SIGWINCH`. Returns the
/// master side and the child.
pub fn spawn_on_pty(argv: &[OsString]) -> io::Result<(OwnedFd, Child)> {
    let size = stdin_winsize().unwrap_or(libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // SAFETY: out-parameters only; the return value is checked.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty just returned these descriptors; we own them.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: as above.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_cloexec(&master)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: setsid, sigprocmask and ioctl are async-signal-safe;
    // nothing here allocates or touches locks between fork and exec.
    unsafe {
        command.pre_exec(|| {
            // the proxy blocks SIGWINCH for sigwait and the mask
            // survives exec — with it inherited, ssh and tmux would
            // never see a resize
            let mut empty = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut empty);
            if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            // stdio is already the pty slave; adopt it as the
            // controlling terminal
            if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    Ok((master, child))
}

/// Copy stdin's current window size onto the pty master (the kernel
/// then signals the child). No-op when stdin is not a terminal.
pub fn resize_to_stdin(master: &OwnedFd) {
    if let Some(size) = stdin_winsize() {
        // SAFETY: master is a live pty fd; the struct outlives the call.
        unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    }
}

fn stdin_winsize() -> Option<libc::winsize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: out-parameter ioctl on stdin; the return value is checked.
    let rc = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut size) };
    (rc == 0 && size.ws_row > 0).then_some(size)
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: plain fcntl on a fd we own; return values are checked.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Put stdin into raw mode so every byte passes through to the child
/// untouched; the original settings return on drop. When stdin is not
/// a terminal this is a no-op.
pub struct RawModeGuard {
    saved: Option<libc::termios>,
}

#[must_use = "dropping the guard restores the terminal immediately"]
pub fn enter_raw_mode() -> RawModeGuard {
    // SAFETY: tcgetattr fills the struct or fails; both are handled.
    let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
    // SAFETY: out-parameter call on stdin; the return value is checked.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut attrs) } != 0 {
        return RawModeGuard { saved: None };
    }
    let saved = attrs;
    // SAFETY: attrs came from tcgetattr above.
    unsafe {
        libc::cfmakeraw(&mut attrs);
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &attrs);
    }
    RawModeGuard { saved: Some(saved) }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            // SAFETY: restoring settings previously read from stdin.
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
        }
    }
}

/// Block `SIGWINCH` for the whole process. Call before spawning
/// threads (they inherit the mask), then have one thread consume the
/// signal via [`wait_sigwinch`].
pub fn block_sigwinch() {
    // SAFETY: standard sigmask setup; a failure only costs resize
    // propagation, so the return values are deliberately unchecked.
    unsafe {
        let mut set = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGWINCH);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Sleep until the next `SIGWINCH`; false means waiting is broken and
/// the caller should stop its loop.
pub fn wait_sigwinch() -> bool {
    // SAFETY: sigwait on a locally built set; the result is checked.
    unsafe {
        let mut set = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGWINCH);
        let mut signal: libc::c_int = 0;
        libc::sigwait(&set, &mut signal) == 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::io::Read;

    /// The proxy blocks SIGWINCH before spawning, and the mask
    /// survives exec — the child must start with a clean mask, or ssh
    /// and tmux go deaf to resizes and remote panes render at the
    /// attach-time size forever.
    #[test]
    fn child_signal_mask_is_clean() {
        block_sigwinch();
        let argv = [
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from("grep SigBlk /proc/self/status"),
        ];
        let (master, mut child) = spawn_on_pty(&argv).unwrap();
        let mut output = String::new();
        // EIO at child exit is the pty's EOF, not a failure
        let _ = std::fs::File::from(master).read_to_string(&mut output);
        assert!(child.wait().unwrap().success());
        let mask = output.trim().strip_prefix("SigBlk:").unwrap().trim();
        let mask = u64::from_str_radix(mask, 16).unwrap();
        assert_eq!(
            mask & (1 << (libc::SIGWINCH - 1)),
            0,
            "SIGWINCH blocked in child: SigBlk={mask:016x}"
        );
    }
}
