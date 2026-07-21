//! XDG locations and the bundled tmux config. Path discovery lives in
//! the frontend on purpose — `stashee-core` takes paths as arguments.

use std::fs;
use std::path::PathBuf;

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

/// Where pasted clipboard images land before their path is typed into
/// a pane (dnd.rs). Runtime dir: the session's tmpfs, so leftovers
/// vanish at logout and never earlier.
pub fn paste_dir() -> PathBuf {
    glib::user_runtime_dir().join("stashee").join("paste")
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
