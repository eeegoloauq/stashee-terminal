//! Live settings plumbing. config.toml *is* the settings GUI: this
//! module owns the runtime backdrop provider (the one glass token that
//! is both user-configurable and system-overridable) and the file
//! watch that applies saved changes to the running app.

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;

use stashee_core::config::Config;

use crate::window::{Ctx, toast};
use crate::{keys, pane, paths};

/// The window backdrop: user-configurable (`appearance.opacity`,
/// live-reloadable) and system-overridable (high contrast forces it
/// opaque), so it lives in a runtime provider layered over the static
/// CSS.
pub(crate) struct Backdrop {
    provider: gtk::CssProvider,
    opacity: Cell<f64>,
    reduced: Cell<bool>,
}

impl Backdrop {
    fn refresh(&self) {
        let alpha = if self.reduced.get() {
            1.0
        } else {
            self.opacity.get()
        };
        self.provider.load_from_string(&format!(
            "window.stashee {{ background-color: rgba(16, 18, 26, {alpha:.3}); }}"
        ));
    }
}

/// High contrast is GNOME's reduce-transparency signal: it forces the
/// backdrop opaque and swaps the surface tokens via the
/// `reduced-transparency` class. Tracked live.
pub(crate) fn install_backdrop(window: &adw::ApplicationWindow, opacity: f64) -> Rc<Backdrop> {
    let backdrop = Rc::new(Backdrop {
        provider: gtk::CssProvider::new(),
        opacity: Cell::new(opacity),
        reduced: Cell::new(false),
    });
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &backdrop.provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let window = window.downgrade();
    let for_contrast = backdrop.clone();
    let apply = move |manager: &adw::StyleManager| {
        let Some(window) = window.upgrade() else {
            return;
        };
        let reduced = manager.is_high_contrast();
        if reduced {
            window.add_css_class("reduced-transparency");
        } else {
            window.remove_css_class("reduced-transparency");
        }
        for_contrast.reduced.set(reduced);
        for_contrast.refresh();
    };
    let manager = adw::StyleManager::default();
    apply(&manager);
    manager.connect_high_contrast_notify(apply);
    backdrop
}

/// Live reload: saving config.toml applies to the running app.
/// CHANGES_DONE_HINT covers in-place writes; CREATED, RENAMED, and
/// MOVED_IN cover editors that save atomically (vim, GNOME Text
/// Editor). A vanished file is left alone — current settings stand
/// until the next start.
pub(crate) fn watch_config(ctx: &Rc<Ctx>) {
    let file = gio::File::for_path(paths::config_file());
    let monitor =
        match file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => monitor,
            Err(err) => {
                tracing::warn!("cannot watch config.toml, live reload disabled: {err}");
                return;
            }
        };
    let weak = Rc::downgrade(ctx);
    monitor.connect_changed(move |_, _, _, event| {
        if matches!(
            event,
            gio::FileMonitorEvent::ChangesDoneHint
                | gio::FileMonitorEvent::Created
                | gio::FileMonitorEvent::Renamed
                | gio::FileMonitorEvent::MovedIn
        ) && let Some(ctx) = weak.upgrade()
        {
            reload_config(&ctx);
        }
    });
    *ctx.config_monitor.borrow_mut() = Some(monitor);
}

fn reload_config(ctx: &Rc<Ctx>) {
    let new = match Config::load(&paths::config_file()) {
        Ok(config) => config,
        Err(err) => {
            let cause = err.root_cause().to_string();
            toast(
                ctx,
                &format!(
                    "config.toml not applied: {}",
                    cause.lines().next().unwrap_or_default()
                ),
            );
            return;
        }
    };
    let old = ctx.config.replace(new.clone());
    if new == old {
        return;
    }
    if new.appearance.opacity != old.appearance.opacity {
        ctx.backdrop.opacity.set(new.appearance.opacity);
        ctx.backdrop.refresh();
    }
    if new.appearance.font != old.appearance.font {
        for view in ctx.views.borrow().iter() {
            for pane in &view.panes {
                pane::apply_font(&pane.terminal, &new.appearance.font);
            }
        }
    }
    if new.keys != old.keys
        && let Some(window) = ctx.window.upgrade()
    {
        for message in keys::install_shortcuts(ctx, &window) {
            toast(ctx, &message);
        }
    }
}
