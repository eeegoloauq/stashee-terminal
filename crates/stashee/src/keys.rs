//! Keybindings: building the window's key controller from
//! `config.keys`. What each shortcut *does* lives in window.rs; this
//! module only turns bindings into matchers and wires them up.
//!
//! We match key events ourselves instead of using ShortcutController:
//! GTK's `KeyvalTrigger` stores its keyval lowercased, and GDK's
//! non-Latin layout fallback (`gdk_key_event_matches`) looks that
//! keyval up without re-applying Shift — so a Shift shortcut like
//! `<Ctrl><Shift>t` matches on a Latin layout but never on, say, a
//! Cyrillic one (the lookup lands on shift level 0 while the event
//! sits on level 1). Calling the same matcher with the *shifted*
//! keyval restores symmetry on every layout.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use vte4::prelude::*;

use stashee_core::config::Keys;
use stashee_core::layout::Direction;

use crate::voice;
use crate::window::{Ctx, focused_terminal, move_focus, switch_nth};

/// One parsed binding: an accelerator and what it runs.
struct Binding {
    keyval: gdk::Key,
    modifiers: gdk::ModifierType,
    run: Box<dyn Fn()>,
}

impl Binding {
    fn matches(&self, event: &gdk::KeyEvent) -> bool {
        let keyval = if self.modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            self.keyval.to_upper()
        } else {
            self.keyval
        };
        event.matches(keyval, self.modifiers) != gdk::KeyMatch::None
    }
}

/// An empty binding disables its shortcut; an unparsable one warns
/// and falls back to the default (which is ours, so it parses).
fn parse(
    binding: &str,
    default: &str,
    name: &str,
    warnings: &mut Vec<String>,
) -> Option<(gdk::Key, gdk::ModifierType)> {
    if binding.trim().is_empty() {
        return None;
    }
    if let Some(accel) = gtk::accelerator_parse(binding) {
        return Some(accel);
    }
    warnings.push(format!(
        "[keys] {name} = {binding:?} is not a valid accelerator — using {default:?}"
    ));
    gtk::accelerator_parse(default)
}

/// Build the window's shortcuts from `config.keys` — called again on
/// live config reload, replacing the previous controller. Returns
/// complaints about bindings that did not parse.
pub(crate) fn install_shortcuts(ctx: &Rc<Ctx>, window: &adw::ApplicationWindow) -> Vec<String> {
    let keys = ctx.config.borrow().keys.clone();
    let defaults = Keys::default();
    let mut warnings = Vec::new();
    let mut bindings: Vec<Binding> = Vec::new();

    for (binding, default, name, action) in [
        (
            &keys.new_pane,
            &defaults.new_pane,
            "new_pane",
            "win.new-pane",
        ),
        (
            &keys.new_ssh_pane,
            &defaults.new_ssh_pane,
            "new_ssh_pane",
            "win.new-ssh-pane",
        ),
        (
            &keys.close_pane,
            &defaults.close_pane,
            "close_pane",
            "win.close-pane",
        ),
    ] {
        if let Some((keyval, modifiers)) = parse(binding, default, name, &mut warnings) {
            let window = window.clone();
            bindings.push(Binding {
                keyval,
                modifiers,
                run: Box::new(move || {
                    if WidgetExt::activate_action(&window, action, None).is_err() {
                        tracing::warn!("action {action} is not available");
                    }
                }),
            });
        }
    }
    // Alt+N is fixed (not in `[keys]`): nine numbered bindings would
    // drown the config for a shortcut nobody remaps.
    for n in 1..=9usize {
        if let Some((keyval, modifiers)) = gtk::accelerator_parse(format!("<Alt>{n}")) {
            let ctx = ctx.clone();
            bindings.push(Binding {
                keyval,
                modifiers,
                run: Box::new(move || switch_nth(&ctx, n - 1)),
            });
        }
    }
    // Alt by default because the obvious alternatives are taken: GNOME
    // owns Ctrl+Alt+Arrows (workspaces), readline owns Ctrl+Arrows
    // (word motion). Same convention as Tilix and Terminator.
    for (binding, default, name, direction) in [
        (
            &keys.focus_left,
            &defaults.focus_left,
            "focus_left",
            Direction::Left,
        ),
        (
            &keys.focus_right,
            &defaults.focus_right,
            "focus_right",
            Direction::Right,
        ),
        (
            &keys.focus_up,
            &defaults.focus_up,
            "focus_up",
            Direction::Up,
        ),
        (
            &keys.focus_down,
            &defaults.focus_down,
            "focus_down",
            Direction::Down,
        ),
    ] {
        if let Some((keyval, modifiers)) = parse(binding, default, name, &mut warnings) {
            let ctx = ctx.clone();
            bindings.push(Binding {
                keyval,
                modifiers,
                run: Box::new(move || move_focus(&ctx, direction)),
            });
        }
    }
    if let Some((keyval, modifiers)) = parse(&keys.copy, &defaults.copy, "copy", &mut warnings) {
        let ctx = ctx.clone();
        bindings.push(Binding {
            keyval,
            modifiers,
            run: Box::new(move || {
                // Only with a native VTE selection: in tmux panes a mouse
                // selection belongs to tmux, not VTE, and copying "nothing"
                // would clobber the clipboard with an empty string.
                if let Some(terminal) = focused_terminal(&ctx)
                    && terminal.has_selection()
                {
                    terminal.copy_clipboard_format(vte4::Format::Text);
                }
            }),
        });
    }
    if let Some((keyval, modifiers)) = parse(&keys.paste, &defaults.paste, "paste", &mut warnings) {
        let ctx = ctx.clone();
        bindings.push(Binding {
            keyval,
            modifiers,
            run: Box::new(move || {
                if let Some(terminal) = focused_terminal(&ctx) {
                    terminal.paste_clipboard();
                }
            }),
        });
    }
    if let Some((keyval, modifiers)) = parse(&keys.voice, &defaults.voice, "voice", &mut warnings) {
        let ctx = ctx.clone();
        bindings.push(Binding {
            keyval,
            modifiers,
            run: Box::new(move || voice::toggle(&ctx)),
        });
    }

    // Capture phase, so the shortcuts win over the terminal's input.
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let ctx_for_events = ctx.clone();
    controller.connect_key_pressed(move |controller, keyval, _, _| {
        // Esc belongs to the shell — except while a voice recording
        // runs; then it cancels the recording and goes no further.
        if keyval == gdk::Key::Escape && voice::active(&ctx_for_events) {
            voice::cancel(&ctx_for_events);
            return glib::Propagation::Stop;
        }
        let Some(event) = controller
            .current_event()
            .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
        else {
            return glib::Propagation::Proceed;
        };
        match bindings.iter().find(|binding| binding.matches(&event)) {
            Some(binding) => {
                (binding.run)();
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });

    if let Some(old) = ctx.shortcuts.borrow_mut().take() {
        window.remove_controller(&old);
    }
    window.add_controller(controller.clone());
    *ctx.shortcuts.borrow_mut() = Some(controller);
    warnings
}
