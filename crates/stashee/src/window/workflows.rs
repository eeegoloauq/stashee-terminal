//! Workflow operations: create, rename, stash toggle, folder, delete,
//! and the name dialog they share.

use super::*;

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

pub(super) fn add_workflow_dialog(ctx: &Rc<Ctx>) {
    name_dialog(ctx, "New Workflow", "Create", "", |ctx, name| {
        create_workflow(ctx, name);
    });
}

pub(super) fn create_workflow(ctx: &Rc<Ctx>, name: &str) {
    if !name_is_free(ctx, name, "") {
        toast(ctx, "That name is already taken");
        return;
    }
    let mut workflow = Workflow::new(name, glib::home_dir());
    // A config template for this name declares the starting panes; an
    // empty or absent template means the usual single local shell.
    workflow.panes = ctx
        .config
        .borrow()
        .template_for(name)
        .map(|template| {
            template
                .panes
                .iter()
                .map(|pane| PaneSpec::from_template(pane, &glib::home_dir()))
                .collect()
        })
        .unwrap_or_default();
    if workflow.panes.is_empty() {
        workflow.panes.push(PaneSpec::new_local());
    }
    ctx.state.borrow_mut().workflows.push(workflow.clone());
    build_view(ctx, &workflow, &[]);
    switch_to(ctx, name);
}

pub(super) fn rename_workflow_dialog(ctx: &Rc<Ctx>, name: &str) {
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
                PaneKind::Ssh { host, .. } => run_detached(
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
        // live panes must learn the new session names: an SSH pane's
        // reconnect attaches by name, and a stale one would recreate
        // the old session on the remote
        for pane in &view.panes {
            *pane.session.borrow_mut() = tmux::session_name(new, &pane.id);
        }
    }
    if ctx.active.borrow().eq_ignore_ascii_case(old) {
        *ctx.active.borrow_mut() = new.to_owned();
        ctx.state.borrow_mut().last_active = Some(new.to_owned());
    }
    sync_sidebar(ctx);
    save(ctx);
}

/// Ids of `workflow`'s panes that run inside tmux, per the live view.
pub(super) fn stashed_pane_ids(ctx: &Rc<Ctx>, workflow: &str) -> Vec<String> {
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

pub(super) fn toggle_stash(ctx: &Rc<Ctx>, name: &str) {
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

pub(super) fn delete_workflow(ctx: &Rc<Ctx>, name: &str) {
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
            PaneKind::Ssh { host, .. } => {
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

/// "Set Folder…" from a workflow's context menu: pick the directory
/// new panes of that workflow open in.
pub(super) fn pick_folder(ctx: &Rc<Ctx>, name: &str) {
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
    let dialog = gtk::FileDialog::builder()
        .title(format!("Folder for “{name}”"))
        .modal(true)
        .build();
    dialog.set_initial_folder(Some(&gio::File::for_path(&workflow.default_dir)));
    let window = ctx.toasts.root().and_downcast::<gtk::Window>();
    let ctx = ctx.clone();
    let name = name.to_owned();
    dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
        // dismissing the dialog also lands here — only act on a pick
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        if let Some(workflow) = ctx
            .state
            .borrow_mut()
            .workflows
            .iter_mut()
            .find(|wf| wf.name.eq_ignore_ascii_case(&name))
        {
            workflow.default_dir = path;
        }
        sync_sidebar(&ctx);
        save(&ctx);
    });
}
