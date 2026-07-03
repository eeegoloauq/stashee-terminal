//! SSH pane commands. Remote sessions use the remote's *default* tmux
//! socket on purpose: the same session stays reachable from any other
//! machine, not just from stashee.

/// argv to create-or-attach a stashed SSH pane.
///
/// Besides attaching, the sequence turns `set-clipboard` on so that
/// applications inside the remote tmux can copy via OSC 52 (the local
/// proxy picks it up — see the frontend). `\;` survives the remote
/// shell as tmux's command separator. Two constraints shape this:
/// a failed command aborts the whole sequence, so only options every
/// tmux version knows may appear; and `set` alone cannot start a
/// server, hence the explicit `start-server`.
#[must_use]
pub fn attach_remote_argv(host: &str, session: &str) -> Vec<String> {
    vec![
        "ssh".into(),
        "-t".into(),
        host.into(),
        "--".into(),
        "tmux".into(),
        "start-server".into(),
        "\\;".into(),
        "set".into(),
        "-s".into(),
        "set-clipboard".into(),
        "on".into(),
        "\\;".into(),
        "new-session".into(),
        "-A".into(),
        "-s".into(),
        session.into(),
    ]
}

/// Fallback when the remote has no tmux: a plain connection, with the
/// pane showing a "Not stashed" banner (see SPEC.md "SSH panes").
#[must_use]
pub fn plain_argv(host: &str) -> Vec<String> {
    vec!["ssh".into(), host.into()]
}

/// True when the ssh child's wait status means the remote command was
/// not found (exit code 127): the host has no tmux, and the pane
/// should fall back to a plain connection.
#[must_use]
pub fn remote_tmux_missing(wait_status: i32) -> bool {
    // raw waitpid(2) status, as GLib's child watch delivers it: low
    // 7 bits zero = exited normally, exit code in the next byte
    wait_status & 0x7f == 0 && (wait_status >> 8) & 0xff == 127
}

/// True when the ssh client's wait status means the *transport* died
/// (suspend/resume, network change, remote reboot) rather than a user
/// exit: ssh reserves exit code 255 for its own errors, and the OSC 52
/// proxy mirrors exit codes. A detach or a remote `kill-session` exits
/// 0 — those close the pane; this one reattaches it.
#[must_use]
pub fn connection_lost(wait_status: i32) -> bool {
    wait_status & 0x7f == 0 && (wait_status >> 8) & 0xff == 255
}

/// `ssh <host> -- tmux <args…>` — a one-shot remote tmux command (no
/// tty, unlike the attach).
fn remote_tmux_argv(host: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_owned(),
        host.to_owned(),
        "--".to_owned(),
        "tmux".to_owned(),
    ];
    argv.extend(args.iter().map(|&arg| arg.to_owned()));
    argv
}

/// argv to rename a remote session (workflow rename keeps sessions in
/// step, so the next app start reattaches instead of creating a
/// duplicate).
#[must_use]
pub fn rename_remote_argv(host: &str, old: &str, new: &str) -> Vec<String> {
    remote_tmux_argv(host, &["rename-session", "-t", old, new])
}

/// argv to kill a remote session (`Ctrl+W` on an SSH pane); the
/// remote session ending makes the local ssh client exit, which drives
/// the UI update — the same code path as local panes.
#[must_use]
pub fn kill_remote_argv(host: &str, session: &str) -> Vec<String> {
    remote_tmux_argv(host, &["kill-session", "-t", session])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_attach_argv_matches_spec() {
        assert_eq!(
            attach_remote_argv("e@server", "stashee-srv-abc123"),
            [
                "ssh",
                "-t",
                "e@server",
                "--",
                "tmux",
                "start-server",
                "\\;",
                "set",
                "-s",
                "set-clipboard",
                "on",
                "\\;",
                "new-session",
                "-A",
                "-s",
                "stashee-srv-abc123",
            ]
        );
    }

    #[test]
    fn plain_fallback_is_just_ssh() {
        assert_eq!(plain_argv("e@server"), ["ssh", "e@server"]);
    }

    #[test]
    fn only_a_clean_exit_127_means_tmux_is_missing() {
        assert!(remote_tmux_missing(127 << 8));
        assert!(!remote_tmux_missing(0)); // clean exit
        assert!(!remote_tmux_missing(1 << 8)); // remote command failed
        assert!(!remote_tmux_missing(255 << 8)); // ssh itself failed
        assert!(!remote_tmux_missing(9)); // killed by a signal
    }

    #[test]
    fn only_a_clean_exit_255_means_the_connection_died() {
        assert!(connection_lost(255 << 8));
        assert!(!connection_lost(0)); // detach / remote kill-session
        assert!(!connection_lost(127 << 8)); // tmux missing on host
        assert!(!connection_lost(1 << 8)); // remote command failed
        assert!(!connection_lost(9)); // killed by a signal
    }

    #[test]
    fn remote_rename_argv_matches_spec() {
        assert_eq!(
            rename_remote_argv("e@server", "stashee-srv-abc123", "stashee-prod-abc123"),
            [
                "ssh",
                "e@server",
                "--",
                "tmux",
                "rename-session",
                "-t",
                "stashee-srv-abc123",
                "stashee-prod-abc123",
            ]
        );
    }

    #[test]
    fn remote_kill_argv_matches_spec() {
        assert_eq!(
            kill_remote_argv("e@server", "stashee-srv-abc123"),
            [
                "ssh",
                "e@server",
                "--",
                "tmux",
                "kill-session",
                "-t",
                "stashee-srv-abc123",
            ]
        );
    }
}
