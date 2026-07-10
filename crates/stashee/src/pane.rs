//! One terminal pane: a VTE widget attached to its tmux session (or a
//! plain shell for non-stashed workflows).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use vte4::prelude::*;

use stashee_core::config::Config;
use stashee_core::model::{PaneKind, PaneSpec, Workflow};
use stashee_core::{ssh, tmux};

use crate::proxy;

/// GNOME-flavored 16-color palette; exact values are an open question
/// in SPEC.md and live only here.
const PALETTE: [&str; 16] = [
    "#242832", "#f66151", "#8ff0a4", "#f9f06b", "#62a0ea", "#dc8add", "#93ddc2", "#deddda",
    "#5e6272", "#ff7b63", "#b5f5c0", "#f8e45c", "#99c1f1", "#e8a6ec", "#b0e8d4", "#f6f5f4",
];
const FOREGROUND: &str = "#e2e4ea";

const NO_TMUX_BANNER: &str = "Not stashed — tmux not found on host";
const RECONNECT_BANNER: &str = "Connection lost — reconnecting…";
/// A reattached client outliving this window means the link is back.
const RECONNECT_SETTLED: Duration = Duration::from_secs(10);

pub struct Pane {
    pub id: String,
    pub root: gtk::Widget,
    pub terminal: vte4::Terminal,
    /// Whether this pane runs inside tmux — fixed at build time
    /// (`Workflow::pane_stashed`), so flipping the workflow's stash
    /// toggle never changes how an existing pane is closed.
    pub stashed: bool,
    /// Set once an SSH pane has fallen back to a plain connection (no
    /// tmux on the host): there is no remote session to kill or
    /// reattach — the pane is effectively not stashed.
    pub ssh_fallback: Rc<Cell<bool>>,
    /// Set while an SSH pane's transport is down and a reattach is
    /// pending: there is no live client, so `Ctrl+W` cannot rely on a
    /// client exit to drive the removal.
    pub reconnecting: Rc<Cell<bool>>,
    /// pid of the pane's direct child (the OSC 52 proxy, or the bare
    /// command when the proxy could not wrap); `None` between an exit
    /// and the next spawn. The post-resume transport probe
    /// (window.rs) sends it SIGWINCH.
    pub child_pid: Rc<Cell<Option<gtk::glib::Pid>>>,
    /// Only SSH panes have a transport worth probing after resume.
    pub is_ssh: bool,
}

/// Shared by all panes: `on_exited` fires when the pane's process ends
/// (tmux client exit = session ended — one code path, see
/// ARCHITECTURE.md); `on_focus` tracks which pane `Ctrl+W` acts on.
pub struct Callbacks {
    pub on_exited: Rc<dyn Fn(&str)>,
    pub on_focus: Rc<dyn Fn(&str)>,
    /// OSC 7: the pane's shell reported a new working directory.
    pub on_dir_changed: OnDirChanged,
}

pub type OnDirChanged = Rc<dyn Fn(&str, PathBuf)>;

pub fn build(
    spec: &PaneSpec,
    workflow: &Workflow,
    stashed: bool,
    config: &Config,
    tmux_conf: &Path,
    callbacks: &Callbacks,
) -> Pane {
    let terminal = vte4::Terminal::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    style(&terminal, config);

    let focus = gtk::EventControllerFocus::new();
    let id = spec.id.clone();
    let on_focus = callbacks.on_focus.clone();
    focus.connect_enter(move |_| on_focus(&id));
    terminal.add_controller(focus);

    // Right-click pastes the clipboard. Capture phase, claimed: VTE
    // must not forward button 3 to the application in the pane (tmux
    // mouse reporting would swallow it).
    let paste = gtk::GestureClick::new();
    paste.set_button(gtk::gdk::BUTTON_SECONDARY);
    paste.set_propagation_phase(gtk::PropagationPhase::Capture);
    paste.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(terminal) = gesture.widget().and_downcast::<vte4::Terminal>() {
            terminal.grab_focus();
            terminal.paste_clipboard();
        }
    });
    terminal.add_controller(paste);

    let session = tmux::session_name(&workflow.name, &spec.id);
    // A remembered directory may have been deleted since the last run;
    // spawning there would fail and silently drop the pane.
    let dir = spec
        .last_dir
        .clone()
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| workflow.default_dir.clone());
    let working_dir = dir.display().to_string();

    let banner = adw::Banner::new(NO_TMUX_BANNER);
    let ssh_fallback = Rc::new(Cell::new(false));
    let reconnecting = Rc::new(Cell::new(false));
    let child_pid = Rc::new(Cell::new(None));

    let id = spec.id.clone();
    let on_exited = callbacks.on_exited.clone();
    match &spec.kind {
        // A host without tmux makes the wrapper exit 127; the pane
        // falls back to a plain connection instead of disappearing
        // (SPEC.md "SSH panes").
        PaneKind::Ssh { host } => {
            let host = host.clone();
            let banner = banner.clone();
            let fallback = ssh_fallback.clone();
            let reconnecting = reconnecting.clone();
            let child_pid = child_pid.clone();
            let working_dir = working_dir.clone();
            let session = session.clone();
            let attempts = Rc::new(Cell::new(0u32));
            let exits = Rc::new(Cell::new(0u64));
            terminal.connect_child_exited(move |terminal, status| {
                exits.set(exits.get() + 1);
                child_pid.set(None);
                if ssh::remote_tmux_missing(status) && !fallback.get() {
                    fallback.set(true);
                    banner.set_title(NO_TMUX_BANNER);
                    banner.set_revealed(true);
                    spawn(
                        terminal,
                        proxy::wrap(ssh::plain_argv(&host)),
                        &working_dir,
                        &id,
                        &on_exited,
                        &child_pid,
                    );
                } else if ssh::connection_lost(status) || ssh::killed(status) {
                    // A dead transport or a killed child (logout,
                    // shutdown) is not a user exit: the pane must
                    // survive in state, so reattach after a backoff
                    // instead of dropping it (SPEC.md "SSH panes").
                    reconnecting.set(true);
                    banner.set_title(RECONNECT_BANNER);
                    banner.set_revealed(true);
                    let attempt = attempts.get();
                    attempts.set(attempt.saturating_add(1));
                    let argv = if fallback.get() {
                        ssh::plain_argv(&host)
                    } else {
                        ssh::attach_remote_argv(&host, &session)
                    };
                    let delay = Duration::from_secs(1 << attempt.min(4));
                    let terminal = terminal.downgrade();
                    let banner = banner.downgrade();
                    let fallback = fallback.clone();
                    let reconnecting = reconnecting.clone();
                    let child_pid = child_pid.clone();
                    let attempts = attempts.clone();
                    let exits = exits.clone();
                    let working_dir = working_dir.clone();
                    let id = id.clone();
                    let on_exited = on_exited.clone();
                    gtk::glib::timeout_add_local_once(delay, move || {
                        // The pane may have been closed while waiting.
                        let Some(terminal) = terminal.upgrade() else {
                            return;
                        };
                        spawn(
                            &terminal,
                            proxy::wrap(argv),
                            &working_dir,
                            &id,
                            &on_exited,
                            &child_pid,
                        );
                        let seen = exits.get();
                        gtk::glib::timeout_add_local_once(RECONNECT_SETTLED, move || {
                            if exits.get() != seen {
                                return; // died again; the next attempt owns the banner
                            }
                            attempts.set(0);
                            reconnecting.set(false);
                            if let Some(banner) = banner.upgrade() {
                                if fallback.get() {
                                    banner.set_title(NO_TMUX_BANNER);
                                } else {
                                    banner.set_revealed(false);
                                }
                            }
                        });
                    });
                } else {
                    on_exited(&id);
                }
            });
        }
        PaneKind::Local => {
            let child_pid = child_pid.clone();
            terminal.connect_child_exited(move |_, _| {
                child_pid.set(None);
                on_exited(&id);
            });
            // tmux does not forward OSC 7, so this only ever fires for
            // plain-shell panes; stashed panes get their `last_dir`
            // from tmux at save time (window::capture_last_dirs).
            let id = spec.id.clone();
            let on_dir_changed = callbacks.on_dir_changed.clone();
            terminal.connect_current_directory_uri_notify(move |terminal| {
                if let Some(uri) = terminal.current_directory_uri()
                    && let Some(dir) = local_path_from_uri(&uri, &gtk::glib::host_name())
                {
                    on_dir_changed(&id, dir);
                }
            });
        }
    }

    // Every pane runs under the OSC 52 proxy: VTE silently drops
    // OSC 52, so copies made by anything inside the pane — a remote
    // tmux, the local tmux, a TUI selecting text itself — only reach
    // the system clipboard through it (SPEC.md "Selection & clipboard").
    let argv = match (&spec.kind, stashed) {
        (PaneKind::Ssh { host }, _) => proxy::wrap(ssh::attach_remote_argv(host, &session)),
        (PaneKind::Local, true) => proxy::wrap(tmux::attach_local_argv(tmux_conf, &session, &dir)),
        (PaneKind::Local, false) => proxy::wrap(vec![default_shell()]),
    };
    spawn(
        &terminal,
        argv,
        &working_dir,
        &spec.id,
        &callbacks.on_exited,
        &child_pid,
    );

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("pane");
    root.append(&banner);
    root.append(&terminal);

    Pane {
        id: spec.id.clone(),
        root: root.upcast(),
        terminal,
        stashed,
        ssh_fallback,
        reconnecting,
        child_pid,
        is_ssh: matches!(spec.kind, PaneKind::Ssh { .. }),
    }
}

fn spawn(
    terminal: &vte4::Terminal,
    argv_owned: Vec<String>,
    working_dir: &str,
    id: &str,
    on_exited: &Rc<dyn Fn(&str)>,
    child_pid: &Rc<Cell<Option<gtk::glib::Pid>>>,
) {
    let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();

    // Explicitly inherit our environment — an empty envv would spawn
    // the child with no environment at all.
    let envv_owned: Vec<String> = std::env::vars()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let envv: Vec<&str> = envv_owned.iter().map(String::as_str).collect();

    let id = id.to_owned();
    let on_exited = on_exited.clone();
    let child_pid = child_pid.clone();
    terminal.spawn_async(
        vte4::PtyFlags::DEFAULT,
        Some(working_dir),
        &argv,
        &envv,
        gtk::glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        gtk::gio::Cancellable::NONE,
        move |result| match result {
            Ok(pid) => child_pid.set(Some(pid)),
            Err(err) => {
                // spawn never started, so child-exited will not fire
                tracing::error!("pane spawn failed: {err}");
                on_exited(&id);
            }
        },
    );
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_owned())
}

/// Path from an OSC 7 `file://` URI, if it is on this machine. A shell
/// running `ssh` by hand reports the *remote's* directory — that must
/// never become a local respawn dir.
fn local_path_from_uri(uri: &str, local_host: &str) -> Option<PathBuf> {
    let (path, host) = gtk::glib::filename_from_uri(uri).ok()?;
    match host {
        None => Some(path),
        Some(host) => (host.eq_ignore_ascii_case("localhost")
            || host.eq_ignore_ascii_case(local_host))
        .then_some(path),
    }
}

/// Also called on live config reload; an empty description restores
/// the system monospace font.
pub fn apply_font(terminal: &vte4::Terminal, font: &str) {
    if font.is_empty() {
        terminal.set_font(None);
    } else {
        terminal.set_font(Some(&gtk::pango::FontDescription::from_string(font)));
    }
}

fn style(terminal: &vte4::Terminal, config: &Config) {
    apply_font(terminal, &config.appearance.font);
    // The window carries the glass backdrop; the terminal itself is
    // fully transparent (see data/style.css).
    let background = gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0);
    let palette_owned: Vec<gtk::gdk::RGBA> = PALETTE.iter().map(|hex| color(hex)).collect();
    let palette: Vec<&gtk::gdk::RGBA> = palette_owned.iter().collect();
    terminal.set_colors(Some(&color(FOREGROUND)), Some(&background), &palette);
}

fn color(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::WHITE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostless_uri_gives_the_path() {
        assert_eq!(
            local_path_from_uri("file:///home/e/dev", "fedora"),
            Some(PathBuf::from("/home/e/dev"))
        );
    }

    #[test]
    fn percent_encoding_is_decoded() {
        assert_eq!(
            local_path_from_uri("file:///home/e/my%20dir", "fedora"),
            Some(PathBuf::from("/home/e/my dir"))
        );
    }

    #[test]
    fn this_machine_and_localhost_count_as_local() {
        assert_eq!(
            local_path_from_uri("file://Fedora/home/e", "fedora"),
            Some(PathBuf::from("/home/e"))
        );
        assert_eq!(
            local_path_from_uri("file://localhost/home/e", "fedora"),
            Some(PathBuf::from("/home/e"))
        );
    }

    #[test]
    fn foreign_hostnames_are_ignored() {
        assert_eq!(
            local_path_from_uri("file://server.invalid/root", "fedora"),
            None
        );
    }

    #[test]
    fn non_file_uris_are_ignored() {
        assert_eq!(local_path_from_uri("https://example.com/e", "fedora"), None);
    }
}
