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

mod panes;
mod sessions;
mod workflows;

use panes::*;
use sessions::*;
use workflows::*;

use crate::cli;
use crate::grid::TilingGrid;
use crate::pane::{self, Pane};
use crate::paths;
use crate::settings::Backdrop;
use crate::sidebar::{self, Sidebar};
use crate::{keys, settings, shortcuts};

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
    /// Pane close-pane acts on.
    focused: Option<String>,
}

/// Shared by window.rs and its satellites keys.rs (shortcut assembly),
/// settings.rs (backdrop, config watch), and shortcuts.rs (the
/// shortcuts dialog); fields those need are `pub(crate)`.
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
    pub(crate) toasts: adw::ToastOverlay,
    tmux_conf: PathBuf,
    pub(crate) backdrop: Rc<Backdrop>,
    /// For swapping the shortcut controller on config reload.
    pub(crate) window: glib::WeakRef<adw::ApplicationWindow>,
    pub(crate) shortcuts: RefCell<Option<gtk::EventControllerKey>>,
    /// Held so the config.toml watch keeps firing (settings.rs).
    pub(crate) config_monitor: RefCell<Option<gio::FileMonitor>>,
    /// Held so the logind resume watch keeps firing (`watch_resume`);
    /// dropping the subscription unsubscribes.
    resume_watch: RefCell<Option<gio::SignalSubscription>>,
    /// Voice input state: session, transcriber, model download (voice.rs).
    pub(crate) voice: RefCell<crate::voice::VoiceCtl>,
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
    // Every run: refresh the reference copy so options added by this
    // version are discoverable without touching the user's file.
    if let Err(err) = Config::ensure_reference(&paths::config_reference()) {
        tracing::warn!("cannot refresh config.toml.default: {err:#}");
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

    let title = adw::WindowTitle::new(&active, "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let ssh_item = gio::MenuItem::new(Some("New SSH Pane…"), Some("win.new-ssh-pane"));
    ssh_item.set_attribute_value("accel", Some(&"<Ctrl><Shift>t".to_variant()));
    let new_menu = gio::Menu::new();
    // the split button itself follows the focused pane's connection —
    // this entry is the explicit way to a local shell from an SSH
    // workflow
    new_menu.append(Some("New Local Pane"), Some("win.new-local-pane"));
    new_menu.append_item(&ssh_item);
    let new_button = adw::SplitButton::builder()
        .icon_name("tab-new-symbolic")
        .tooltip_text("New pane (Ctrl+T)")
        .dropdown_tooltip("New SSH pane")
        .menu_model(&new_menu)
        .build();
    new_button.set_action_name(Some("win.new-pane"));
    // the button sits in the window corner — left-aligning the popover
    // keeps it hanging off the button instead of past the window edge
    if let Some(popover) = new_button.popover() {
        popover.set_halign(gtk::Align::Start);
    }
    header.pack_start(&new_button);
    let app_menu = gio::Menu::new();
    app_menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    app_menu.append(Some("About Stashee"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&app_menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    header.pack_end(&menu_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Stashee")
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
        resume_watch: RefCell::new(None),
        voice: RefCell::new(crate::voice::VoiceCtl::default()),
    });

    let workflows = ctx.state.borrow().workflows.clone();
    for workflow in &workflows {
        build_view(&ctx, workflow, &live);
    }
    switch_to(&ctx, &active);

    install_actions(&ctx, &window);
    warnings.extend(keys::install_shortcuts(&ctx, &window));
    settings::watch_config(&ctx);
    watch_resume(&ctx);

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
        let for_drop = ctx.clone();
        ctx.sidebar.connect_pane_drop(move |id, workflow| {
            move_pane_to_workflow(&for_drop, id, workflow);
        });
    }
    {
        let ctx = ctx.clone();
        window.connect_map(move |_| focus_active_pane(&ctx));
    }
    {
        // The last save before a possible reboot: no other save event
        // is guaranteed between the user's final `cd` and closing.
        let ctx = ctx.clone();
        window.connect_close_request(move |_| {
            save(&ctx);
            glib::Propagation::Proceed
        });
    }

    for message in &warnings {
        toast(&ctx, message);
    }
    CTX.with(|slot| *slot.borrow_mut() = Some(ctx.clone()));
    Ok(window)
}

/// The reconnect logic (pane.rs) only ever *reacts* to the ssh client
/// exiting, and after suspend/resume an idle ssh on a half-open TCP
/// link exits only when its keepalive timer notices (~30 s, see
/// ssh::PANE_OPTS). logind broadcasts `PrepareForSleep(false)` on
/// resume: probe every SSH pane's transport right then, so dead links
/// fail immediately instead. The probe is a SIGWINCH — ssh answers it
/// with a window-change packet, which a live connection ignores
/// entirely and a dead one trips over (exit 255 → reconnect). Without
/// a system bus (containers, non-systemd) keepalive stays the only
/// detector.
fn watch_resume(ctx: &Rc<Ctx>) {
    let ctx = ctx.clone();
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |bus| {
        let connection = match bus {
            Ok(connection) => connection,
            Err(err) => {
                tracing::debug!("no system bus; resume probe disabled: {err}");
                return;
            }
        };
        // Weak: the subscription lives in ctx — a strong ctx here
        // would be a cycle.
        let weak = Rc::downgrade(&ctx);
        let subscription = connection.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(ctx) = weak.upgrade() else { return };
                // true = going down, false = back up
                if signal.parameters.get::<(bool,)>() == Some((false,)) {
                    probe_ssh_transports(&ctx);
                }
            },
        );
        *ctx.resume_watch.borrow_mut() = Some(subscription);
    });
}

/// One SIGWINCH per SSH pane, visible workflows or not — hidden panes
/// are exactly the ones nothing else would ever poke.
fn probe_ssh_transports(ctx: &Rc<Ctx>) {
    for view in ctx.views.borrow().iter() {
        for pane in &view.panes {
            if pane.is_ssh
                && let Some(pid) = pane.child_pid.get()
            {
                stashee_pty::send_sigwinch(pid.0);
            }
        }
    }
}

fn fallback(app: &adw::Application, message: &str) -> adw::ApplicationWindow {
    let page = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Stashee could not start")
        .description(message)
        .build();
    adw::ApplicationWindow::builder()
        .application(app)
        .title("Stashee")
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

    let action = gio::SimpleAction::new("new-local-pane", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| add_pane(&ctx, PaneSpec::new_local()));
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("new-ssh-pane", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| ssh_host_dialog(&ctx));
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("shortcuts", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| shortcuts::show(&ctx));
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("about", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| show_about(&ctx));
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

    let action = gio::SimpleAction::new("set-folder", Some(glib::VariantTy::STRING));
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(name) = parameter.and_then(glib::Variant::get::<String>) {
                pick_folder(&ctx, &name);
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
    let open = gtk::Button::with_label("New Pane");
    open.add_css_class("pill");
    open.add_css_class("suggested-action");
    open.set_halign(gtk::Align::Center);
    open.set_action_name(Some("win.new-pane"));
    let empty = adw::StatusPage::builder()
        .icon_name("utilities-terminal-symbolic")
        .title("Stashed and empty")
        .description("Press Ctrl+T to open a terminal")
        .child(&open)
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
    let for_drop = ctx.clone();
    let for_paste = ctx.clone();
    let for_files = ctx.clone();
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
        on_pane_drop: Rc::new(move |dragged, target| swap_panes(&for_drop, dragged, target)),
        on_paste: Rc::new(move |id| crate::dnd::paste_into(&for_paste, id)),
        on_file_drop: Rc::new(move |id, files| crate::dnd::files_dropped(&for_files, id, files)),
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
    ctx.content.set_visible_child(&stack);
    sync_sidebar(ctx);
    save(ctx);
    focus_active_pane(ctx);
}

/// Mirror state into the sidebar and the header title (the active
/// workflow's name over its folder).
fn sync_sidebar(ctx: &Rc<Ctx>) {
    let workflows = ctx.state.borrow().workflows.clone();
    let active = ctx.active.borrow().clone();
    ctx.sidebar.refresh(&workflows, &active);
    if let Some(workflow) = workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(&active))
    {
        ctx.title.set_title(&workflow.name);
        ctx.title
            .set_subtitle(&sidebar::display_dir(&workflow.default_dir));
    }
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

/// The focused pane's root overlay (every pane is a `gtk::Overlay`,
/// see pane.rs) and its terminal — for the voice pill and for feeding
/// the transcript. Same focused-or-first rule as [`focused_terminal`].
pub(crate) fn focused_pane(ctx: &Rc<Ctx>) -> Option<(gtk::Overlay, vte4::Terminal)> {
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
        })
        .and_then(|pane| {
            let overlay = pane.root.clone().downcast::<gtk::Overlay>().ok()?;
            Some((overlay, pane.terminal.clone()))
        })
}

/// The focused pane's id, wherever `Ctrl+Shift+V` should land — same
/// focused-or-first rule as [`focused_terminal`].
pub(crate) fn focused_pane_id(ctx: &Rc<Ctx>) -> Option<String> {
    let views = ctx.views.borrow();
    let active = ctx.active.borrow();
    views
        .iter()
        .find(|view| view.name.eq_ignore_ascii_case(&active))
        .and_then(|view| {
            view.focused
                .clone()
                .or_else(|| view.panes.first().map(|pane| pane.id.clone()))
        })
}

/// A pane's terminal and, for SSH panes, its host — for typing file
/// paths into it (dnd.rs). The host comes from state: the `Pane`
/// widget deliberately keeps no connection details.
pub(crate) fn pane_connection(ctx: &Rc<Ctx>, id: &str) -> Option<(vte4::Terminal, Option<String>)> {
    let terminal = {
        let views = ctx.views.borrow();
        views
            .iter()
            .flat_map(|view| view.panes.iter())
            .find(|pane| pane.id == id)
            .map(|pane| pane.terminal.clone())
    }?;
    let host = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .flat_map(|workflow| workflow.panes.iter())
        .find(|spec| spec.id == id)
        .and_then(|spec| match &spec.kind {
            PaneKind::Ssh { host, .. } => Some(host.clone()),
            PaneKind::Local => None,
        });
    Some((terminal, host))
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

/// "About Stashee" from the primary menu.
fn show_about(ctx: &Rc<Ctx>) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Stashee")
        .application_icon("dev.stashee.Terminal")
        .developer_name("Stashee contributors")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/eeegoloauq/stashee-terminal")
        .issue_url("https://github.com/eeegoloauq/stashee-terminal/issues")
        .license_type(gtk::License::MitX11)
        .comments("A tiling terminal workspace — shells never die by accident.")
        .build();
    dialog.present(Some(&ctx.toasts));
}

fn save(ctx: &Rc<Ctx>) {
    capture_last_dirs(ctx);
    if let Err(err) = ctx.state.borrow().save(&paths::state_file()) {
        tracing::error!("saving state failed: {err:#}");
        toast(ctx, "Could not save state");
    }
}

/// Ask tmux where every stashed pane's shell currently is and remember
/// it in state. tmux swallows OSC 7, so this — not the VTE signal — is
/// how stashed local panes get their respawn directory after a reboot;
/// running on every save keeps the state file current without timers,
/// since a pane's directory can only change while the app is open.
fn capture_last_dirs(ctx: &Rc<Ctx>) {
    let argv = tmux::list_pane_dirs_argv();
    let output = match Command::new(&argv[0]).args(&argv[1..]).output() {
        // a failure exit is normal: no server means no live panes
        Ok(output) if output.status.success() => output,
        Ok(_) => return,
        Err(err) => {
            tracing::debug!("cannot list pane directories: {err}");
            return;
        }
    };
    let dirs = tmux::parse_pane_dirs(&String::from_utf8_lossy(&output.stdout));
    if dirs.is_empty() {
        return;
    }
    let mut state = ctx.state.borrow_mut();
    for workflow in &mut state.workflows {
        let name = workflow.name.clone();
        for spec in &mut workflow.panes {
            if spec.kind == PaneKind::Local
                && let Some(dir) = dirs.get(&tmux::session_name(&name, &spec.id))
            {
                spec.last_dir = Some(dir.clone());
            }
        }
    }
}

pub(crate) fn toast(ctx: &Rc<Ctx>, message: &str) {
    // Toast titles are Pango markup, and messages carry arbitrary user
    // strings — key bindings ("<Ctrl>t"), workflow names.
    ctx.toasts
        .add_toast(adw::Toast::new(glib::markup_escape_text(message).as_str()));
}
