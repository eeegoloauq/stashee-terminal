//! Everything the app says to tmux, in one place: pure functions that
//! build argv vectors or parse output. The frontend does the spawning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Dedicated socket name — keeps our sessions out of the user's own
/// tmux server, and `tmux -L stashee ls` cleanly lists what we own.
pub const SOCKET: &str = "stashee";

/// Every session we own starts with this.
pub const SESSION_PREFIX: &str = "stashee-";

/// Workflow name → session-name slug: lowercased, `[a-z0-9]` kept,
/// runs of anything else collapsed to a single `-`. tmux forbids `.`
/// and `:` in session names; we are stricter so names stay readable.
#[must_use]
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            if lower.is_ascii_alphanumeric() {
                out.push(lower);
            } else if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "workflow".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `("My Work", "abc123")` → `stashee-my-work-abc123`.
#[must_use]
pub fn session_name(workflow_name: &str, pane_id: &str) -> String {
    format!("{SESSION_PREFIX}{}-{pane_id}", sanitize(workflow_name))
}

/// Inverse of [`session_name`]:
/// `stashee-my-work-abc123` → `("my-work", "abc123")`.
/// `None` for sessions that are not ours.
#[must_use]
pub fn parse_session_name(session: &str) -> Option<(&str, &str)> {
    let rest = session.strip_prefix(SESSION_PREFIX)?;
    let (slug, id) = rest.rsplit_once('-')?;
    if slug.is_empty() || id.is_empty() {
        return None;
    }
    Some((slug, id))
}

/// `tmux -L stashee <args…>` — every command against our socket
/// starts the same way.
fn socket_argv(args: &[&str]) -> Vec<String> {
    let mut argv = vec!["tmux".to_owned(), "-L".to_owned(), SOCKET.to_owned()];
    argv.extend(args.iter().map(|&arg| arg.to_owned()));
    argv
}

/// argv to create-or-attach a stashed local pane.
///
/// `set-clipboard on` is in the bundled conf, but server options are
/// read once at server start — a server left running by an older build
/// predates the setting. Inject it at attach, exactly like the SSH
/// attach does (`;` is tmux's command separator; no shell involved
/// here, so it needs no escaping). `set` alone cannot start a server,
/// hence the explicit `start-server`.
///
/// `run` (a workflow-template command) rides on `new-session`, which
/// executes it only when the call actually creates the session —
/// reattaching ignores it, so the command can never fire twice.
#[must_use]
pub fn attach_local_argv(
    tmux_conf: &Path,
    session: &str,
    dir: &Path,
    run: Option<&str>,
) -> Vec<String> {
    let mut argv = socket_argv(&[
        "-f",
        &tmux_conf.display().to_string(),
        "start-server",
        ";",
        "set",
        "-s",
        "set-clipboard",
        "on",
        ";",
        "new-session",
        "-A",
        "-s",
        session,
        "-c",
        &dir.display().to_string(),
    ]);
    if let Some(run) = run {
        argv.push(run_then_shell(run));
    }
    argv
}

/// `<command>; exec $SHELL` — a session's startup command that leaves
/// a shell behind when it exits, so the pane never dies with it. tmux
/// hands session commands to `sh -c`, which resolves the fallback.
#[must_use]
pub fn run_then_shell(command: &str) -> String {
    format!("{command}; exec \"${{SHELL:-/bin/sh}}\"")
}

/// argv to pre-create the very first launch's pane session, detached:
/// it prints the welcome greeting (`stashee --welcome`), then execs the
/// user's shell. The pane itself attaches with [`attach_local_argv`] —
/// `new-session -A` ignores the command on attach, so the greeting can
/// never reappear on a reattach.
#[must_use]
pub fn welcome_session_argv(
    tmux_conf: &Path,
    session: &str,
    dir: &Path,
    exe: &Path,
) -> Vec<String> {
    let command = run_then_shell(&format!(
        "{} --welcome",
        shell_quote(&exe.display().to_string())
    ));
    socket_argv(&[
        "-f",
        &tmux_conf.display().to_string(),
        "new-session",
        "-d",
        "-s",
        session,
        "-c",
        &dir.display().to_string(),
        &command,
    ])
}

/// Single-quote `text` for sh — tmux hands the session command to
/// `sh -c`, and the executable path is not ours to trust. The SSH
/// module reuses it to keep template arguments whole across the
/// remote shell.
pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// argv to kill one session (used by `Ctrl+W`; the resulting client
/// exit drives the UI update — one code path).
#[must_use]
pub fn kill_session_argv(session: &str) -> Vec<String> {
    socket_argv(&["kill-session", "-t", session])
}

/// argv to rename a session (workflow rename keeps sessions in step).
#[must_use]
pub fn rename_session_argv(old: &str, new: &str) -> Vec<String> {
    socket_argv(&["rename-session", "-t", old, new])
}

/// argv to list live session names on our socket, one per line.
#[must_use]
pub fn list_sessions_argv() -> Vec<String> {
    socket_argv(&["list-sessions", "-F", "#{session_name}"])
}

/// argv to list every pane's working directory. Tab-separated because
/// paths contain spaces; the activity flags disambiguate hand-made
/// splits (see [`parse_pane_dirs`]).
#[must_use]
pub fn list_pane_dirs_argv() -> Vec<String> {
    socket_argv(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{window_active}#{pane_active}\t#{pane_current_path}",
    ])
}

/// Working directory per session from [`list_pane_dirs_argv`] output.
/// A session normally holds one pane, but power users may split it by
/// hand (`Ctrl+B` passes through) — then the active pane's directory
/// wins. Foreign sessions and malformed lines are ignored.
#[must_use]
pub fn parse_pane_dirs(output: &str) -> HashMap<String, PathBuf> {
    let mut dirs = HashMap::new();
    for line in output.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(session), Some(active), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !session.starts_with(SESSION_PREFIX) || path.is_empty() {
            continue;
        }
        if active == "11" {
            dirs.insert(session.to_owned(), PathBuf::from(path));
        } else {
            dirs.entry(session.to_owned())
                .or_insert_with(|| PathBuf::from(path));
        }
    }
    dirs
}

/// Our sessions from [`list_sessions_argv`] output; foreign lines are
/// ignored (power users may create sessions on our socket by hand).
#[must_use]
pub fn parse_session_list(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(SESSION_PREFIX))
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_simple_names() {
        assert_eq!(sanitize("work"), "work");
        assert_eq!(sanitize("Work"), "work");
        assert_eq!(sanitize("web3"), "web3");
    }

    #[test]
    fn sanitize_collapses_junk_to_single_dashes() {
        assert_eq!(sanitize("My Work!"), "my-work");
        assert_eq!(sanitize("a__b"), "a-b");
        assert_eq!(sanitize("  spaced  out  "), "spaced-out");
    }

    #[test]
    fn sanitize_falls_back_when_nothing_survives() {
        assert_eq!(sanitize(""), "workflow");
        assert_eq!(sanitize("///"), "workflow");
        assert_eq!(sanitize("мой"), "workflow");
    }

    #[test]
    fn session_names_round_trip() {
        let session = session_name("My Work", "abc123");
        assert_eq!(session, "stashee-my-work-abc123");
        assert_eq!(parse_session_name(&session), Some(("my-work", "abc123")));
    }

    #[test]
    fn foreign_sessions_do_not_parse() {
        assert_eq!(parse_session_name("main"), None);
        assert_eq!(parse_session_name("stashee-"), None);
        assert_eq!(parse_session_name("stashee-noid"), None);
    }

    #[test]
    fn attach_argv_matches_spec() {
        let argv = attach_local_argv(
            Path::new("/usr/share/stashee/tmux.conf"),
            "stashee-work-abc123",
            Path::new("/home/e/dev"),
            None,
        );
        assert_eq!(
            argv,
            [
                "tmux",
                "-L",
                "stashee",
                "-f",
                "/usr/share/stashee/tmux.conf",
                "start-server",
                ";",
                "set",
                "-s",
                "set-clipboard",
                "on",
                ";",
                "new-session",
                "-A",
                "-s",
                "stashee-work-abc123",
                "-c",
                "/home/e/dev",
            ]
        );
    }

    #[test]
    fn a_template_run_rides_on_new_session_and_keeps_the_shell() {
        let argv = attach_local_argv(
            Path::new("/data/tmux.conf"),
            "stashee-work-abc123",
            Path::new("/home/e/dev"),
            Some("claude"),
        );
        assert_eq!(
            argv.last().map(String::as_str),
            Some("claude; exec \"${SHELL:-/bin/sh}\"")
        );
    }

    #[test]
    fn welcome_argv_runs_detached_and_quotes_the_exe() {
        let argv = welcome_session_argv(
            Path::new("/data/tmux.conf"),
            "stashee-welcome-abc123",
            Path::new("/home/e"),
            Path::new("/home/e/my bin/stashee"),
        );
        assert_eq!(
            argv,
            [
                "tmux",
                "-L",
                "stashee",
                "-f",
                "/data/tmux.conf",
                "new-session",
                "-d",
                "-s",
                "stashee-welcome-abc123",
                "-c",
                "/home/e",
                "'/home/e/my bin/stashee' --welcome; exec \"${SHELL:-/bin/sh}\"",
            ]
        );
    }

    #[test]
    fn shell_quote_survives_embedded_quotes() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    #[test]
    fn session_list_parsing_skips_foreign_lines() {
        let output = "stashee-work-abc123\nmain\n\nstashee-srv-x1y2z3\n";
        assert_eq!(
            parse_session_list(output),
            ["stashee-work-abc123", "stashee-srv-x1y2z3"]
        );
    }

    #[test]
    fn pane_dirs_argv_matches_spec() {
        assert_eq!(
            list_pane_dirs_argv(),
            [
                "tmux",
                "-L",
                "stashee",
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{window_active}#{pane_active}\t#{pane_current_path}",
            ]
        );
    }

    #[test]
    fn pane_dirs_survive_spaces_and_skip_foreign_sessions() {
        let output = "stashee-work-abc123\t11\t/home/e/my dir\nmain\t11\t/root\n";
        let dirs = parse_pane_dirs(output);
        assert_eq!(dirs.len(), 1);
        assert_eq!(
            dirs.get("stashee-work-abc123"),
            Some(&PathBuf::from("/home/e/my dir"))
        );
    }

    #[test]
    fn the_active_pane_wins_a_hand_split_session() {
        // active pane last, then active pane first: both orders
        let last = "stashee-work-abc123\t10\t/tmp\nstashee-work-abc123\t11\t/home/e\n";
        assert_eq!(
            parse_pane_dirs(last).get("stashee-work-abc123"),
            Some(&PathBuf::from("/home/e"))
        );
        let first = "stashee-work-abc123\t11\t/home/e\nstashee-work-abc123\t10\t/tmp\n";
        assert_eq!(
            parse_pane_dirs(first).get("stashee-work-abc123"),
            Some(&PathBuf::from("/home/e"))
        );
    }

    #[test]
    fn malformed_pane_dir_lines_are_ignored() {
        let output = "stashee-work-abc123\t11\nno tabs at all\nstashee-x-y\t11\t\n";
        assert!(parse_pane_dirs(output).is_empty());
    }
}
