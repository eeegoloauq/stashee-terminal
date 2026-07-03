//! Keybindings: building the window's shortcut controller from
//! `config.keys`. What each shortcut *does* lives in window.rs; this
//! module only turns bindings into triggers and wires them up.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use vte4::prelude::*;

use stashee_core::config::Keys;
use stashee_core::layout::Direction;

use crate::window::{Ctx, focused_terminal, move_focus, switch_nth};

/// An empty binding disables its shortcut; an unparsable one warns
/// and falls back to the default (which is ours, so it parses).
fn trigger(
    binding: &str,
    default: &str,
    name: &str,
    warnings: &mut Vec<String>,
) -> Option<gtk::ShortcutTrigger> {
    if binding.trim().is_empty() {
        return None;
    }
    if let Some(trigger) = gtk::ShortcutTrigger::parse_string(binding) {
        return Some(shift_normalized(trigger));
    }
    warnings.push(format!(
        "[keys] {name} = {binding:?} is not a valid accelerator — using {default:?}"
    ));
    gtk::ShortcutTrigger::parse_string(default).map(shift_normalized)
}

/// GTK's accelerator parser lowercases the keyval, and GDK's non-Latin
/// layout fallback (`gdk_key_event_matches`) looks that keyval up
/// without re-applying Shift — so "<Ctrl><Shift>c" matches on a Latin
/// layout but never on, say, a Cyrillic one. Rebuilding the trigger
/// with the shifted (upper) keyval matches on every layout; the exact
/// path upcases before comparing, so Latin layouts are unaffected.
fn shift_normalized(trigger: gtk::ShortcutTrigger) -> gtk::ShortcutTrigger {
    let Ok(keyval_trigger) = trigger.clone().downcast::<gtk::KeyvalTrigger>() else {
        return trigger;
    };
    let modifiers = keyval_trigger.modifiers();
    let upper = keyval_trigger.keyval().to_upper();
    if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) && upper != keyval_trigger.keyval() {
        gtk::KeyvalTrigger::new(upper, modifiers).upcast()
    } else {
        trigger
    }
}

/// Build the window's shortcuts from `config.keys` — called again on
/// live config reload, replacing the previous controller. Returns
/// complaints about bindings that did not parse.
pub(crate) fn install_shortcuts(ctx: &Rc<Ctx>, window: &adw::ApplicationWindow) -> Vec<String> {
    let keys = ctx.config.borrow().keys.clone();
    let defaults = Keys::default();
    let mut warnings = Vec::new();

    // Capture phase, so the shortcuts win over the terminal's input.
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
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
        if let Some(trigger) = trigger(binding, default, name, &mut warnings) {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
    }
    // Alt+N is fixed (not in `[keys]`): nine numbered bindings would
    // drown the config for a shortcut nobody remaps.
    for n in 1..=9usize {
        let ctx = ctx.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(&format!("<Alt>{n}")),
            Some(gtk::CallbackAction::new(move |_, _| {
                switch_nth(&ctx, n - 1);
                glib::Propagation::Stop
            })),
        ));
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
        if let Some(trigger) = trigger(binding, default, name, &mut warnings) {
            let ctx = ctx.clone();
            shortcuts.add_shortcut(gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::CallbackAction::new(move |_, _| {
                    move_focus(&ctx, direction);
                    glib::Propagation::Stop
                })),
            ));
        }
    }
    if let Some(trigger) = trigger(&keys.copy, &defaults.copy, "copy", &mut warnings) {
        let ctx = ctx.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            Some(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                // Only with a native VTE selection: in tmux panes a mouse
                // selection belongs to tmux, not VTE, and copying "nothing"
                // would clobber the clipboard with an empty string.
                if let Some(terminal) = focused_terminal(&ctx)
                    && terminal.has_selection()
                {
                    terminal.copy_clipboard_format(vte4::Format::Text);
                }
                glib::Propagation::Stop
            })),
        ));
    }
    if let Some(trigger) = trigger(&keys.paste, &defaults.paste, "paste", &mut warnings) {
        let ctx = ctx.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            Some(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                if let Some(terminal) = focused_terminal(&ctx) {
                    terminal.paste_clipboard();
                }
                glib::Propagation::Stop
            })),
        ));
    }

    if let Some(old) = ctx.shortcuts.borrow_mut().take() {
        window.remove_controller(&old);
    }
    window.add_controller(shortcuts.clone());
    *ctx.shortcuts.borrow_mut() = Some(shortcuts);
    warnings
}
