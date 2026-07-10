//! SSH pane commands. Remote sessions use the remote's *default* tmux
//! socket on purpose: the same session stays reachable from any other
//! machine, not just from stashee.

/// Transport keepalive for pane connections. Without it, an idle ssh
/// on a half-open TCP link — the normal aftermath of suspend/resume —
/// blocks in read forever: no exit status, no reconnect, a frozen
/// pane. With it, ssh itself notices a dead transport within ~30 s
/// and exits 255, which is exactly what [`connection_lost`] feeds on.
/// `ConnectTimeout` bounds reconnect attempts made while the network
/// is still coming back up. Command-line `-o` outranks `~/.ssh/config`
/// deliberately: pane reconnection depends on these being in force.
const PANE_OPTS: [&str; 6] = [
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ConnectTimeout=10",
];

/// One-shot commands (kill/rename) run headless — no tty to answer a
/// prompt with — so fail fast instead of hanging on a dead network or
/// an interactive auth request.
const ONESHOT_OPTS: [&str; 4] = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

fn with_opts(opts: &[&str], rest: &[&str]) -> Vec<String> {
    let mut argv = vec!["ssh".to_owned()];
    argv.extend(opts.iter().map(|&opt| opt.to_owned()));
    argv.extend(rest.iter().map(|&arg| arg.to_owned()));
    argv
}

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
    with_opts(
        &PANE_OPTS,
        &[
            "-t",
            host,
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
            session,
        ],
    )
}

/// Fallback when the remote has no tmux: a plain connection, with the
/// pane showing a "Not stashed" banner (see SPEC.md "SSH panes").
#[must_use]
pub fn plain_argv(host: &str) -> Vec<String> {
    with_opts(&PANE_OPTS, &[host])
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

/// True when the ssh child was killed rather than exiting on its own:
/// a signal death, or an exit code of 128+sig (the OSC 52 proxy maps
/// its child's signal death to that). This is session teardown — a
/// GNOME logout or system shutdown SIGTERMs pane children while the
/// app is still alive — not a user exit, so the pane must survive in
/// state; callers treat it like a lost connection.
#[must_use]
pub fn killed(wait_status: i32) -> bool {
    wait_status & 0x7f != 0 || (wait_status >> 8) & 0xff >= 128
}

/// `ssh <host> -- tmux <args…>` — a one-shot remote tmux command (no
/// tty, unlike the attach).
fn remote_tmux_argv(host: &str, args: &[&str]) -> Vec<String> {
    let mut argv = with_opts(&ONESHOT_OPTS, &[host, "--", "tmux"]);
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
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
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
    fn plain_fallback_keeps_the_keepalive() {
        assert_eq!(
            plain_argv("e@server"),
            [
                "ssh",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "e@server",
            ]
        );
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
    fn only_a_violent_death_counts_as_killed() {
        assert!(killed(15)); // SIGTERM (logout/shutdown)
        assert!(killed(9)); // SIGKILL
        assert!(killed(143 << 8)); // proxy's 128+SIGTERM mapping
        assert!(!killed(0)); // detach / user exit
        assert!(!killed(1 << 8)); // remote command failed
        assert!(!killed(127 << 8)); // tmux missing on host
    }

    #[test]
    fn remote_rename_argv_matches_spec() {
        assert_eq!(
            rename_remote_argv("e@server", "stashee-srv-abc123", "stashee-prod-abc123"),
            [
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
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
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
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
