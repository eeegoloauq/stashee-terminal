//! User preferences (`config.toml`). Hand-editable; every field has a
//! default, so a partial or missing file always works. The first run
//! materializes [`Config::TEMPLATE`] — a fully commented copy of the
//! defaults — so the file documents itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tmux;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub behavior: Behavior,
    pub keys: Keys,
    /// Workflow templates, keyed by workflow name. Applied once, when
    /// a workflow with a matching name (by tmux slug, so "My Proj" and
    /// "my-proj" collide here exactly as everywhere else) is created.
    pub workflows: BTreeMap<String, WorkflowTemplate>,
}

/// Declared panes for a workflow created by name — the config is the
/// declaration, `state.toml` stays the runtime truth. An existing
/// workflow is never reshaped by its template.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowTemplate {
    pub panes: Vec<PaneTemplate>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneTemplate {
    /// SSH destination (anything `ssh` accepts); absent = local pane.
    pub ssh: Option<String>,
    /// Working directory. Local panes expand a leading `~` against the
    /// local home; for SSH panes the path is resolved on the remote.
    pub cwd: Option<String>,
    /// Command executed when the pane's tmux *session* is created —
    /// never on reattach — with the shell kept after it exits.
    pub run: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Terminal surface opacity, clamped to `0.0..=1.0` on load.
    pub opacity: f64,
    /// Pango font description; empty means the system monospace font.
    pub font: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            opacity: 0.88,
            font: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Behavior {
    pub confirm_workflow_delete: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            confirm_workflow_delete: true,
        }
    }
}

/// Keybindings in GTK accelerator syntax (`"<Ctrl><Shift>t"`,
/// `"<Alt>Left"`). An empty string disables the binding. Validation
/// happens in the frontend — this crate never depends on GTK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub new_pane: String,
    pub new_ssh_pane: String,
    pub close_pane: String,
    pub focus_left: String,
    pub focus_right: String,
    pub focus_up: String,
    pub focus_down: String,
    pub copy: String,
    pub paste: String,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            new_pane: "<Ctrl>t".to_owned(),
            new_ssh_pane: "<Ctrl><Shift>t".to_owned(),
            close_pane: "<Ctrl>w".to_owned(),
            focus_left: "<Alt>Left".to_owned(),
            focus_right: "<Alt>Right".to_owned(),
            focus_up: "<Alt>Up".to_owned(),
            focus_down: "<Alt>Down".to_owned(),
            copy: "<Ctrl><Shift>c".to_owned(),
            paste: "<Ctrl><Shift>v".to_owned(),
        }
    }
}

impl Config {
    /// The self-documenting config written on first run: prose lines
    /// start with `## `, every default with `# ` — uncomment to
    /// override. `template_stays_true_to_defaults` keeps it honest.
    pub const TEMPLATE: &str = "\
## stashee configuration.
## Everything here is optional: the values below are the defaults,
## commented out — uncomment a line to override it. The running app
## picks up saved changes instantly. `stashee config` opens this file.

[appearance]
## Window backdrop opacity, 0.0 to 1.0.
# opacity = 0.88

## Terminal font, a Pango description like \"JetBrains Mono 12\".
## Empty picks the system monospace font.
# font = \"\"

[behavior]
## Ask for confirmation before deleting a workflow.
# confirm_workflow_delete = true

[keys]
## GTK accelerator syntax: modifiers in angle brackets, then the key —
## \"<Ctrl><Shift>t\", \"<Alt>Left\", \"F11\". Set a binding to \"\" to
## disable it. Alt+1..9 (switch to the n-th workflow) is fixed.
# new_pane = \"<Ctrl>t\"
# new_ssh_pane = \"<Ctrl><Shift>t\"
# close_pane = \"<Ctrl>w\"
# focus_left = \"<Alt>Left\"
# focus_right = \"<Alt>Right\"
# focus_up = \"<Alt>Up\"
# focus_down = \"<Alt>Down\"
# copy = \"<Ctrl><Shift>c\"
# paste = \"<Ctrl><Shift>v\"

## Workflow templates: declare the panes a workflow starts with. When
## a workflow with a matching name is created — `stashee myproj` or the
## New Workflow dialog — its panes come from here instead of a single
## default shell. `run` executes when a pane's tmux session is
## *created* (never on reattach — reopening the app returns to the
## running program), and the pane falls back to a shell when it exits.
## Omit `ssh` for a local pane; `cwd` is resolved on the remote for SSH
## panes (remote tmux >= 1.9 needed for `cwd` over SSH).
##
##   [[workflows.myproj.panes]]
##   ssh = \"dev\"                  # a Host alias from ~/.ssh/config
##   cwd = \"/opt/myproj\"
##   run = \"claude\"
##
##   [[workflows.myproj.panes]]    # second pane: plain remote shell
##   ssh = \"dev\"
##   cwd = \"/opt/myproj\"
";

    /// The template for `name`, if one is declared. Names collide by
    /// tmux slug — the app-wide uniqueness rule.
    #[must_use]
    pub fn template_for(&self, name: &str) -> Option<&WorkflowTemplate> {
        let slug = tmux::sanitize(name);
        self.workflows
            .iter()
            .find(|(declared, _)| tmux::sanitize(declared) == slug)
            .map(|(_, template)| template)
    }

    /// Missing file → defaults. An unparsable file is an error — never
    /// silently ignore a config the user edited by hand.
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            Self::default()
        };
        config.appearance.opacity = config.appearance.opacity.clamp(0.0, 1.0);
        Ok(config)
    }

    /// Write [`TEMPLATE`](Self::TEMPLATE) if no config exists yet. An
    /// existing file — whatever its content — is never touched.
    pub fn ensure(path: &Path) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, Self::TEMPLATE).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let config = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config, Config::default());
        assert!((config.appearance.opacity - 0.88).abs() < 1e-9);
        assert!(config.behavior.confirm_workflow_delete);
    }

    #[test]
    fn partial_file_fills_the_rest_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[appearance]\nopacity = 0.5\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!((config.appearance.opacity - 0.5).abs() < 1e-9);
        assert_eq!(config.appearance.font, "");
        assert!(config.behavior.confirm_workflow_delete);
        assert_eq!(config.keys, Keys::default());
    }

    #[test]
    fn out_of_range_opacity_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[appearance]\nopacity = 7.0\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!((config.appearance.opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn garbage_errors_instead_of_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "appearance = \"yes\"").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn keys_load_and_may_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[keys]\nnew_pane = \"<Ctrl>n\"\nclose_pane = \"\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.keys.new_pane, "<Ctrl>n");
        assert_eq!(config.keys.close_pane, "");
        assert_eq!(config.keys.copy, "<Ctrl><Shift>c");
    }

    #[test]
    fn template_parses_to_defaults_as_written() {
        let config: Config = toml::from_str(Config::TEMPLATE).unwrap();
        assert_eq!(config, Config::default());
    }

    /// Uncommenting every default in the template must reproduce
    /// `Config::default()` exactly — the file's documentation can
    /// never drift from the code.
    #[test]
    fn template_stays_true_to_defaults() {
        let uncommented: String = Config::TEMPLATE
            .lines()
            .filter(|line| !line.starts_with("##"))
            .map(|line| line.strip_prefix("# ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let config: Config = toml::from_str(&uncommented).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn workflow_templates_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[workflows.myproj.panes]]
ssh = "dev"
cwd = "/opt/myproj"
run = "claude"

[[workflows.myproj.panes]]
cwd = "~/notes"
"#,
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let template = config.template_for("myproj").unwrap();
        assert_eq!(template.panes.len(), 2);
        assert_eq!(template.panes[0].ssh.as_deref(), Some("dev"));
        assert_eq!(template.panes[0].cwd.as_deref(), Some("/opt/myproj"));
        assert_eq!(template.panes[0].run.as_deref(), Some("claude"));
        assert_eq!(template.panes[1].ssh, None);
        assert_eq!(template.panes[1].run, None);
    }

    #[test]
    fn templates_match_by_slug_like_workflow_names_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[[workflows.\"My Proj\".panes]]\nrun = \"claude\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!(config.template_for("my-proj").is_some());
        assert!(config.template_for("MY PROJ").is_some());
        assert!(config.template_for("other").is_none());
    }

    #[test]
    fn ensure_writes_once_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        Config::ensure(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), Config::TEMPLATE);
        fs::write(&path, "[appearance]\nopacity = 0.3\n").unwrap();
        Config::ensure(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[appearance]\nopacity = 0.3\n"
        );
    }
}
