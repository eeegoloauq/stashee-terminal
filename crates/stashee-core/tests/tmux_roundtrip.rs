//! The argv builders and parsers against a real tmux: create, inspect,
//! rename and kill a session the way the app does. Runs on a private
//! `TMUX_TMPDIR`, so a live Stashee socket is never touched; skipped
//! when tmux is not installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use stashee_core::tmux;

struct Tmux {
    dir: tempfile::TempDir,
}

impl Tmux {
    fn run(&self, argv: &[String]) -> String {
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .env("TMUX_TMPDIR", self.dir.path())
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .output()
            .expect("tmux runs");
        assert!(
            output.status.success(),
            "{argv:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn sessions(&self) -> Vec<String> {
        tmux::parse_session_list(&self.run(&tmux::list_sessions_argv()))
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", tmux::SOCKET, "kill-server"])
            .env("TMUX_TMPDIR", self.dir.path())
            .output();
    }
}

fn tmux_installed() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

#[test]
fn a_session_lives_through_create_inspect_rename_and_kill() {
    if !tmux_installed() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let tmux = Tmux {
        dir: tempfile::tempdir().unwrap(),
    };
    let dir = tempfile::tempdir().unwrap();
    let session = tmux::session_name("My Work", "ab12cd");
    assert_eq!(session, "stashee-my-work-ab12cd");

    // `true` stands in for `stashee --welcome`: it prints nothing and
    // the session falls through to `exec $SHELL`.
    tmux.run(&tmux::welcome_session_argv(
        Path::new("/dev/null"),
        &session,
        dir.path(),
        Path::new("/bin/true"),
    ));
    assert_eq!(tmux.sessions(), std::slice::from_ref(&session));

    let dirs = tmux::parse_pane_dirs(&tmux.run(&tmux::list_pane_dirs_argv()));
    let reported = dirs.get(&session).expect("pane dir reported");
    assert_eq!(
        reported.canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );

    // The close guard's question. Right after creation the pane may
    // still be running `true`; either way it names a command.
    let command = tmux.run(&tmux::current_command_argv(&session));
    assert!(!command.trim().is_empty(), "no foreground command reported");

    let renamed = tmux::session_name("side", "ab12cd");
    tmux.run(&tmux::rename_session_argv(&session, &renamed));
    assert_eq!(tmux.sessions(), std::slice::from_ref(&renamed));

    let other = tmux::session_name("side", "ef34gh");
    tmux.run(&tmux::welcome_session_argv(
        Path::new("/dev/null"),
        &other,
        dir.path(),
        Path::new("/bin/true"),
    ));
    tmux.run(&tmux::kill_session_argv(&renamed));
    assert_eq!(tmux.sessions(), [other]);
}
