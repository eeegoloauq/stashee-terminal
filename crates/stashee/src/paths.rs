//! XDG locations and the bundled tmux config. Path discovery lives in
//! the frontend on purpose — `stashee-core` takes paths as arguments.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gtk4::glib;

pub fn config_file() -> PathBuf {
    glib::user_config_dir().join("stashee").join("config.toml")
}

/// The regenerated full-template reference next to the real config
/// (`Config::ensure_reference`).
pub fn config_reference() -> PathBuf {
    glib::user_config_dir()
        .join("stashee")
        .join("config.toml.default")
}

pub fn state_file() -> PathBuf {
    glib::user_data_dir().join("stashee").join("state.toml")
}

/// Where downloaded voice models live (SPEC.md "Voice input").
#[cfg(feature = "stt")]
pub fn models_dir() -> PathBuf {
    glib::user_data_dir().join("stashee").join("models")
}

/// `$XDG_RUNTIME_DIR/stashee`, created or tightened to 0700. It holds
/// the clipboard socket and pasted images. `/run/user/<uid>` is
/// private already, but glib falls back to `~/.cache` when
/// `XDG_RUNTIME_DIR` is unset, and there the umask alone would decide
/// who else can write into the clipboard.
pub fn private_runtime_dir() -> Result<PathBuf> {
    let dir = glib::user_runtime_dir().join("stashee");
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Fails on a directory someone else owns: chmod is refused.
fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // an existing directory keeps its old mode until told otherwise
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {}", dir.display()))
}

/// Where pasted clipboard images land before their path is typed into
/// a pane (dnd.rs). Runtime dir: the session's tmpfs, so leftovers
/// vanish at logout and never earlier.
pub fn paste_dir() -> Result<PathBuf> {
    Ok(private_runtime_dir()?.join("paste"))
}

/// Unix socket where the running app receives OSC 52 copies from the
/// pane-side proxies (clipboard.rs serves it, proxy.rs connects).
pub fn clipboard_socket() -> PathBuf {
    glib::user_runtime_dir()
        .join("stashee")
        .join("clipboard.sock")
}

const TMUX_CONF: &str = include_str!("../data/tmux.conf");

/// tmux needs a real file path for `-f`, so the bundled config is
/// materialized into our data dir and refreshed whenever the bundled
/// content changes.
pub fn ensure_tmux_conf() -> Result<PathBuf> {
    let path = glib::user_data_dir().join("stashee").join("tmux.conf");
    if fs::read_to_string(&path).is_ok_and(|current| current == TMUX_CONF) {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, TMUX_CONF).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn private_dir_is_created_and_tightened_to_owner_only() {
        let root = std::env::temp_dir().join(format!("stashee-paths-{}", std::process::id()));
        let dir = root.join("stashee");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
        ensure_private_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(mode, 0o700);
    }
}
