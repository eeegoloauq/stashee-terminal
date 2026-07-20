//! Persisted app state (`state.toml`) and startup reconciliation
//! against live tmux sessions. State is a hint; tmux is the truth for
//! local panes of stashed workflows (see ARCHITECTURE.md).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{PaneKind, PaneSpec, Workflow};
use crate::tmux;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<String>,
    /// Most-recent-first suggestions for the "+ SSH" host prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_hosts: Vec<String>,
    /// Kernel boot id at the last save. A mismatch at startup means
    /// the machine rebooted, so dead local sessions are respawned as
    /// fresh shells instead of dropped (see [`State::reconcile`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

/// The kernel's boot id — a UUID regenerated on every boot. `None`
/// when `/proc` is unavailable, which just disables reboot detection.
#[must_use]
pub fn current_boot_id() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

/// Suggestions beyond this age would just pad the dialog.
const RECENT_HOSTS_KEPT: usize = 8;

impl State {
    /// Missing file → default state. A corrupt file is an error: the
    /// frontend backs it up as `state.toml.bak` and starts fresh —
    /// never guess at a user's workflows.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "no state file, starting fresh");
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Write a sibling temp file, then rename over `path`, so a crash
    /// mid-save never leaves a truncated state file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing state")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Move `host` to the front of the recent-hosts suggestions.
    pub fn remember_host(&mut self, host: &str) {
        self.recent_hosts.retain(|known| known != host);
        self.recent_hosts.insert(0, host.to_owned());
        self.recent_hosts.truncate(RECENT_HOSTS_KEPT);
    }

    /// Drop `host` from the recent-hosts suggestions.
    pub fn forget_host(&mut self, host: &str) {
        self.recent_hosts.retain(|known| known != host);
    }

    /// Reconcile saved workflows with the live sessions on our socket
    /// (the "Startup reconciliation" algorithm in ARCHITECTURE.md):
    /// stashed workflows drop local panes whose session died; live
    /// sessions nobody knows are adopted, creating their workflow
    /// (rooted at `new_workflow_dir`) if needed; SSH panes and
    /// non-stashed workflows keep their specs untouched.
    ///
    /// Exception: when `boot_id` differs from the one saved, the
    /// machine rebooted — every session died with it, and a dead
    /// session says nothing about user intent. Dead local panes are
    /// kept, and attaching (`new-session -A`) respawns each one as a
    /// fresh shell in its remembered directory.
    pub fn reconcile(
        &mut self,
        live_sessions: &[String],
        new_workflow_dir: &Path,
        boot_id: Option<&str>,
    ) {
        let live: Vec<(&str, &str)> = live_sessions
            .iter()
            .filter_map(|s| tmux::parse_session_name(s))
            .collect();
        // Only a *known* mismatch is a reboot; an absent id on either
        // side falls back to "tmux is the truth".
        let rebooted = match (self.boot_id.as_deref(), boot_id) {
            (Some(saved), Some(current)) => saved != current,
            _ => false,
        };
        if let Some(current) = boot_id {
            self.boot_id = Some(current.to_owned());
        }

        for wf in &mut self.workflows {
            if !wf.stash {
                continue;
            }
            let slug = tmux::sanitize(&wf.name);
            wf.panes.retain(|pane| match pane.kind {
                PaneKind::Local => {
                    rebooted || live.iter().any(|(s, id)| *s == slug && *id == pane.id)
                }
                PaneKind::Ssh { .. } => true,
            });
        }

        for (slug, id) in live {
            let known = self
                .workflows
                .iter()
                .any(|wf| tmux::sanitize(&wf.name) == slug && wf.panes.iter().any(|p| p.id == id));
            if known {
                continue;
            }
            tracing::debug!(slug, id, "adopting session created outside the app");
            let adopted = PaneSpec {
                id: id.to_owned(),
                kind: PaneKind::Local,
                last_dir: None,
                run: None,
            };
            match self
                .workflows
                .iter_mut()
                .find(|wf| tmux::sanitize(&wf.name) == slug)
            {
                Some(wf) => wf.panes.push(adopted),
                None => {
                    let mut wf = Workflow::new(slug, new_workflow_dir);
                    wf.panes.push(adopted);
                    self.workflows.push(wf);
                }
            }
        }

        if let Some(last) = &self.last_active
            && !self
                .workflows
                .iter()
                .any(|wf| wf.name.eq_ignore_ascii_case(last))
        {
            self.last_active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn pane(id: &str, kind: PaneKind) -> PaneSpec {
        PaneSpec {
            id: id.into(),
            kind,
            last_dir: None,
            run: None,
        }
    }

    fn workflow(name: &str, stash: bool, panes: Vec<PaneSpec>) -> Workflow {
        Workflow {
            name: name.into(),
            default_dir: "/home/e".into(),
            stash,
            panes,
        }
    }

    fn live(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn dead_local_panes_are_dropped_for_stashed_workflows() {
        let mut state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![
                    pane("aaaaaa", PaneKind::Local),
                    pane("bbbbbb", PaneKind::Local),
                ],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(&live(&["stashee-work-aaaaaa"]), Path::new("/home/e"), None);
        let ids: Vec<&str> = state.workflows[0]
            .panes
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["aaaaaa"]);
    }

    #[test]
    fn ssh_panes_survive_reconcile() {
        let mut state = State {
            workflows: vec![workflow(
                "srv",
                true,
                vec![pane(
                    "cccccc",
                    PaneKind::Ssh {
                        host: "e@server".into(),
                        cwd: None,
                    },
                )],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), None);
        assert_eq!(state.workflows[0].panes.len(), 1);
    }

    #[test]
    fn non_stashed_workflows_keep_their_panes() {
        let mut state = State {
            workflows: vec![workflow(
                "scratch",
                false,
                vec![pane("dddddd", PaneKind::Local)],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), None);
        assert_eq!(state.workflows[0].panes.len(), 1);
    }

    #[test]
    fn unknown_sessions_are_adopted_into_the_matching_workflow() {
        let mut state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![pane("aaaaaa", PaneKind::Local)],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(
            &live(&["stashee-work-aaaaaa", "stashee-work-eeeeee"]),
            Path::new("/home/e"),
            None,
        );
        let ids: Vec<&str> = state.workflows[0]
            .panes
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["aaaaaa", "eeeeee"], "adopted pane appends at the end");
    }

    #[test]
    fn unknown_slug_creates_a_workflow() {
        let mut state = State::default();
        state.reconcile(
            &live(&["stashee-deploy-ffffff"]),
            Path::new("/home/e"),
            None,
        );
        assert_eq!(state.workflows.len(), 1);
        assert_eq!(state.workflows[0].name, "deploy");
        assert_eq!(state.workflows[0].default_dir, Path::new("/home/e"));
        assert!(state.workflows[0].stash);
    }

    #[test]
    fn reboot_keeps_dead_local_panes() {
        let mut state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![
                    pane("aaaaaa", PaneKind::Local),
                    pane("bbbbbb", PaneKind::Local),
                ],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: Some("boot-1".into()),
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), Some("boot-2"));
        assert_eq!(state.workflows[0].panes.len(), 2);
        assert_eq!(state.boot_id.as_deref(), Some("boot-2"));
    }

    #[test]
    fn same_boot_still_drops_dead_local_panes() {
        let mut state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![pane("aaaaaa", PaneKind::Local)],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: Some("boot-1".into()),
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), Some("boot-1"));
        assert!(state.workflows[0].panes.is_empty());
    }

    #[test]
    fn unknown_saved_boot_id_is_not_a_reboot() {
        let mut state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![pane("aaaaaa", PaneKind::Local)],
            )],
            last_active: None,
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), Some("boot-1"));
        assert!(state.workflows[0].panes.is_empty());
        assert_eq!(state.boot_id.as_deref(), Some("boot-1"));
    }

    #[test]
    fn last_active_is_cleared_when_its_workflow_is_gone() {
        let mut state = State {
            workflows: vec![workflow("work", true, Vec::new())],
            last_active: Some("gone".into()),
            recent_hosts: Vec::new(),
            boot_id: None,
        };
        state.reconcile(&live(&[]), Path::new("/home/e"), None);
        assert_eq!(state.last_active, None);
    }

    #[test]
    fn remembered_hosts_are_deduped_most_recent_first() {
        let mut state = State::default();
        state.remember_host("e@alpha");
        state.remember_host("e@beta");
        state.remember_host("e@alpha");
        assert_eq!(state.recent_hosts, ["e@alpha", "e@beta"]);
    }

    #[test]
    fn remembered_hosts_are_capped() {
        let mut state = State::default();
        for n in 0..20 {
            state.remember_host(&format!("host-{n}"));
        }
        assert_eq!(state.recent_hosts.len(), RECENT_HOSTS_KEPT);
        assert_eq!(state.recent_hosts[0], "host-19");
    }

    #[test]
    fn forgotten_hosts_leave_the_suggestions() {
        let mut state = State::default();
        state.remember_host("e@alpha");
        state.remember_host("e@beta");
        state.forget_host("e@alpha");
        assert_eq!(state.recent_hosts, ["e@beta"]);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.toml");
        let state = State {
            workflows: vec![workflow(
                "work",
                true,
                vec![
                    pane("aaaaaa", PaneKind::Local),
                    pane(
                        "cccccc",
                        PaneKind::Ssh {
                            host: "e@server".into(),
                            cwd: Some("/opt/app".into()),
                        },
                    ),
                ],
            )],
            last_active: Some("work".into()),
            recent_hosts: vec!["e@server".into()],
            boot_id: Some("f81d4fae-7dec-11d0-a765-00a0c91e6bf6".into()),
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn loading_a_missing_file_gives_the_default() {
        assert_eq!(
            State::load(Path::new("/nonexistent/state.toml")).unwrap(),
            State::default()
        );
    }

    #[test]
    fn loading_a_corrupt_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        fs::write(&path, "not = [valid").unwrap();
        assert!(State::load(&path).is_err());
    }
}
