//! Data types shared by state, tmux naming, and the frontends.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One terminal in a workflow's grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Short random suffix, stable across restarts; part of the tmux
    /// session name (see [`crate::tmux::session_name`]).
    pub id: String,
    pub kind: PaneKind,
    /// Last known working directory (OSC 7). Respawn directory for
    /// panes of non-stashed workflows and for stashed panes recreated
    /// after a reboot; informational otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneKind {
    Local,
    Ssh { host: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    /// Unique, compared case-insensitively.
    pub name: String,
    /// Where `Ctrl+T` opens new panes.
    pub default_dir: PathBuf,
    /// When true (the default), every pane runs inside a tmux session
    /// and survives the app; when false, panes are plain shells.
    #[serde(default = "default_stash")]
    pub stash: bool,
    #[serde(default)]
    pub panes: Vec<PaneSpec>,
}

fn default_stash() -> bool {
    true
}

impl PaneSpec {
    #[must_use]
    pub fn new_local() -> Self {
        Self {
            id: new_pane_id(),
            kind: PaneKind::Local,
            last_dir: None,
        }
    }

    #[must_use]
    pub fn new_ssh(host: impl Into<String>) -> Self {
        Self {
            id: new_pane_id(),
            kind: PaneKind::Ssh { host: host.into() },
            last_dir: None,
        }
    }
}

impl Workflow {
    #[must_use]
    pub fn new(name: impl Into<String>, default_dir: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            default_dir: default_dir.into(),
            stash: true,
            panes: Vec::new(),
        }
    }

    /// Whether `spec` runs inside tmux. A pane's mode is never stored:
    /// a local pane is stashed if its workflow stashes *or* its session
    /// is already alive — so a pane created before stashing was turned
    /// off keeps its mode across the toggle and across restarts, and is
    /// never orphaned. SSH panes always target the remote's tmux.
    #[must_use]
    pub fn pane_stashed(&self, spec: &PaneSpec, live_sessions: &[String]) -> bool {
        match spec.kind {
            PaneKind::Ssh { .. } => true,
            PaneKind::Local => {
                self.stash
                    || live_sessions
                        .iter()
                        .any(|live| *live == crate::tmux::session_name(&self.name, &spec.id))
            }
        }
    }
}

/// Six lowercase base-36 characters — unique enough within a workflow,
/// short enough to stay readable in `tmux ls`.
#[must_use]
pub fn new_pane_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..6)
        .map(|_| ALPHABET[fastrand::usize(..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_ids_are_six_base36_chars() {
        for _ in 0..100 {
            let id = new_pane_id();
            assert_eq!(id.len(), 6);
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            );
        }
    }

    #[test]
    fn workflow_defaults_to_stashed() {
        assert!(Workflow::new("work", "/home/e").stash);
    }

    #[test]
    fn panes_of_a_stashed_workflow_are_stashed() {
        let wf = Workflow::new("work", "/home/e");
        assert!(wf.pane_stashed(&PaneSpec::new_local(), &[]));
    }

    #[test]
    fn dead_panes_of_a_non_stashed_workflow_are_plain() {
        let mut wf = Workflow::new("work", "/home/e");
        wf.stash = false;
        assert!(!wf.pane_stashed(&PaneSpec::new_local(), &[]));
    }

    #[test]
    fn a_live_session_keeps_its_pane_stashed_after_the_toggle_flips() {
        let mut wf = Workflow::new("work", "/home/e");
        wf.stash = false;
        let spec = PaneSpec::new_local();
        let live = vec![format!("stashee-work-{}", spec.id)];
        assert!(wf.pane_stashed(&spec, &live));
    }

    #[test]
    fn ssh_panes_are_always_stashed() {
        let mut wf = Workflow::new("work", "/home/e");
        wf.stash = false;
        assert!(wf.pane_stashed(&PaneSpec::new_ssh("e@server"), &[]));
    }

    #[test]
    fn stash_defaults_to_true_when_absent_in_toml() {
        let wf: Workflow = toml::from_str("name = \"work\"\ndefault_dir = \"/home/e\"\n")
            .unwrap_or_else(|e| panic!("parse: {e}"));
        assert!(wf.stash);
        assert!(wf.panes.is_empty());
    }
}
