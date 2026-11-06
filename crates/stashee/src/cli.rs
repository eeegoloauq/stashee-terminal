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
const BOLD: &str = "\x1b[1m";

const TAGLINE: &str = "a Linux terminal that keeps your shells running after you close it";

const KEYS: [(&str, &str); 6] = [
    ("Ctrl+T", "new pane — the grid tiles itself"),
    (
        "Ctrl+Shift+T",
        "new SSH pane (survives reboots; plain ssh won't)",
    ),
    (
        "Ctrl+Shift+W",
        "close pane (asks first if something is running)",
    ),
    ("Alt+1…9", "switch workflow"),
    ("Alt+Arrows", "move focus between panes"),
    (
        "Ctrl+Shift+V",
        "paste — a copied image lands as a file path (scp'd to SSH hosts)",
    ),
];

const COMMANDS: [(&str, &str); 4] = [
    ("stashee", "open the app on the last active workflow"),
    ("stashee <name>", "open or create a workflow from any shell"),
    ("stashee list", "print what's stashed"),
    ("stashee config", "every setting, one file, applied live"),
];

/// The keys and commands blocks of `stashee help`.
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

/// `stashee help` / `--help`: every key and command.
/// `color` should be whether stdout is a terminal — help gets piped.
pub fn help_text(color: bool) -> String {
    format!(
        "stashee {}\n{TAGLINE}\n\n{}\n  \
         Closing the window keeps every shell running; reopening\n  \
         brings them back where they were.\n",
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

/// Short on purpose: one promise the user can test right away, three
/// keys, and the way to the rest. Every line fits a pane in a
/// three-column split (the greeting stays in scrollback at any width).
fn greeting() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "\n  {BOLD}{ACCENT}stashee{RESET} {DIM}{version}{RESET}\n\n  \
         Close this window. This shell keeps\n  \
         running and will be here when you reopen.\n\n  \
         {ACCENT}Ctrl+T{RESET} new pane   {ACCENT}Ctrl+Shift+W{RESET} close\n  \
         {ACCENT}Alt+1…9{RESET} workflows {DIM}· stashee help{RESET}\n\n"
    )
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
    if let Err(err) = Config::ensure_reference(&paths::config_reference()) {
        eprintln!("stashee: {err:#}"); // reference only — still editable
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

    /// Visible width: ANSI escapes stripped, characters counted.
    fn visible_width(line: &str) -> usize {
        let mut width = 0;
        let mut in_escape = false;
        for ch in line.chars() {
            match (in_escape, ch) {
                (false, '\x1b') => in_escape = true,
                (true, 'm') => in_escape = false,
                (true, _) => {}
                (false, _) => width += 1,
            }
        }
        width
    }

    #[test]
    fn greeting_fits_a_three_column_split() {
        let text = greeting();
        assert!(text.lines().count() <= 10, "greeting grew: {text}");
        for line in text.lines() {
            assert!(visible_width(line) <= 44, "too wide: {line:?}");
        }
        assert!(text.contains("stashee help"));
        assert!(text.contains("Close this window"));
    }

    #[test]
    fn help_text_lists_every_key_and_command_without_color() {
        let text = help_text(false);
        for (key, what) in KEYS.iter().chain(&COMMANDS) {
            assert!(text.contains(key), "missing key {key:?}");
            assert!(text.contains(what), "missing description {what:?}");
        }
        assert!(!text.contains('\x1b'), "plain help must carry no ANSI");
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
