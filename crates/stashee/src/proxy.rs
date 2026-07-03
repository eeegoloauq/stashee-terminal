//! `stashee --osc52-proxy <command…>` — the hidden pane-side mode
//! behind SSH clipboard support. It runs the command on its own pty
//! and relays bytes untouched, lifting OSC 52 clipboard writes (which
//! VTE silently drops) out to `wl-copy`. This is what makes "select
//! in a remote tmux, paste locally" work — see SPEC.md "SSH panes".
//! Runs before GTK: no display, no GApplication, no single-instance.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus, Stdio};

use stashee_core::osc52;

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
                    copy_to_clipboard(&text);
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

/// Copy failures are deliberately silent: our stderr *is* the user's
/// terminal, and a copy that cannot land has nowhere better to report.
fn copy_to_clipboard(text: &[u8]) {
    let argv: &[&str] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &["wl-copy"]
    } else {
        &["xclip", "-selection", "clipboard", "-in"]
    };
    let child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text);
    }
    // wl-copy forks off a clipboard server and returns immediately
    let _ = child.wait();
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
