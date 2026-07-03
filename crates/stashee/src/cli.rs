//! The displayless command surface: `stashee list`, usage text, and
//! the tmux session probe shared with startup. Nothing here may
//! require a display or a running instance.

use std::process::Command;

use gtk4::glib;

use stashee_core::config::Config;
use stashee_core::model::Workflow;
use stashee_core::state::State;
use stashee_core::tmux;

use crate::paths;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const ACCENT: &str = "\x1b[38;2;153;193;241m";

/// The wordmark, one gradient step per line.
const LOGO: [(&str, &str); 6] = [
    (
        "\x1b[38;2;199;216;245m",
        "███████╗████████╗ █████╗ ███████╗██╗  ██╗███████╗███████╗",
    ),
    (
        "\x1b[38;2;175;199;242m",
        "██╔════╝╚══██╔══╝██╔══██╗██╔════╝██║  ██║██╔════╝██╔════╝",
    ),
    (
        "\x1b[38;2;151;182;240m",
        "███████╗   ██║   ███████║███████╗███████║█████╗  █████╗  ",
    ),
    (
        "\x1b[38;2;127;165;237m",
        "╚════██║   ██║   ██╔══██║╚════██║██╔══██║██╔══╝  ██╔══╝  ",
    ),
    (
        "\x1b[38;2;103;148;234m",
        "███████║   ██║   ██║  ██║███████║██║  ██║███████╗███████╗",
    ),
    (
        "\x1b[38;2;85;136;221m",
        "╚══════╝   ╚═╝   ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚══════╝╚══════╝",
    ),
];

const TAGLINE: &str = "a tiling terminal workspace — shells never die by accident";

const KEYS: [(&str, &str); 5] = [
    ("Ctrl+T", "new pane — the grid tiles itself"),
    (
        "Ctrl+Shift+T",
        "new SSH pane (survives reboots; plain ssh won't)",
    ),
    ("Ctrl+W", "close pane (kills its shell)"),
    ("Alt+1…9", "switch workflow"),
    ("Alt+Arrows", "move focus between panes"),
];

const COMMANDS: [(&str, &str); 4] = [
    ("stashee", "open the app on the last active workflow"),
    ("stashee <name>", "open or create a workflow from any shell"),
    ("stashee list", "print what's stashed"),
    ("stashee config", "every setting, one file, applied live"),
];

/// The keys and commands blocks shared by the first-run greeting and
/// `stashee help` — one source, so the two can never drift apart.
fn cribsheet(color: bool) -> String {
    let (accent, reset) = if color { (ACCENT, RESET) } else { ("", "") };
    let mut out = String::new();
    for (key, what) in KEYS {
        out.push_str(&format!("  {accent}{key:<16}{reset} {what}\n"));
    }
    out.push('\n');
    for (command, what) in COMMANDS {
        out.push_str(&format!("  {accent}{command:<16}{reset} {what}\n"));
    }
    out
}

/// The bare `Usage:` block for argument errors — terse on purpose;
/// `stashee help` is the readable page.
pub fn usage() -> String {
    let mut out = String::from("Usage:\n");
    for (command, what) in COMMANDS {
        out.push_str(&format!("  {command:<16} {what}\n"));
    }
    out
}

/// `stashee help` / `--help`: the greeting minus the wordmark.
/// `color` should be whether stdout is a terminal — help gets piped.
pub fn help_text(color: bool) -> String {
    format!(
        "stashee {}\n{TAGLINE}\n\n{}\n  \
         Closing the window stashes everything: each shell keeps\n  \
         running and comes back right where it was.\n",
        env!("CARGO_PKG_VERSION"),
        cribsheet(color)
    )
}

/// `stashee --welcome`: the greeting the very first launch's pane
/// prints above its shell — window.rs pre-creates that session with
/// it. Hidden from usage, like `--osc52-proxy`.
pub fn welcome() -> glib::ExitCode {
    print!("{}", greeting());
    glib::ExitCode::SUCCESS
}

fn greeting() -> String {
    let mut out = String::from("\n");
    for (color, line) in LOGO {
        out.push_str(&format!("  {color}{line}{RESET}\n"));
    }
    out.push_str(&format!("\n  {DIM}{TAGLINE}{RESET}\n\n"));
    out.push_str(&cribsheet(true));
    out.push_str(
        "\n  Closing the window stashes everything: each shell — this one\n  \
         included — keeps running and comes back right where it was.\n\n",
    );
    out
}

/// `stashee config`: open the config (created from the commented
/// template on demand) in `$VISUAL`/`$EDITOR`, or print its path when
/// neither is set — pipeline-friendly either way.
pub fn edit_config() -> glib::ExitCode {
    let path = paths::config_file();
    if let Err(err) = Config::ensure(&path) {
        eprintln!("stashee: {err:#}");
        return glib::ExitCode::FAILURE;
    }
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .filter(|editor| !editor.trim().is_empty());
    let Some(editor) = editor else {
        println!("{}", path.display());
        eprintln!("stashee: $EDITOR is not set — edit the file above by hand");
        return glib::ExitCode::SUCCESS;
    };
    // `$EDITOR` may carry arguments ("code -w"), so let a shell split
    // it; the path is passed out-of-band to survive any quoting.
    let status = Command::new("sh")
        .args(["-c", &format!("{editor} \"$1\""), "sh"])
        .arg(&path)
        .status();
    match status {
        Ok(status) if status.success() => glib::ExitCode::SUCCESS,
        Ok(_) => glib::ExitCode::FAILURE,
        Err(err) => {
            eprintln!("stashee: running {editor:?}: {err}");
            glib::ExitCode::FAILURE
        }
    }
}

/// `stashee list`: reconcile state with live sessions and print.
/// Read-only — the app saves on every change, so the state file is
/// current even while an instance is running.
pub fn list() -> glib::ExitCode {
    let mut state = match State::load(&paths::state_file()) {
        Ok(state) => state,
        Err(err) => {
            eprintln!("stashee: state is unreadable: {err:#}");
            return glib::ExitCode::FAILURE;
        }
    };
    state.reconcile(
        &live_sessions(),
        &glib::home_dir(),
        stashee_core::state::current_boot_id().as_deref(),
    );
    print!("{}", render(&state.workflows));
    glib::ExitCode::SUCCESS
}

fn render(workflows: &[Workflow]) -> String {
    if workflows.is_empty() {
        return "no workflows\n".to_owned();
    }
    let width = workflows
        .iter()
        .map(|wf| wf.name.chars().count())
        .max()
        .unwrap_or(0);
    workflows
        .iter()
        .map(|wf| {
            let panes = wf.panes.len();
            let noun = if panes == 1 { "pane" } else { "panes" };
            format!("{:<width$}  {panes} {noun}\n", wf.name)
        })
        .collect()
}

/// Live sessions on our socket; an unreachable or not-yet-started tmux
/// server simply means nothing is stashed.
pub fn live_sessions() -> Vec<String> {
    let argv = tmux::list_sessions_argv();
    match Command::new(&argv[0]).args(&argv[1..]).output() {
        Ok(out) if out.status.success() => {
            tmux::parse_session_list(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stashee_core::model::PaneSpec;

    fn workflow(name: &str, panes: usize) -> Workflow {
        let mut wf = Workflow::new(name, "/home/e");
        for _ in 0..panes {
            wf.panes.push(PaneSpec::new_local());
        }
        wf
    }

    #[test]
    fn render_aligns_names_and_declines_pane() {
        let list = [workflow("work", 3), workflow("s", 1)];
        assert_eq!(render(&list), "work  3 panes\ns     1 pane\n");
    }

    #[test]
    fn render_reports_the_empty_list() {
        assert_eq!(render(&[]), "no workflows\n");
    }

    #[test]
    fn greeting_covers_the_whole_cribsheet() {
        let text = greeting();
        for (key, what) in KEYS.iter().chain(&COMMANDS) {
            assert!(text.contains(key), "missing key {key:?}");
            assert!(text.contains(what), "missing description {what:?}");
        }
    }

    #[test]
    fn help_text_is_the_greeting_minus_the_wordmark() {
        let text = help_text(false);
        for (key, what) in KEYS.iter().chain(&COMMANDS) {
            assert!(text.contains(key), "missing key {key:?}");
            assert!(text.contains(what), "missing description {what:?}");
        }
        assert!(!text.contains('\x1b'), "plain help must carry no ANSI");
        assert!(!text.contains('█'), "help must not include the wordmark");
    }

    #[test]
    fn usage_lists_every_command_without_color() {
        let text = usage();
        for (command, _) in COMMANDS {
            assert!(text.contains(command), "missing command {command:?}");
        }
        assert!(!text.contains('\x1b'));
    }
}
