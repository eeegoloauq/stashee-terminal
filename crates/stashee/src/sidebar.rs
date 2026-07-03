//! Workflow sidebar: the list, an add button, and the active
//! workflow's folder. Pure presentation — selection and clicks are
//! forwarded through closures, and the context menu dispatches window
//! actions (`win.rename-workflow` etc.) carrying the workflow name.

use std::path::Path;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

use stashee_core::model::Workflow;

pub struct Sidebar {
    root: gtk::Box,
    list: gtk::ListBox,
    add: gtk::Button,
    folder: gtk::Button,
    folder_label: gtk::Label,
}

impl Sidebar {
    pub fn new() -> Self {
        let heading = gtk::Label::new(Some("Workflows"));
        heading.add_css_class("dim-label");
        heading.add_css_class("caption-heading");
        heading.set_halign(gtk::Align::Start);
        heading.set_margin_top(12);
        heading.set_margin_start(18);
        heading.set_margin_bottom(4);

        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);

        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let add = gtk::Button::new();
        add.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("New workflow")
                .build(),
        ));
        add.add_css_class("flat");
        add.set_margin_start(8);
        add.set_margin_end(8);
        add.set_margin_bottom(8);

        let folder_label = gtk::Label::new(None);
        folder_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let folder_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        folder_box.set_halign(gtk::Align::Start);
        folder_box.append(&gtk::Image::from_icon_name("folder-symbolic"));
        folder_box.append(&folder_label);
        let folder = gtk::Button::new();
        folder.set_child(Some(&folder_box));
        folder.add_css_class("flat");
        folder.set_margin_start(8);
        folder.set_margin_end(8);
        folder.set_margin_bottom(8);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("sidebar-panel");
        root.set_size_request(200, -1);
        root.append(&heading);
        root.append(&scroll);
        root.append(&add);
        root.append(&folder);

        Self {
            root,
            list,
            add,
            folder,
            folder_label,
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// `f` receives the activated row's index; list order mirrors
    /// `state.workflows`.
    pub fn connect_select(&self, f: impl Fn(usize) + 'static) {
        self.list.connect_row_activated(move |_, row| {
            if let Ok(index) = usize::try_from(row.index()) {
                f(index);
            }
        });
    }

    pub fn connect_add(&self, f: impl Fn() + 'static) {
        self.add.connect_clicked(move |_| f());
    }

    pub fn connect_folder(&self, f: impl Fn() + 'static) {
        self.folder.connect_clicked(move |_| f());
    }

    /// Rebuild the list to mirror `workflows`; `active` drives the
    /// selected row and the folder button.
    pub fn refresh(&self, workflows: &[Workflow], active: &str) {
        self.list.remove_all();
        for workflow in workflows {
            let label = gtk::Label::new(Some(&workflow.name));
            label.set_halign(gtk::Align::Start);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label));
            attach_menu(&row, &workflow.name, workflow.stash);
            self.list.append(&row);
            if workflow.name.eq_ignore_ascii_case(active) {
                self.list.select_row(Some(&row));
                self.folder_label
                    .set_text(&display_dir(&workflow.default_dir));
                self.folder
                    .set_tooltip_text(Some(&workflow.default_dir.display().to_string()));
            }
        }
    }
}

/// Right-click menu on a workflow row. Entries dispatch window actions
/// with the workflow name as the target, so the menu stays stateless.
fn attach_menu(row: &gtk::ListBoxRow, name: &str, stash: bool) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let name = name.to_owned();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(row) = gesture.widget() else {
            return;
        };
        let menu = gio::Menu::new();
        menu.append_item(&action_item("Rename…", "win.rename-workflow", &name));
        let toggle = if stash {
            "Turn off stashing"
        } else {
            "Turn on stashing"
        };
        menu.append_item(&action_item(toggle, "win.toggle-stash", &name));
        menu.append_item(&action_item("Delete…", "win.delete-workflow", &name));

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&row);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| {
            // unparenting inside the signal warns; defer one tick
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    });
    row.add_controller(gesture);
}

fn action_item(label: &str, action: &str, workflow: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&workflow.to_variant()));
    item
}

fn display_dir(dir: &Path) -> String {
    match dir.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => dir.display().to_string(),
    }
}
