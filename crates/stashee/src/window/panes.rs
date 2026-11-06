//! Pane operations on the window: add, remove, close (with the busy
//! check), swap and move between workflows, and the SSH host dialog.

use super::*;
use crate::close_guard::{self, Activity, Probe};

/// Alt+drag dropped one pane onto another: swap their grid positions
/// (the tiling convention — tmux `swap-pane`, i3 swap — and the only
/// fully predictable move in an order-based grid). Order lives in two
/// mirrors, state and the view; both swap, then the grid animates.
pub(super) fn swap_panes(ctx: &Rc<Ctx>, dragged: &str, target: &str) {
    if dragged == target {
        return;
    }
    let active = ctx.active.borrow().clone();
    {
        let mut state = ctx.state.borrow_mut();
        let Some(workflow) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(&active))
        else {
            return;
        };
        let (Some(from), Some(to)) = (
            workflow.panes.iter().position(|pane| pane.id == dragged),
            workflow.panes.iter().position(|pane| pane.id == target),
        ) else {
            return;
        };
        workflow.panes.swap(from, to);
    }
    if let Some(index) = view_index(ctx, &active) {
        {
            let mut views = ctx.views.borrow_mut();
            if let Some(view) = views.get_mut(index)
                && let (Some(from), Some(to)) = (
                    view.panes.iter().position(|pane| pane.id == dragged),
                    view.panes.iter().position(|pane| pane.id == target),
                )
            {
                view.panes.swap(from, to);
            }
        }
        refresh_view(ctx, index);
    }
    save(ctx);
}

/// Alt+drag dropped a pane onto a sidebar workflow: the pane moves
/// there live — the widget (and its running client) is reparented, and
/// the tmux session is renamed so the new workflow owns it across
/// restarts. The same session-follows-name rule as workflow rename.
pub(super) fn move_pane_to_workflow(ctx: &Rc<Ctx>, id: &str, target: &str) {
    let source = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .find(|wf| wf.panes.iter().any(|pane| pane.id == id))
        .map(|wf| wf.name.clone());
    let Some(source) = source else {
        return;
    };
    if source.eq_ignore_ascii_case(target) {
        return;
    }
    let spec = ctx
        .state
        .borrow()
        .workflows
        .iter()
        .flat_map(|wf| wf.panes.iter())
        .find(|spec| spec.id == id)
        .cloned();
    let Some(spec) = spec else {
        return;
    };

    let from = tmux::session_name(&source, id);
    let to = tmux::session_name(target, id);
    if from != to {
        let stashed = stashed_pane_ids(ctx, &source).contains(&spec.id);
        match &spec.kind {
            PaneKind::Local if stashed => {
                // synchronous like workflow rename: local tmux answers
                // instantly and a failed rename must not pass silently
                let argv = tmux::rename_session_argv(&from, &to);
                match Command::new(&argv[0]).args(&argv[1..]).output() {
                    Ok(out) if out.status.success() => {}
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        tracing::error!("moving {from}: {}", stderr.trim());
                        toast(ctx, "Could not rename the pane's tmux session");
                        return;
                    }
                    Err(err) => {
                        tracing::error!("moving {from}: {err}");
                        toast(ctx, "Could not rename the pane's tmux session");
                        return;
                    }
                }
            }
            PaneKind::Local => {} // plain shell: no session to rename
            PaneKind::Ssh { host, .. } => run_detached(
                ctx,
                ssh::rename_remote_argv(host, &from, &to),
                "Could not rename the remote session",
            ),
        }
    }

    {
        let mut state = ctx.state.borrow_mut();
        for wf in &mut state.workflows {
            wf.panes.retain(|pane| pane.id != id);
        }
        if let Some(wf) = state
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(target))
        {
            wf.panes.push(spec);
        }
    }

    let (Some(src), Some(dst)) = (view_index(ctx, &source), view_index(ctx, target)) else {
        save(ctx);
        return;
    };
    let moved = {
        let mut views = ctx.views.borrow_mut();
        let Some(view) = views.get_mut(src) else {
            return;
        };
        let Some(pos) = view.panes.iter().position(|pane| pane.id == id) else {
            return;
        };
        if view.focused.as_deref() == Some(id) {
            view.focused = None;
        }
        let pane = view.panes.remove(pos);
        *pane.session.borrow_mut() = to;
        pane
    };
    // source first: its grid unparents the widget, then the target's
    // grid adopts it — the pane (and its running client) never respawns
    refresh_view(ctx, src);
    if let Some(view) = ctx.views.borrow_mut().get_mut(dst) {
        view.panes.push(moved);
    }
    refresh_view(ctx, dst);
    save(ctx);
    focus_active_pane(ctx);
    toast(ctx, &format!("Moved to “{target}”"));
}

/// OSC 7 from a pane: remember the directory it is in, so a plain-shell
/// pane respawns there on the next launch (SPEC.md "Workflows").
pub(super) fn set_last_dir(ctx: &Rc<Ctx>, id: &str, dir: PathBuf) {
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

/// `Ctrl+T` / the header "+" click: the new pane inherits the focused
/// pane's connection — an SSH pane focused means a new pane on the
/// same host (and its declared start directory, but never its `run`
/// command); a local pane, or an empty workflow, means a local shell.
/// The "+" menu's New Local Pane is the explicit local escape hatch.
pub(super) fn new_pane(ctx: &Rc<Ctx>) {
    match focused_spec(ctx).map(|spec| spec.kind) {
        Some(PaneKind::Ssh { host, cwd }) => {
            ctx.state.borrow_mut().remember_host(&host);
            let mut spec = PaneSpec::new_ssh(host);
            if let PaneKind::Ssh { cwd: slot, .. } = &mut spec.kind {
                *slot = cwd;
            }
            add_pane(ctx, spec);
        }
        _ => add_pane(ctx, PaneSpec::new_local()),
    }
}

/// The spec of the active workflow's focused pane (or its first, when
/// nothing was focused yet — the same fallback the copy shortcut uses).
fn focused_spec(ctx: &Rc<Ctx>) -> Option<PaneSpec> {
    let id = {
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
    }?;
    current_workflow(ctx)?
        .panes
        .into_iter()
        .find(|spec| spec.id == id)
}

pub(super) fn new_ssh_pane(ctx: &Rc<Ctx>, host: &str) {
    ctx.state.borrow_mut().remember_host(host);
    add_pane(ctx, PaneSpec::new_ssh(host));
}

/// Append `spec` to the active workflow and attach its pane.
pub(super) fn add_pane(ctx: &Rc<Ctx>, spec: PaneSpec) {
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

/// The "New SSH Pane" prompt: a host entry with recent-hosts suggestions
/// (SPEC.md "SSH panes"). Activating a suggestion connects right away.
pub(super) fn ssh_host_dialog(ctx: &Rc<Ctx>) {
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
        // Rows are removable, so activation and removal share one
        // mutable list that stays aligned with the rows' indices.
        let hosts = Rc::new(RefCell::new(recent));
        for host in hosts.borrow().iter() {
            list.append(&host_row(ctx, host, &hosts, &list));
        }
        {
            let ctx = ctx.clone();
            let dialog = dialog.clone();
            let hosts = hosts.clone();
            list.connect_row_activated(move |_, row| {
                let host = hosts.borrow().get(row.index().max(0) as usize).cloned();
                if let Some(host) = host {
                    dialog.close();
                    new_ssh_pane(&ctx, &host);
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

/// One recent-host suggestion: the host label and a trash button.
/// Removing drops the host from the suggestions (state and dialog
/// alike); the button owns its clicks, so the rest of the row still
/// activates to connect.
fn host_row(
    ctx: &Rc<Ctx>,
    host: &str,
    hosts: &Rc<RefCell<Vec<String>>>,
    list: &gtk::ListBox,
) -> gtk::ListBoxRow {
    let label = gtk::Label::new(Some(host));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_tooltip_text(Some("Remove"));
    remove.set_valign(gtk::Align::Center);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.set_margin_start(12);
    content.set_margin_end(6);
    content.set_margin_top(2);
    content.set_margin_bottom(2);
    content.append(&label);
    content.append(&remove);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&content));
    {
        let ctx = ctx.clone();
        let hosts = hosts.clone();
        let list = list.downgrade();
        let row = row.downgrade();
        let host = host.to_owned();
        remove.connect_clicked(move |_| {
            let Some(list) = list.upgrade() else {
                return;
            };
            if let Some(row) = row.upgrade() {
                list.remove(&row);
            }
            hosts.borrow_mut().retain(|known| known != &host);
            list.set_visible(!hosts.borrow().is_empty());
            ctx.state.borrow_mut().forget_host(&host);
            save(&ctx);
        });
    }
    row
}

pub(super) fn remove_pane(ctx: &Rc<Ctx>, id: &str) {
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

/// The close key: closes an idle shell at once, asks first when the
/// pane runs something or cannot be checked (close_guard.rs).
pub(super) fn close_focused_pane(ctx: &Rc<Ctx>) {
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
    let Some(closing) = closing(ctx, &id) else {
        return;
    };
    let probe = closing.probe();
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let activity = gio::spawn_blocking(move || close_guard::probe(probe))
            .await
            .unwrap_or(Activity::Unknown);
        match activity {
            Activity::Idle => close_pane(&ctx, &id),
            other => confirm_close(&ctx, &id, &other),
        }
    });
}

fn confirm_close(ctx: &Rc<Ctx>, id: &str, activity: &Activity) {
    let body = match activity {
        Activity::Busy(command) => {
            format!("“{command}” is still running. Closing the pane ends it.")
        }
        _ => "Stashee could not check whether something is running in it. \
              Closing the pane ends anything that is."
            .to_owned(),
    };
    let dialog = adw::AlertDialog::new(Some("Close this pane?"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("close", "Close Pane");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    {
        let ctx = ctx.clone();
        let id = id.to_owned();
        dialog.connect_response(Some("close"), move |_, _| close_pane(&ctx, &id));
    }
    dialog.present(Some(&ctx.toasts));
}

/// What closing a pane involves, read fresh each time: a confirmation
/// may have stayed open while the pane reconnected or fell back.
struct Closing {
    kind: PaneKind,
    session: String,
    /// The pane knows its own mode; the workflow's stash toggle may
    /// have flipped since it was built.
    stashed: bool,
    /// An SSH pane on a plain connection (no tmux on the host): no
    /// remote session to kill or ask.
    ssh_fallback: bool,
    /// An SSH pane between connections: no live client.
    reconnecting: bool,
    pid: Option<i32>,
}

impl Closing {
    fn probe(&self) -> Probe {
        match &self.kind {
            PaneKind::Local if self.stashed => {
                Probe::Tmux(tmux::current_command_argv(&self.session))
            }
            PaneKind::Local => self.pid.map_or(Probe::Blind, Probe::Process),
            PaneKind::Ssh { .. } if self.ssh_fallback || self.reconnecting => Probe::Blind,
            PaneKind::Ssh { host, .. } => {
                Probe::Tmux(ssh::remote_current_command_argv(host, &self.session))
            }
        }
    }
}

fn closing(ctx: &Rc<Ctx>, id: &str) -> Option<Closing> {
    let views = ctx.views.borrow();
    let (view, pane) = views.iter().find_map(|view| {
        view.panes
            .iter()
            .find(|pane| pane.id == id)
            .map(|pane| (view, pane))
    })?;
    let state = ctx.state.borrow();
    let spec = state
        .workflows
        .iter()
        .find(|wf| wf.name.eq_ignore_ascii_case(&view.name))?
        .panes
        .iter()
        .find(|spec| spec.id == id)?;
    Some(Closing {
        kind: spec.kind.clone(),
        session: pane.session.borrow().clone(),
        stashed: pane.stashed,
        ssh_fallback: pane.ssh_fallback.get(),
        reconnecting: pane.reconnecting.get(),
        pid: pane.child_pid.get().map(|pid| pid.0),
    })
}

fn close_pane(ctx: &Rc<Ctx>, id: &str) {
    let Some(closing) = closing(ctx, id) else {
        return; // already gone: its shell exited while we were asking
    };
    match (&closing.kind, closing.stashed) {
        // The killed session makes the pane's client exit, and
        // child-exited drives the UI update — one code path.
        (PaneKind::Local, true) => {
            run_detached(
                ctx,
                tmux::kill_session_argv(&closing.session),
                "Could not close the pane",
            );
        }
        // Dropping the widget ends the plain connection.
        (PaneKind::Ssh { .. }, _) if closing.ssh_fallback => remove_pane(ctx, id),
        // Mid-reconnect there is no live client whose exit could drive
        // the removal: kill the remote session best-effort and remove
        // the pane directly.
        (PaneKind::Ssh { host, .. }, _) if closing.reconnecting => {
            run_detached(
                ctx,
                ssh::kill_remote_argv(host, &closing.session),
                "Could not close the pane",
            );
            remove_pane(ctx, id);
        }
        (PaneKind::Ssh { host, .. }, _) => {
            run_detached(
                ctx,
                ssh::kill_remote_argv(host, &closing.session),
                "Could not close the pane",
            );
        }
        // no session to kill; remove the plain-shell pane directly
        (PaneKind::Local, false) => remove_pane(ctx, id),
    }
}

/// Fire and forget; the outcome, if any, arrives via child-exited.
pub(super) fn run_detached(ctx: &Rc<Ctx>, argv: Vec<String>, failure: &str) {
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
