//! Main window: reconciles state with live tmux sessions, hosts one
//! tiling grid per workflow, and wires the sidebar, pane lifecycle,
//! and keybindings together.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use anyhow::{Context as _, Result};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use stashee_core::config::Config;
use stashee_core::layout::{self, Direction};
use stashee_core::model::{PaneKind, PaneSpec, Workflow};
use stashee_core::state::State;
use stashee_core::{ssh, tmux};

use crate::cli;
use crate::grid::TilingGrid;
use crate::pane::{self, Pane};
use crate::paths;
use crate::settings::Backdrop;
use crate::sidebar::Sidebar;
use crate::{keys, settings};

thread_local! {
    /// The one live window's context, for `stashee <name>` arriving
    /// over D-Bus after startup (see main.rs).
    static CTX: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
}

/// One workflow's widgets and live panes. A view exists for every
/// workflow, not just the visible one: panes of inactive workflows
/// stay attached, so switching is instant and output keeps flowing.
pub(crate) struct View {
    name: String,
    /// "empty" placeholder or the grid.
    stack: gtk::Stack,
    grid: TilingGrid,
    pub(crate) panes: Vec<Pane>,
    /// Pane `Ctrl+W` acts on.
    focused: Option<String>,
}

/// Shared by window.rs and its satellites keys.rs (shortcut assembly)
/// and settings.rs (backdrop, config watch); fields those two need are
/// `pub(crate)`.
pub(crate) struct Ctx {
    /// Replaced wholesale on live config reload (settings.rs).
    pub(crate) config: RefCell<Config>,
    state: RefCell<State>,
    /// Same order as `state.workflows` — index-addressable from the
    /// sidebar and `Alt+N`.
    pub(crate) views: RefCell<Vec<View>>,
    active: RefCell<String>,
    content: gtk::Stack,
    title: adw::WindowTitle,
    sidebar: Sidebar,
    toasts: adw::ToastOverlay,
    tmux_conf: PathBuf,
    pub(crate) backdrop: Rc<Backdrop>,
    /// For swapping the shortcut controller on config reload.
    pub(crate) window: glib::WeakRef<adw::ApplicationWindow>,
    pub(crate) shortcuts: RefCell<Option<gtk::ShortcutController>>,
    /// Held so the config.toml watch keeps firing (settings.rs).
    pub(crate) config_monitor: RefCell<Option<gio::FileMonitor>>,
}

pub fn present(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    match build(app) {
        Ok(window) => window.present(),
        Err(err) => {
            tracing::error!("startup failed: {err:#}");
            fallback(app, &format!("{err:#}")).present();
        }
    }
}

/// `stashee <name>`: open or focus the workflow, creating it if new.
/// Names collide by tmux slug — the same uniqueness rule as in-app.
pub fn present_workflow(app: &adw::Application, name: &str) {
    present(app);
    // no context = startup failed and the fallback window is showing
    let Some(ctx) = CTX.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let slug = tmux::sanitize(name);
    let existing = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .find(|wf| tmux::sanitize(&wf.name) == slug)
        .map(|wf| wf.name.clone());
    match existing {
        Some(existing) => switch_to(&ctx, &existing),
        None => create_workflow(&ctx, name),
    }
}

/// Pre-create the very first launch's pane session so it greets before
/// the shell (SPEC.md "Workflows"). Best-effort: on any failure the
/// pane's own `new-session -A` still spawns it, just without the
/// greeting.
fn spawn_welcome_session(tmux_conf: &Path, session: &str) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!("cannot resolve own executable; skipping the greeting: {err}");
            return;
        }
    };
    let argv = tmux::welcome_session_argv(tmux_conf, session, &glib::home_dir(), &exe);
    match Command::new(&argv[0]).args(&argv[1..]).status() {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!("welcome session exited with {status}"),
        Err(err) => tracing::warn!("cannot pre-create the welcome session: {err}"),
    }
}

fn build(app: &adw::Application) -> Result<adw::ApplicationWindow> {
    Command::new("tmux")
        .arg("-V")
        .output()
        .context("tmux is required but was not found — install it with `sudo dnf install tmux`")?;
    let tmux_conf = paths::ensure_tmux_conf()?;

    let mut warnings: Vec<String> = Vec::new();

    // First run: materialize the commented template so the config
    // documents itself. Failure is not worth blocking startup over.
    if let Err(err) = Config::ensure(&paths::config_file()) {
        tracing::warn!("cannot write the config template: {err:#}");
    }
    let config = Config::load(&paths::config_file()).unwrap_or_else(|err| {
        tracing::warn!("config unreadable, using defaults: {err:#}");
        warnings.push("config.toml is unreadable — using defaults".to_owned());
        Config::default()
    });

    let state_path = paths::state_file();
    let mut state = State::load(&state_path).unwrap_or_else(|err| {
        tracing::warn!("state unreadable, starting fresh: {err:#}");
        let backup = state_path.with_extension("toml.bak");
        if std::fs::rename(&state_path, &backup).is_ok() {
            warnings.push(format!(
                "state.toml was corrupt — backed up as {}",
                backup.display()
            ));
        }
        State::default()
    });

    let live = cli::live_sessions();
    state.reconcile(
        &live,
        &glib::home_dir(),
        stashee_core::state::current_boot_id().as_deref(),
    );
    if state.workflows.is_empty() {
        let mut workflow = Workflow::new("Welcome", glib::home_dir());
        let pane = PaneSpec::new_local();
        spawn_welcome_session(&tmux_conf, &tmux::session_name(&workflow.name, &pane.id));
        workflow.panes.push(pane);
        state.last_active = Some(workflow.name.clone());
        state.workflows.push(workflow);
    }
    let active = state
        .last_active
        .clone()
        .filter(|name| {
            state
                .workflows
                .iter()
                .any(|wf| wf.name.eq_ignore_ascii_case(name))
        })
        .or_else(|| state.workflows.first().map(|wf| wf.name.clone()))
        .context("no workflow to open")?;
    state.last_active = Some(active.clone());

    let content = gtk::Stack::new();
    content.set_transition_type(gtk::StackTransitionType::Crossfade);
    content.set_transition_duration(150);
    content.set_hexpand(true);

    let sidebar = Sidebar::new();
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(sidebar.widget());
    body.append(&content);

    let title = adw::WindowTitle::new("stashee", &active);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let ssh_button = gtk::Button::with_label("+ SSH");
    ssh_button.set_action_name(Some("win.new-ssh-pane"));
    ssh_button.set_tooltip_text(Some("New SSH pane (Ctrl+Shift+T)"));
    header.pack_start(&ssh_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("stashee")
        .default_width(1200)
        .default_height(760)
        .content(&toasts)
        .build();
    window.add_css_class("stashee");
    let backdrop = settings::install_backdrop(&window, config.appearance.opacity);

    let ctx = Rc::new(Ctx {
        config: RefCell::new(config),
        state: RefCell::new(state),
        views: RefCell::new(Vec::new()),
        active: RefCell::new(active.clone()),
        content,
        title,
        sidebar,
        toasts,
        tmux_conf,
        backdrop,
        window: window.downgrade(),
        shortcuts: RefCell::new(None),
        config_monitor: RefCell::new(None),
    });

    let workflows = ctx.state.borrow().workflows.clone();
    for workflow in &workflows {
        build_view(&ctx, workflow, &live);
    }
    switch_to(&ctx, &active);

    install_actions(&ctx, &window);
    warnings.extend(keys::install_shortcuts(&ctx, &window));
    settings::watch_config(&ctx);

    {
        let for_select = ctx.clone();
        ctx.sidebar.connect_select(move |index| {
            // defer: switching rebuilds the sidebar, and mutating the
            // list from inside its own row-activated signal is unsafe
            let ctx = for_select.clone();
            glib::idle_add_local_once(move || switch_nth(&ctx, index));
        });
    }
    {
        let for_add = ctx.clone();
        ctx.sidebar
            .connect_add(move || add_workflow_dialog(&for_add));
    }
    {
        let for_folder = ctx.clone();
        ctx.sidebar.connect_folder(move || pick_folder(&for_folder));
    }

    {
        let ctx = ctx.clone();
        window.connect_map(move |_| focus_active_pane(&ctx));
    }

    for message in &warnings {
        toast(&ctx, message);
    }
    CTX.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
    Ok(window)
}

fn fallback(app: &adw::Application, message: &str) -> adw::ApplicationWindow {
    let page = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("stashee could not start")
        .description(message)
        .build();
    adw::ApplicationWindow::builder()
        .application(app)
        .title("stashee")
        .default_width(720)
        .default_height(480)
        .content(&page)
        .build()
}

fn install_actions(ctx: &Rc<Ctx>, window: &adw::ApplicationWindow) {
    let action = gio::SimpleAction::new("new-pane", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| new_pane(&ctx));
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("new-ssh-pane", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| ssh_host_dialog(&ctx));
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("close-pane", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| close_focused_pane(&ctx));
    }
    window.add_action(&action);

    // Dispatched by the sidebar's context menu with the workflow name
    // as the target.
    let action = gio::SimpleAction::new("rename-workflow", Some(glib::VariantTy::STRING));
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(name) = parameter.and_then(glib::Variant::get::<String>) {
                rename_workflow_dialog(&ctx, &name);
            }
        });
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("toggle-stash", Some(glib::VariantTy::STRING));
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(name) = parameter.and_then(glib::Variant::get::<String>) {
                toggle_stash(&ctx, &name);
            }
        });
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("delete-workflow", Some(glib::VariantTy::STRING));
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(name) = parameter.and_then(glib::Variant::get::<String>) {
                delete_workflow(&ctx, &name);
            }
        });
    }
    window.add_action(&action);
}

/// Build the view (widgets + attached panes) for `workflow` and append
/// it to the content stack. Call order must mirror `state.workflows`.
/// `live` decides each pane's mode: an alive session reattaches as a
/// tmux pane even when the workflow no longer stashes.
fn build_view(ctx: &Rc<Ctx>, workflow: &Workflow, live: &[String]) {
    let empty = adw::StatusPage::builder()
        .icon_name("utilities-terminal-symbolic")
        .title("Stashed and empty")
        .description("Press Ctrl+T to open a terminal")
        .build();
    let grid = TilingGrid::new();
    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&grid, Some("grid"));

    let panes: Vec<Pane> = workflow
        .panes
        .iter()
        .map(|spec| {
            pane::build(
                spec,
                workflow,
                workflow.pane_stashed(spec, live),
                &ctx.config.borrow(),
                &ctx.tmux_conf,
                &callbacks(ctx),
            )
        })
        .collect();

    ctx.content.add_child(&stack);
    let index = {
        let mut views = ctx.views.borrow_mut();
        views.push(View {
            name: workflow.name.clone(),
            stack,
            grid,
            panes,
            focused: None,
        });
        views.len() - 1
    };
    refresh_view(ctx, index);
}

fn callbacks(ctx: &Rc<Ctx>) -> pane::Callbacks {
    let for_exit = ctx.clone();
    let for_focus = ctx.clone();
    let for_dir = ctx.clone();
    pane::Callbacks {
        on_exited: Rc::new(move |id| remove_pane(&for_exit, id)),
        on_focus: Rc::new(move |id| {
            let mut views = for_focus.views.borrow_mut();
            if let Some(view) = views
                .iter_mut()
                .find(|view| view.panes.iter().any(|pane| pane.id == id))
            {
                view.focused = Some(id.to_owned());
            }
        }),
        on_dir_changed: Rc::new(move |id, dir| set_last_dir(&for_dir, id, dir)),
    }
}

/// OSC 7 from a pane: remember the directory it is in, so a plain-shell
/// pane respawns there on the next launch (SPEC.md "Workflows").
fn set_last_dir(ctx: &Rc<Ctx>, id: &str, dir: PathBuf) {
    let changed = {
        let mut state = ctx.state.borrow_mut();
        let Some(spec) = state
            .workflows
            .iter_mut()
            .flat_map(|wf| wf.panes.iter_mut())
            .find(|spec| spec.id == id)
        else {
            return;
        };
        if spec.last_dir.as_ref() == Some(&dir) {
            false
        } else {
            spec.last_dir = Some(dir);
            true
        }
    };
    if changed {
        save(ctx);
    }
}

fn view_index(ctx: &Rc<Ctx>, name: &str) -> Option<usize> {
    ctx.views
        .borrow()
        .iter()
        .position(|view| view.name.eq_ignore_ascii_case(name))
}

fn current_workflow(ctx: &Rc<Ctx>) -> Option<Workflow> {
    let active = ctx.active.borrow();
    ctx.state
        .borrow()
        .workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(&active))
        .cloned()
}

/// Re-tile one view's grid. Clones the handles out first: GTK calls
/// can re-enter our callbacks, which must find the RefCells unborrowed.
fn refresh_view(ctx: &Rc<Ctx>, index: usize) {
    let handles = {
        let views = ctx.views.borrow();
        views.get(index).map(|view| {
            (
                view.grid.clone(),
                view.stack.clone(),
                view.panes
                    .iter()
                    .map(|pane| pane.root.clone())
                    .collect::<Vec<_>>(),
            )
        })
    };
    let Some((grid, stack, widgets)) = handles else {
        return;
    };
    grid.set_panes(&widgets);
    stack.set_visible_child_name(if widgets.is_empty() { "empty" } else { "grid" });
}

pub(crate) fn switch_nth(ctx: &Rc<Ctx>, index: usize) {
    let name = ctx.views.borrow().get(index).map(|view| view.name.clone());
    if let Some(name) = name {
        switch_to(ctx, &name);
    }
}

fn switch_to(ctx: &Rc<Ctx>, name: &str) {
    let handles = {
        let views = ctx.views.borrow();
        views
            .iter()
            .find(|view| view.name.eq_ignore_ascii_case(name))
            .map(|view| (view.name.clone(), view.stack.clone()))
    };
    let Some((name, stack)) = handles else {
        return;
    };
    *ctx.active.borrow_mut() = name.clone();
    ctx.state.borrow_mut().last_active = Some(name.clone());
    ctx.title.set_subtitle(&name);
    ctx.content.set_visible_child(&stack);
    sync_sidebar(ctx);
    save(ctx);
    focus_active_pane(ctx);
}

fn sync_sidebar(ctx: &Rc<Ctx>) {
    let workflows = ctx.state.borrow().workflows.clone();
    let active = ctx.active.borrow().clone();
    ctx.sidebar.refresh(&workflows, &active);
}

/// The active workflow's focused pane, or its first when nothing was
/// focused yet.
pub(crate) fn focused_terminal(ctx: &Rc<Ctx>) -> Option<vte4::Terminal> {
    let views = ctx.views.borrow();
    let active = ctx.active.borrow();
    views
        .iter()
        .find(|view| view.name.eq_ignore_ascii_case(&active))
        .and_then(|view| {
            view.focused
                .as_ref()
                .and_then(|id| view.panes.iter().find(|pane| &pane.id == id))
                .or_else(|| view.panes.first())
                .map(|pane| pane.terminal.clone())
        })
}

fn focus_active_pane(ctx: &Rc<Ctx>) {
    if let Some(terminal) = focused_terminal(ctx) {
        terminal.grab_focus();
    }
}

/// `Alt+Arrows`: focus the neighbouring pane in the grid. The pane's
/// focus controller updates `view.focused` (see `callbacks`).
pub(crate) fn move_focus(ctx: &Rc<Ctx>, direction: Direction) {
    let terminal = {
        let views = ctx.views.borrow();
        let active = ctx.active.borrow();
        views
            .iter()
            .find(|view| view.name.eq_ignore_ascii_case(&active))
            .and_then(|view| {
                let current = view
                    .focused
                    .as_ref()
                    .and_then(|id| view.panes.iter().position(|pane| &pane.id == id))
                    .unwrap_or(0);
                let target = layout::neighbor(view.panes.len(), current, direction)?;
                view.panes.get(target).map(|pane| pane.terminal.clone())
            })
    };
    if let Some(terminal) = terminal {
        terminal.grab_focus();
    }
}

fn new_pane(ctx: &Rc<Ctx>) {
    add_pane(ctx, PaneSpec::new_local());
}

fn new_ssh_pane(ctx: &Rc<Ctx>, host: &str) {
    ctx.state.borrow_mut().remember_host(host);
    add_pane(ctx, PaneSpec::new_ssh(host));
}

/// Append `spec` to the active workflow and attach its pane.
fn add_pane(ctx: &Rc<Ctx>, spec: PaneSpec) {
    let active = ctx.active.borrow().clone();
    let workflow = {
        let mut state = ctx.state.borrow_mut();
        let Some(workflow) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(&active))
        else {
            return;
        };
        workflow.panes.push(spec.clone());
        workflow.clone()
    };
    let Some(index) = view_index(ctx, &active) else {
        return;
    };
    let pane = pane::build(
        &spec,
        &workflow,
        workflow.pane_stashed(&spec, &[]),
        &ctx.config.borrow(),
        &ctx.tmux_conf,
        &callbacks(ctx),
    );
    let terminal = pane.terminal.clone();
    {
        let mut views = ctx.views.borrow_mut();
        if let Some(view) = views.get_mut(index) {
            view.panes.push(pane);
            view.focused = Some(spec.id.clone());
        }
    }
    refresh_view(ctx, index);
    save(ctx);
    terminal.grab_focus();
}

/// The "+ SSH" prompt: a host entry with recent-hosts suggestions
/// (SPEC.md "SSH panes"). Activating a suggestion connects right away.
fn ssh_host_dialog(ctx: &Rc<Ctx>) {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("user@host"));
    entry.set_activates_default(true);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&entry);

    let dialog = adw::AlertDialog::new(
        Some("New SSH Pane"),
        Some("Anything your ssh command accepts: user@host or a Host alias from ~/.ssh/config."),
    );

    let recent = ctx.state.borrow().recent_hosts.clone();
    if !recent.is_empty() {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for host in &recent {
            let label = gtk::Label::new(Some(host));
            label.set_xalign(0.0);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            label.set_margin_start(12);
            label.set_margin_end(12);
            list.append(&label);
        }
        {
            let ctx = ctx.clone();
            let dialog = dialog.clone();
            list.connect_row_activated(move |_, row| {
                if let Some(host) = recent.get(row.index().max(0) as usize) {
                    dialog.close();
                    new_ssh_pane(&ctx, host);
                }
            });
        }
        content.append(&list);
    }

    dialog.set_extra_child(Some(&content));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("connect", "Connect");
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("connect"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("connect", false);

    {
        let dialog = dialog.clone();
        entry.connect_changed(move |entry| {
            dialog.set_response_enabled("connect", host_is_usable(entry.text().trim()));
        });
    }
    {
        let ctx = ctx.clone();
        let entry = entry.clone();
        dialog.connect_response(Some("connect"), move |_, _| {
            let text = entry.text();
            let host = text.trim();
            if host_is_usable(host) {
                new_ssh_pane(&ctx, host);
            }
        });
    }
    dialog.present(Some(&ctx.toasts));
    entry.grab_focus();
}

/// Non-empty and not option-like: the host sits before the `--` in our
/// ssh argv, so a leading dash would parse as an ssh flag.
fn host_is_usable(host: &str) -> bool {
    !host.is_empty() && !host.starts_with('-')
}

fn remove_pane(ctx: &Rc<Ctx>, id: &str) {
    let removed = {
        let mut views = ctx.views.borrow_mut();
        let Some(index) = views
            .iter()
            .position(|view| view.panes.iter().any(|pane| pane.id == id))
        else {
            return;
        };
        let view = &mut views[index];
        let Some(pos) = view.panes.iter().position(|pane| pane.id == id) else {
            return;
        };
        view.panes.remove(pos);
        if view.focused.as_deref() == Some(id) {
            // Browser tab-close behavior: focus the pane that takes
            // the closed pane's slot, or the new last one.
            view.focused = view
                .panes
                .get(pos.min(view.panes.len().saturating_sub(1)))
                .map(|pane| pane.id.clone());
        }
        (index, view.name.clone())
    };
    let (index, view_name) = removed;
    {
        let mut state = ctx.state.borrow_mut();
        if let Some(workflow) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(&view_name))
        {
            workflow.panes.retain(|spec| spec.id != id);
        }
    }
    refresh_view(ctx, index);
    save(ctx);
    if ctx.active.borrow().eq_ignore_ascii_case(&view_name) {
        focus_active_pane(ctx);
    }
}

fn close_focused_pane(ctx: &Rc<Ctx>) {
    let focused = {
        let views = ctx.views.borrow();
        let active = ctx.active.borrow();
        views
            .iter()
            .find(|view| view.name.eq_ignore_ascii_case(&active))
            .and_then(|view| view.focused.clone())
    };
    let Some(id) = focused else {
        return;
    };
    let Some(workflow) = current_workflow(ctx) else {
        return;
    };
    let Some(spec) = workflow.panes.iter().find(|spec| spec.id == id) else {
        return;
    };
    // The pane knows its own mode — the workflow's stash toggle may
    // have flipped since it was built. A fallen-back SSH pane (no tmux
    // on the host) has no remote session to kill; dropping the widget
    // ends its plain connection.
    let (stashed, ssh_fallback, reconnecting) = {
        let views = ctx.views.borrow();
        views
            .iter()
            .flat_map(|view| view.panes.iter())
            .find(|pane| pane.id == id)
            .map_or((workflow.stash, false, false), |pane| {
                (
                    pane.stashed,
                    pane.ssh_fallback.get(),
                    pane.reconnecting.get(),
                )
            })
    };
    let session = tmux::session_name(&workflow.name, &id);
    match (&spec.kind, stashed) {
        // The killed session makes the pane's client exit, and
        // child-exited drives the UI update — one code path.
        (PaneKind::Local, true) => {
            run_detached(
                ctx,
                tmux::kill_session_argv(&session),
                "Could not close the pane",
            );
        }
        (PaneKind::Ssh { .. }, _) if ssh_fallback => remove_pane(ctx, &id),
        // Mid-reconnect there is no live client whose exit could drive
        // the removal: kill the remote session best-effort and remove
        // the pane directly.
        (PaneKind::Ssh { host }, _) if reconnecting => {
            run_detached(
                ctx,
                ssh::kill_remote_argv(host, &session),
                "Could not close the pane",
            );
            remove_pane(ctx, &id);
        }
        (PaneKind::Ssh { host }, _) => {
            run_detached(
                ctx,
                ssh::kill_remote_argv(host, &session),
                "Could not close the pane",
            );
        }
        // no session to kill; remove the plain-shell pane directly
        (PaneKind::Local, false) => remove_pane(ctx, &id),
    }
}

/// Fire and forget; the outcome, if any, arrives via child-exited.
fn run_detached(ctx: &Rc<Ctx>, argv: Vec<String>, failure: &str) {
    match Command::new(&argv[0]).args(&argv[1..]).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait(); // reap
            });
        }
        Err(err) => {
            tracing::error!("{failure}: {err}");
            toast(ctx, failure);
        }
    }
}

/// Modal text-entry dialog shared by "new workflow" and rename.
/// `current` is the name being renamed (empty when creating), exempt
/// from the uniqueness check.
fn name_dialog(
    ctx: &Rc<Ctx>,
    heading: &str,
    confirm: &str,
    current: &str,
    on_confirm: impl Fn(&Rc<Ctx>, &str) + 'static,
) {
    let entry = gtk::Entry::new();
    entry.set_text(current);
    entry.set_placeholder_text(Some("workflow name"));
    entry.set_activates_default(true);

    let dialog = adw::AlertDialog::new(Some(heading), None);
    dialog.set_extra_child(Some(&entry));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", confirm);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("confirm"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("confirm", name_is_free(ctx, current.trim(), current));

    {
        let ctx = ctx.clone();
        let dialog = dialog.clone();
        let current = current.to_owned();
        entry.connect_changed(move |entry| {
            let text = entry.text();
            dialog.set_response_enabled("confirm", name_is_free(&ctx, text.trim(), &current));
        });
    }
    {
        let ctx = ctx.clone();
        let entry = entry.clone();
        dialog.connect_response(Some("confirm"), move |_, _| {
            let text = entry.text();
            let name = text.trim();
            if !name.is_empty() {
                on_confirm(&ctx, name);
            }
        });
    }
    dialog.present(Some(&ctx.toasts));
    entry.grab_focus();
}

/// A name is usable when non-empty and no *other* workflow collides
/// with it. Collision is by tmux slug — that is what keys session
/// ownership, so "Work!" and "work" count as the same name.
fn name_is_free(ctx: &Rc<Ctx>, name: &str, current: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let slug = tmux::sanitize(name);
    ctx.state
        .borrow()
        .workflows
        .iter()
        .filter(|wf| !wf.name.eq_ignore_ascii_case(current))
        .all(|wf| tmux::sanitize(&wf.name) != slug)
}

fn add_workflow_dialog(ctx: &Rc<Ctx>) {
    name_dialog(ctx, "New Workflow", "Create", "", |ctx, name| {
        create_workflow(ctx, name);
    });
}

fn create_workflow(ctx: &Rc<Ctx>, name: &str) {
    if !name_is_free(ctx, name, "") {
        toast(ctx, "That name is already taken");
        return;
    }
    let mut workflow = Workflow::new(name, glib::home_dir());
    workflow.panes.push(PaneSpec::new_local());
    ctx.state.borrow_mut().workflows.push(workflow.clone());
    build_view(ctx, &workflow, &[]);
    switch_to(ctx, name);
}

fn rename_workflow_dialog(ctx: &Rc<Ctx>, name: &str) {
    let heading = format!("Rename “{name}”");
    let old = name.to_owned();
    name_dialog(ctx, &heading, "Rename", name, move |ctx, new| {
        apply_rename(ctx, &old, new);
    });
}

fn apply_rename(ctx: &Rc<Ctx>, old: &str, new: &str) {
    if old == new {
        return;
    }
    if !name_is_free(ctx, new, old) {
        toast(ctx, "That name is already taken");
        return;
    }
    let Some(workflow) = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(old))
        .cloned()
    else {
        return;
    };

    // Sessions follow the name (SPEC "Workflows"), otherwise the next
    // start would attach to fresh sessions and orphan the old ones.
    // Per-pane mode, not the workflow toggle: a pane built stashed
    // keeps a session that must follow the rename.
    let stashed_ids = stashed_pane_ids(ctx, old);
    if tmux::sanitize(old) != tmux::sanitize(new) {
        for spec in &workflow.panes {
            let from = tmux::session_name(old, &spec.id);
            let to = tmux::session_name(new, &spec.id);
            match &spec.kind {
                PaneKind::Local if stashed_ids.contains(&spec.id) => {
                    // synchronous on purpose: local tmux answers
                    // instantly and a failed rename must not pass silently
                    let argv = tmux::rename_session_argv(&from, &to);
                    match Command::new(&argv[0]).args(&argv[1..]).output() {
                        Ok(out) if out.status.success() => {}
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            tracing::error!("renaming {from}: {}", stderr.trim());
                            toast(ctx, "Could not rename a pane's tmux session");
                        }
                        Err(err) => {
                            tracing::error!("renaming {from}: {err}");
                            toast(ctx, "Could not rename a pane's tmux session");
                        }
                    }
                }
                PaneKind::Local => {}
                PaneKind::Ssh { host } => run_detached(
                    ctx,
                    ssh::rename_remote_argv(host, &from, &to),
                    "Could not rename a remote session",
                ),
            }
        }
    }

    {
        let mut state = ctx.state.borrow_mut();
        if let Some(wf) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(old))
        {
            wf.name = new.to_owned();
        }
    }
    if let Some(index) = view_index(ctx, old)
        && let Some(view) = ctx.views.borrow_mut().get_mut(index)
    {
        view.name = new.to_owned();
    }
    if ctx.active.borrow().eq_ignore_ascii_case(old) {
        *ctx.active.borrow_mut() = new.to_owned();
        ctx.state.borrow_mut().last_active = Some(new.to_owned());
        ctx.title.set_subtitle(new);
    }
    sync_sidebar(ctx);
    save(ctx);
}

/// Ids of `workflow`'s panes that run inside tmux, per the live view.
fn stashed_pane_ids(ctx: &Rc<Ctx>, workflow: &str) -> Vec<String> {
    let views = ctx.views.borrow();
    views
        .iter()
        .find(|view| view.name.eq_ignore_ascii_case(workflow))
        .map(|view| {
            view.panes
                .iter()
                .filter(|pane| pane.stashed)
                .map(|pane| pane.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn toggle_stash(ctx: &Rc<Ctx>, name: &str) {
    let stash = {
        let mut state = ctx.state.borrow_mut();
        let Some(workflow) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(name))
        else {
            return;
        };
        workflow.stash = !workflow.stash;
        workflow.stash
    };
    save(ctx);
    sync_sidebar(ctx);
    // existing panes keep their mode until closed (SPEC "Workflows")
    toast(
        ctx,
        if stash {
            "Stashing on — new panes survive the app"
        } else {
            "Stashing off — new panes die with the app"
        },
    );
}

fn delete_workflow(ctx: &Rc<Ctx>, name: &str) {
    if !ctx.config.borrow().behavior.confirm_workflow_delete {
        apply_delete(ctx, name);
        return;
    }
    let panes = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(name))
        .map_or(0, |wf| wf.panes.len());
    let body = match panes {
        0 => "It has no open shells.".to_owned(),
        1 => "Its shell will be killed.".to_owned(),
        n => format!("Its {n} shells will be killed."),
    };
    let dialog = adw::AlertDialog::new(Some(&format!("Delete “{name}”?")), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    {
        let ctx = ctx.clone();
        let name = name.to_owned();
        dialog.connect_response(Some("delete"), move |_, _| apply_delete(&ctx, &name));
    }
    dialog.present(Some(&ctx.toasts));
}

fn apply_delete(ctx: &Rc<Ctx>, name: &str) {
    let Some(workflow) = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(name))
        .cloned()
    else {
        return;
    };
    let Some(index) = view_index(ctx, name) else {
        return;
    };

    // Drop the widgets first: clients detach, and the panes'
    // child-exited handlers die with them, so the kills below cannot
    // re-enter the pane lifecycle. Per-pane mode is read off the view
    // before it goes — the workflow toggle may have flipped since.
    let stashed_ids = stashed_pane_ids(ctx, name);
    let view = ctx.views.borrow_mut().remove(index);
    ctx.content.remove(&view.stack);
    drop(view);

    for spec in &workflow.panes {
        let session = tmux::session_name(&workflow.name, &spec.id);
        match &spec.kind {
            PaneKind::Local if stashed_ids.contains(&spec.id) => {
                run_detached(
                    ctx,
                    tmux::kill_session_argv(&session),
                    "Could not kill a session",
                );
            }
            PaneKind::Ssh { host } => {
                run_detached(
                    ctx,
                    ssh::kill_remote_argv(host, &session),
                    "Could not kill a remote session",
                );
            }
            PaneKind::Local => {} // plain shell dies with its widget
        }
    }

    ctx.state
        .borrow_mut()
        .workflows
        .retain(|wf| !wf.name.eq_ignore_ascii_case(name));

    if ctx.state.borrow().workflows.is_empty() {
        // the app always shows a workflow; recreate the first-launch
        // one (an ordinary pane — the greeting is for the first run)
        create_workflow(ctx, "Welcome");
        return;
    }
    if ctx.active.borrow().eq_ignore_ascii_case(name) {
        let first = ctx
            .state
            .borrow()
            .workflows
            .first()
            .map(|wf| wf.name.clone());
        if let Some(first) = first {
            switch_to(ctx, &first);
            return;
        }
    }
    sync_sidebar(ctx);
    save(ctx);
}

fn pick_folder(ctx: &Rc<Ctx>) {
    let Some(workflow) = current_workflow(ctx) else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Workflow folder")
        .modal(true)
        .build();
    dialog.set_initial_folder(Some(&gio::File::for_path(&workflow.default_dir)));
    let window = ctx.toasts.root().and_downcast::<gtk::Window>();
    let ctx = ctx.clone();
    dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
        // dismissing the dialog also lands here — only act on a pick
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        let active = ctx.active.borrow().clone();
        if let Some(workflow) = ctx
            .state
            .borrow_mut()
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(&active))
        {
            workflow.default_dir = path;
        }
        save(&ctx);
        sync_sidebar(&ctx);
    });
}

fn save(ctx: &Rc<Ctx>) {
    if let Err(err) = ctx.state.borrow().save(&paths::state_file()) {
        tracing::error!("saving state failed: {err:#}");
        toast(ctx, "Could not save state");
    }
}

pub(crate) fn toast(ctx: &Rc<Ctx>, message: &str) {
    // Toast titles are Pango markup, and messages carry arbitrary user
    // strings — key bindings ("<Ctrl>t"), workflow names.
    ctx.toasts
        .add_toast(adw::Toast::new(glib::markup_escape_text(message).as_str()));
}
