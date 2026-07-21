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
///
/// `cwd` and `run` come from workflow templates and ride on
/// `new-session`, which applies them only when the call creates the
/// session — a reattach ignores both, so `run` can never fire twice.
/// Both are quoted to survive the remote shell as single words; `cwd`
/// is the one option here younger than the rest (`new-session -c`,
/// tmux 1.9, 2013) and is only emitted when a template asks for it.
#[must_use]
pub fn attach_remote_argv(
    host: &str,
    session: &str,
    cwd: Option<&str>,
    run: Option<&str>,
) -> Vec<String> {
    let mut argv = with_opts(
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
    );
    if let Some(cwd) = cwd {
        argv.push("-c".to_owned());
        argv.push(remote_path(cwd));
    }
    if let Some(run) = run {
        argv.push(crate::tmux::shell_quote(&crate::tmux::run_then_shell(run)));
    }
    argv
}

/// Quote a template path for the remote shell. A leading `~` becomes
/// `$HOME` *outside* the quotes — the remote home is unknowable here,
/// and quoting would keep tilde literal; everything else is
/// single-quoted so spaces survive the ssh join.
fn remote_path(path: &str) -> String {
    if path == "~" {
        return "\"$HOME\"".to_owned();
    }
    match path.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"{}", crate::tmux::shell_quote(&format!("/{rest}"))),
        None => crate::tmux::shell_quote(path),
    }
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

/// argv to copy a local file onto `host`, for a file drop / image
/// paste into one of its panes (the frontend's dnd module). Same
/// fail-fast stance as [`ONESHOT_OPTS`]: scp runs headless, so
/// BatchMode turns a would-be password prompt into a clean error —
/// key or agent auth is the contract (a user-side ControlMaster in
/// `~/.ssh/config` is honored automatically, but never injected here:
/// the pane transport's reconnect semantics stay untouched).
#[must_use]
pub fn upload_argv(host: &str, local: &std::path::Path, remote: &str) -> Vec<String> {
    let mut argv = vec!["scp".to_owned(), "-q".to_owned()];
    argv.extend(ONESHOT_OPTS.iter().map(|&opt| opt.to_owned()));
    argv.push("--".to_owned());
    argv.push(local.display().to_string());
    argv.push(format!("{host}:{remote}"));
    argv
}

/// A remote filename for an upload: ASCII alphanumerics and `.-_`
/// survive, everything else (spaces, quotes, non-ASCII) becomes `_`.
/// The typed remote path must never need quoting at whatever prompt
/// ends up receiving it — a shell, or a CLI reading its own input.
#[must_use]
pub fn remote_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned
    }
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
            attach_remote_argv("e@server", "stashee-srv-abc123", None, None),
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
    fn template_cwd_and_run_ride_on_new_session_quoted() {
        let argv = attach_remote_argv(
            "dev",
            "stashee-myproj-abc123",
            Some("/opt/my proj"),
            Some("claude"),
        );
        let tail: Vec<&str> = argv
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "stashee-myproj-abc123",
                "-c",
                "'/opt/my proj'",
                "'claude; exec \"${SHELL:-/bin/sh}\"'",
            ]
        );
    }

    #[test]
    fn remote_tilde_paths_lean_on_the_remote_home() {
        assert_eq!(remote_path("~"), "\"$HOME\"");
        assert_eq!(remote_path("~/proj"), "\"$HOME\"'/proj'");
        assert_eq!(remote_path("/opt/x"), "'/opt/x'");
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
    fn upload_argv_matches_spec() {
        assert_eq!(
            upload_argv(
                "e@server",
                std::path::Path::new("/run/user/1000/stashee/paste/paste-1a2b-1.png"),
                "/tmp/stashee-1a2b-1-paste.png",
            ),
            [
                "scp",
                "-q",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                "/run/user/1000/stashee/paste/paste-1a2b-1.png",
                "e@server:/tmp/stashee-1a2b-1-paste.png",
            ]
        );
    }

    #[test]
    fn remote_file_names_never_need_quoting() {
        assert_eq!(
            remote_file_name("Screenshot from 2026-07-21 12-00-00.png"),
            "Screenshot_from_2026-07-21_12-00-00.png"
        );
        assert_eq!(remote_file_name("скрин.png"), "_____.png");
        assert_eq!(remote_file_name("a'b\"c$d.txt"), "a_b_c_d.txt");
        assert_eq!(remote_file_name(""), "file");
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
