//! Sessions follow their workflow's name: renaming a workflow or moving
//! a pane to another one renames the tmux sessions behind its panes.
//! Otherwise the next start would attach fresh sessions and orphan the
//! old ones.

use super::*;

/// One pane's session moving from `from` to `to`.
pub(super) struct SessionRename {
    pane_id: String,
    target: Target,
    from: String,
    to: String,
}

enum Target {
    /// A stashed local pane: our tmux socket.
    Local,
    /// An SSH pane attached to the host's tmux.
    Remote(String),
    /// A plain shell or a plain ssh connection: no session to rename.
    Nothing,
}

/// Plan the rename for one pane. The live pane decides, not the
/// workflow toggle: a pane built stashed keeps a session that must
/// follow, and an SSH pane that fell back to plain ssh has none.
pub(super) fn session_rename(
    ctx: &Rc<Ctx>,
    spec: &PaneSpec,
    from: String,
    to: String,
) -> SessionRename {
    let views = ctx.views.borrow();
    let pane = views
        .iter()
        .flat_map(|view| view.panes.iter())
        .find(|pane| pane.id == spec.id);
    let target = match (&spec.kind, pane) {
        (PaneKind::Local, Some(pane)) if pane.stashed => Target::Local,
        (PaneKind::Ssh { host, .. }, Some(pane)) if !pane.ssh_fallback.get() => {
            Target::Remote(host.clone())
        }
        _ => Target::Nothing,
    };
    SessionRename {
        pane_id: spec.id.clone(),
        target,
        from,
        to,
    }
}

/// Rename every session. Local ones go first and all-or-nothing: if
/// one fails, those already renamed are put back and the caller must
/// not go on (returns false; the failure is already reported). Remote
/// renames then run over ssh in the background. Live panes learn their
/// new session names, which an SSH reconnect attaches by — a stale name
/// would recreate the old session on the host.
pub(super) fn rename_sessions(ctx: &Rc<Ctx>, renames: &[SessionRename]) -> bool {
    let mut done: Vec<&SessionRename> = Vec::new();
    for rename in renames
        .iter()
        .filter(|rename| matches!(rename.target, Target::Local))
    {
        if let Err(err) = rename_local(&rename.from, &rename.to) {
            tracing::error!(
                "renaming tmux session {} to {}: {err:#}",
                rename.from,
                rename.to
            );
            for undo in done.iter().rev() {
                if let Err(err) = rename_local(&undo.to, &undo.from) {
                    tracing::error!("restoring tmux session {}: {err:#}", undo.from);
                }
            }
            toast(
                ctx,
                "Could not rename the tmux sessions, so nothing changed",
            );
            return false;
        }
        done.push(rename);
    }
    for rename in renames {
        if let Target::Remote(host) = &rename.target {
            run_detached(
                ctx,
                ssh::rename_remote_argv(host, &rename.from, &rename.to),
                "Could not rename a remote session",
            );
        }
        let views = ctx.views.borrow();
        if let Some(pane) = views
            .iter()
            .flat_map(|view| view.panes.iter())
            .find(|pane| pane.id == rename.pane_id)
        {
            pane.session.borrow_mut().clone_from(&rename.to);
        }
    }
    true
}

/// Synchronous: local tmux answers at once, and the caller needs the
/// outcome before it changes state.
fn rename_local(from: &str, to: &str) -> Result<()> {
    let argv = tmux::rename_session_argv(from, to);
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .context("running tmux")?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
