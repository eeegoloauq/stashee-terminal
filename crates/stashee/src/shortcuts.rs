//! The "Keyboard Shortcuts" dialog: `config.keys` rendered as keycap
//! rows. Hand-rolled rather than GtkShortcutsWindow (deprecated in
//! GTK 4.18, and it can't say "Alt" just once for Alt+arrows).
//! Read-only on purpose: config.toml is the settings GUI
//! (settings.rs), so the footer row opens it instead.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::paths;
use crate::window::Ctx;

/// `<Ctrl><Shift>t` → ["Ctrl", "Shift", "T"], one string per keycap.
/// None for disabled (empty) or unparsable bindings.
fn keycaps(accel: &str) -> Option<Vec<String>> {
    if accel.trim().is_empty() {
        return None;
    }
    let (keyval, modifiers) = gtk::accelerator_parse(accel.trim())?;
    let mut caps: Vec<String> = [
        (gdk::ModifierType::CONTROL_MASK, "Ctrl"),
        (gdk::ModifierType::SHIFT_MASK, "Shift"),
        (gdk::ModifierType::ALT_MASK, "Alt"),
        (gdk::ModifierType::SUPER_MASK, "Super"),
    ]
    .into_iter()
    .filter(|(mask, _)| modifiers.contains(*mask))
    .map(|(_, name)| name.to_owned())
    .collect();
    caps.push(key_label(keyval));
    Some(caps)
}

/// What goes on the key's cap: arrows as glyphs (as GTK's own
/// shortcut labels render them), printable keys uppercased, the rest
/// by keyval name.
fn key_label(keyval: gdk::Key) -> String {
    match keyval.name().as_deref() {
        Some("Left") => "←".to_owned(),
        Some("Right") => "→".to_owned(),
        Some("Up") => "↑".to_owned(),
        Some("Down") => "↓".to_owned(),
        name => keyval
            .to_unicode()
            .filter(|c| !c.is_control() && !c.is_whitespace())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| name.unwrap_or_default().replace('_', " ")),
    }
}

/// A settings-style row: title left, keycaps right. "…" is a plain
/// separator (for ranges like Alt 1…9), not a keycap.
fn shortcut_row(title: &str, caps: &[String]) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    let keys = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    keys.set_valign(gtk::Align::Center);
    for cap in caps {
        let label = gtk::Label::new(Some(cap));
        label.add_css_class(if cap == "…" { "dim-label" } else { "keycap" });
        keys.append(&label);
    }
    row.add_suffix(&keys);
    row
}

/// Assembled from the live config — rebound keys show as bound,
/// disabled (empty) ones drop their row.
pub(crate) fn show(ctx: &Rc<Ctx>) {
    let keys = ctx.config.borrow().keys.clone();

    let mut panes = Vec::new();
    for (title, accel) in [
        ("New pane", &keys.new_pane),
        ("New SSH pane", &keys.new_ssh_pane),
        ("Close pane", &keys.close_pane),
    ] {
        if let Some(caps) = keycaps(accel) {
            panes.push(shortcut_row(title, &caps));
        }
    }
    // The four focus bindings collapse into one row while they share
    // modifiers (the default: Alt once, then the arrows); rebound
    // apart, they fall back to a row per direction.
    let directions = [
        ("Focus left", keycaps(&keys.focus_left)),
        ("Focus right", keycaps(&keys.focus_right)),
        ("Focus up", keycaps(&keys.focus_up)),
        ("Focus down", keycaps(&keys.focus_down)),
    ];
    let bound: Vec<&Vec<String>> = directions.iter().filter_map(|(_, c)| c.as_ref()).collect();
    if bound.len() == directions.len()
        && bound
            .iter()
            .all(|caps| caps[..caps.len() - 1] == bound[0][..bound[0].len() - 1])
    {
        let mut caps = bound[0][..bound[0].len() - 1].to_vec();
        caps.extend(bound.iter().map(|caps| caps[caps.len() - 1].clone()));
        panes.push(shortcut_row("Move focus", &caps));
    } else {
        for (title, caps) in &directions {
            if let Some(caps) = caps {
                panes.push(shortcut_row(title, caps));
            }
        }
    }

    // Alt+drag is a pointer gesture, not a rebindable accelerator.
    panes.push(shortcut_row(
        "Move pane (swap, or drop on a workflow)",
        &["Alt", "Drag"].map(str::to_owned),
    ));

    // Alt+N is fixed (keys.rs), so the range is spelled directly.
    let workflows = vec![shortcut_row(
        "Switch workflow",
        &["Alt", "1", "…", "9"].map(str::to_owned),
    )];

    let mut clipboard = Vec::new();
    for (title, accel) in [("Copy", &keys.copy), ("Paste", &keys.paste)] {
        if let Some(caps) = keycaps(accel) {
            clipboard.push(shortcut_row(title, &caps));
        }
    }

    let page = gtk::Box::new(gtk::Orientation::Vertical, 24);
    page.set_margin_top(12);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);
    for (title, rows) in [
        ("Panes", panes),
        ("Workflows", workflows),
        ("Clipboard", clipboard),
    ] {
        if rows.is_empty() {
            continue;
        }
        let group = adw::PreferencesGroup::builder().title(title).build();
        for row in rows {
            group.add(&row);
        }
        page.append(&group);
    }

    let edit = adw::ActionRow::builder()
        .title("Customize in config.toml")
        .subtitle("[keys] section — saving applies to the running app")
        .activatable(true)
        .build();
    edit.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let window = ctx.window.clone();
    edit.connect_activated(move |_| {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(paths::config_file())));
        launcher.launch(
            window.upgrade().as_ref(),
            gio::Cancellable::NONE,
            |result| {
                if let Err(err) = result {
                    tracing::warn!("cannot open config.toml: {err}");
                }
            },
        );
    });
    let footer = adw::PreferencesGroup::new();
    footer.add(&edit);
    page.append(&footer);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&page)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled));
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(440)
        .child(&toolbar)
        .build();
    dialog.present(Some(&ctx.toasts));
}
