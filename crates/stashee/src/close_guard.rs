//! Ask before closing a pane that is running something. Closing kills
//! the pane's session, and a killed program cannot come back — so the
//! close key acts at once only on an idle shell. The probes block
//! (tmux, ssh, /proc); window.rs runs them off the main thread.

use std::process::{Command, Stdio};

/// How to see what a pane is running.
pub enum Probe {
    /// argv printing the foreground command of the pane's tmux
    /// session, local or over ssh.
    Tmux(Vec<String>),
    /// A plain-shell pane: the pid VTE spawned — the OSC 52 proxy, or
    /// the bare shell when the proxy could not wrap it.
    Process(i32),
    /// Nothing to ask: a pane mid-reconnect, or a plain ssh connection
    /// whose remote side is invisible.
    Blind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    /// The foreground command's name.
    Busy(String),
    Unknown,
}

/// Interactive shells: one of these in the foreground means an idle
/// prompt.
const SHELLS: [&str; 15] = [
    "bash", "zsh", "fish", "sh", "dash", "ash", "ksh", "mksh", "yash", "tcsh", "csh", "nu",
    "elvish", "xonsh", "osh",
];

/// Blocking.
pub fn probe(probe: Probe) -> Activity {
    match probe {
        Probe::Tmux(argv) => {
            let output = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    from_command(&String::from_utf8_lossy(&output.stdout))
                }
                _ => Activity::Unknown,
            }
        }
        Probe::Process(pid) => process_activity(pid),
        Probe::Blind => Activity::Unknown,
    }
}

fn from_command(command: &str) -> Activity {
    let command = command.trim();
    if command.is_empty() {
        Activity::Unknown
    } else if is_shell(command) {
        Activity::Idle
    } else {
        Activity::Busy(command.to_owned())
    }
}

/// `-bash` (a login shell) and `/usr/bin/zsh` count too.
fn is_shell(command: &str) -> bool {
    let name = command.trim().trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    SHELLS.contains(&name)
}

/// A shell is idle when its terminal's foreground process group is
/// its own: a running program takes the foreground.
fn process_activity(pid: i32) -> Activity {
    let shell = if comm(pid).is_some_and(|name| is_shell(&name)) {
        pid
    } else {
        match first_child(pid) {
            Some(child) => child,
            None => return Activity::Unknown,
        }
    };
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{shell}/stat")) else {
        return Activity::Unknown;
    };
    match foreground_group(&stat) {
        Some(group) if group == shell => Activity::Idle,
        Some(group) if group > 0 => {
            Activity::Busy(comm(group).unwrap_or_else(|| "a program".to_owned()))
        }
        _ => Activity::Unknown,
    }
}

fn comm(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|name| name.trim().to_owned())
}

fn first_child(pid: i32) -> Option<i32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `tpgid` from a `/proc/<pid>/stat` line. The command name sits in
/// parentheses and may itself contain spaces or `)`, so fields are
/// counted from the last `)`: state, ppid, pgrp, session, tty, tpgid.
fn foreground_group(stat: &str) -> Option<i32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(5)?.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn shells_are_idle_and_programs_are_busy() {
        assert_eq!(from_command("bash\n"), Activity::Idle);
        assert_eq!(from_command("-zsh"), Activity::Idle);
        assert_eq!(from_command("/usr/bin/fish"), Activity::Idle);
        assert_eq!(from_command("vim\n"), Activity::Busy("vim".to_owned()));
        assert_eq!(from_command("claude"), Activity::Busy("claude".to_owned()));
        assert_eq!(from_command(""), Activity::Unknown);
    }

    #[test]
    fn foreground_group_survives_odd_command_names() {
        let stat = "4242 (my) prog) S 4200 4242 4242 34817 5151 4194560 0 0";
        assert_eq!(foreground_group(stat), Some(5151));
        assert_eq!(foreground_group("garbage"), None);
    }

    #[test]
    fn this_process_reads_back_through_proc() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        assert!(foreground_group(&stat).is_some());
        assert!(comm(pid).is_some());
    }
}
