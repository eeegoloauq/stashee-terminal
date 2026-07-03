//! `stashee --osc52-proxy <command…>` — the hidden pane-side mode
//! behind SSH clipboard support. It runs the command on its own pty
//! and relays bytes untouched, lifting OSC 52 clipboard writes (which
//! VTE silently drops) out to the app's clipboard socket (see
//! clipboard.rs). This is what makes "select in a remote tmux, paste
//! locally" work — see SPEC.md "SSH panes". Runs before GTK: no
//! display, no GApplication, no single-instance.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitStatus;
use std::time::Duration;

use stashee_core::osc52;

use crate::paths;

pub const FLAG: &str = "--osc52-proxy";

/// Prefix `cmd` so it runs under the proxy. If our own executable
/// cannot be resolved, the bare command runs instead — the pane still
/// works, it merely cannot copy to the local clipboard.
pub fn wrap(cmd: Vec<String>) -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned));
    match exe {
        Some(exe) => {
            let mut argv = vec![exe, FLAG.to_owned()];
            argv.extend(cmd);
            argv
        }
        None => {
            tracing::warn!("cannot resolve own executable; pane loses OSC 52 clipboard");
            cmd
        }
    }
}

/// The relay. Blocks until the wrapped command exits and mirrors its
/// exit code — the SSH no-tmux fallback depends on seeing 127.
pub fn run(cmd: &[OsString]) -> i32 {
    if cmd.is_empty() {
        eprintln!("stashee: {FLAG} needs a command to run");
        return 2;
    }
    stashee_pty::block_sigwinch();
    let (master, mut child) = match stashee_pty::spawn_on_pty(cmd) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("stashee: {}: {err}", cmd[0].display());
            return 1;
        }
    };
    let (keys_fd, resize_fd) = match (master.try_clone(), master.try_clone()) {
        (Ok(keys), Ok(resize)) => (keys, resize),
        _ => {
            eprintln!("stashee: cannot clone pty fd");
            return 1;
        }
    };
    let raw_mode = stashee_pty::enter_raw_mode();

    // Keys: pane → child. Exits with the process; a stuck read here
    // holds nothing the child's exit does not release.
    std::thread::spawn(move || {
        let mut keys = File::from(keys_fd);
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if keys.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });

    // Resizes: VTE resizes our pty, the kernel sends SIGWINCH, we
    // mirror the new size onto the child's pty.
    std::thread::spawn(move || {
        while stashee_pty::wait_sigwinch() {
            stashee_pty::resize_to_stdin(&resize_fd);
        }
    });

    // Output: child → pane, watched for OSC 52 copies.
    let mut output = File::from(master);
    let mut stdout = io::stdout().lock();
    let mut scanner = osc52::Scanner::default();
    let mut buf = [0u8; 8192];
    loop {
        match output.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stdout
                    .write_all(&buf[..n])
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
                for text in scanner.feed(&buf[..n]) {
                    copy_to_clipboard(text);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            // EIO: the child hung up its side of the pty
            Err(_) => break,
        }
    }
    drop(raw_mode);
    match child.wait() {
        Ok(status) => exit_code(status),
        Err(_) => 1,
    }
}

/// Hand the copy to the app over its clipboard socket; the app sets
/// the clipboard through GDK — it owns a window, so no focus games.
/// A dedicated thread with a write timeout: the relay loop above must
/// never wait on the clipboard, whatever the other side is doing.
/// Failures are deliberately silent here (our stderr *is* the user's
/// terminal); the app side logs its own.
fn copy_to_clipboard(text: Vec<u8>) {
    std::thread::spawn(move || {
        let Ok(mut stream) = UnixStream::connect(paths::clipboard_socket()) else {
            return;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = stream.write_all(&text);
    });
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
