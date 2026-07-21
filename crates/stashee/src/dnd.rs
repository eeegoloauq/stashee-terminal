//! Files and images into panes: dropping files onto a pane and
//! pasting a clipboard image both end as a *path* typed at the pane's
//! prompt — no trailing newline, same contract as the voice
//! transcript: review, then Enter. The point is SSH panes: the file
//! is scp'd to the pane's host first, because this process is the
//! only party that knows both the local bytes and where the pane
//! points (SPEC.md "Files and images into panes").

use std::cell::Cell;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use vte4::prelude::*;

use stashee_core::{ssh, tmux};

use crate::paths;
use crate::window::{Ctx, focused_pane_id, pane_connection, toast};

/// `Ctrl+Shift+V`: smart paste into the focused pane.
pub(crate) fn paste(ctx: &Rc<Ctx>) {
    if let Some(id) = focused_pane_id(ctx) {
        paste_into(ctx, &id);
    }
}

/// Right-click / `Ctrl+Shift+V` on a known pane. Text always wins and
/// stays VTE's own paste — browsers offer text and image together, and
/// pasting the text is what every terminal would do. An image-*only*
/// clipboard (a fresh screenshot) becomes a PNG file whose path is
/// typed instead.
pub(crate) fn paste_into(ctx: &Rc<Ctx>, id: &str) {
    let Some((terminal, host)) = pane_connection(ctx, id) else {
        return;
    };
    let clipboard = terminal.clipboard();
    // External clipboards advertise mime types, not GTypes — a GNOME
    // screenshot is `image/png`, never a `GdkTexture` offer — so both
    // sides of the decision have to look at mimes too.
    let formats = clipboard.formats();
    let mimes = formats.mime_types();
    let has_text = formats.contains_type(glib::types::Type::STRING)
        || mimes
            .iter()
            .any(|mime| mime.starts_with("text/") || mime.as_str() == "UTF8_STRING");
    let has_image = formats.contains_type(gdk::Texture::static_type())
        || mimes.iter().any(|mime| mime.starts_with("image/"));
    if has_text || !has_image {
        terminal.paste_clipboard();
        return;
    }
    let ctx = ctx.clone();
    clipboard.read_texture_async(gtk::gio::Cancellable::NONE, move |result| {
        match result {
            Ok(Some(texture)) => match save_png(&texture) {
                Ok(path) => deliver(&ctx, &terminal, host.as_deref(), vec![path]),
                Err(err) => {
                    tracing::error!("saving the pasted image failed: {err:#}");
                    toast(&ctx, "Could not save the pasted image");
                }
            },
            // The offer disappeared between the check and the read
            // (clipboard owner quit): fall through to a plain paste.
            Ok(None) => terminal.paste_clipboard(),
            Err(err) => {
                tracing::warn!("clipboard image read failed: {err}");
                terminal.paste_clipboard();
            }
        }
    });
}

/// Files dropped onto a pane (pane.rs' file drop target). Local pane:
/// their paths are typed as-is. SSH pane: uploaded first, then the
/// remote paths are typed.
pub(crate) fn files_dropped(ctx: &Rc<Ctx>, id: &str, files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    let Some((terminal, host)) = pane_connection(ctx, id) else {
        return;
    };
    terminal.grab_focus();
    deliver(ctx, &terminal, host.as_deref(), files);
}

fn deliver(ctx: &Rc<Ctx>, terminal: &vte4::Terminal, host: Option<&str>, files: Vec<PathBuf>) {
    match host {
        None => feed_paths(
            terminal,
            files.iter().map(|path| path.display().to_string()),
        ),
        Some(host) => upload(ctx, terminal, host, files),
    }
}

/// scp the files to the pane's host, then type the remote paths.
/// Sequential on a worker thread; one failure drops the whole batch —
/// half a file list typed at a prompt is worse than a toast.
fn upload(ctx: &Rc<Ctx>, terminal: &vte4::Terminal, host: &str, files: Vec<PathBuf>) {
    let jobs: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|local| {
            let name = local
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let remote = format!(
                "/tmp/stashee-{}-{}",
                unique_suffix(),
                ssh::remote_file_name(&name)
            );
            (local, remote)
        })
        .collect();
    let host = host.to_owned();
    let terminal = glib::SendWeakRef::from(terminal.downgrade());
    let toasts = glib::SendWeakRef::from(ctx.toasts.downgrade());
    std::thread::spawn(move || {
        let mut failure = None;
        for (local, remote) in &jobs {
            let argv = ssh::upload_argv(&host, local, remote);
            match Command::new(&argv[0]).args(&argv[1..]).output() {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::error!("scp to {host} failed: {}", stderr.trim());
                    failure = Some(format!(
                        "Upload to {host} failed — scp needs key or agent auth"
                    ));
                    break;
                }
                Err(err) => {
                    tracing::error!("scp did not start: {err}");
                    failure = Some("Could not run scp".to_owned());
                    break;
                }
            }
        }
        let remotes: Vec<String> = jobs.iter().map(|(_, remote)| remote.clone()).collect();
        glib::MainContext::default().invoke(move || match failure {
            Some(message) => {
                if let Some(toasts) = toasts.upgrade() {
                    toasts.add_toast(adw::Toast::new(glib::markup_escape_text(&message).as_str()));
                }
            }
            None => match terminal.upgrade() {
                Some(terminal) => feed_paths(&terminal, remotes),
                None => tracing::info!("pane closed before the upload finished"),
            },
        });
    });
}

/// Type the paths, space-separated with a trailing space, straight
/// down the pty.
fn feed_paths<I: IntoIterator<Item = String>>(terminal: &vte4::Terminal, paths: I) {
    let mut line = String::new();
    for path in paths {
        line.push_str(&quote_for_prompt(&path));
        line.push(' ');
    }
    terminal.feed_child(line.as_bytes());
}

/// Shell-quote only when needed: a clean path stays bare, which also
/// suits non-shell prompts (a CLI reading a path from its own input).
/// Uploaded paths are always clean by construction (`remote_file_name`).
fn quote_for_prompt(path: &str) -> String {
    let clean = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '~' | '+'));
    if clean {
        path.to_owned()
    } else {
        tmux::shell_quote(path)
    }
}

/// Clipboard image → PNG in the runtime dir (the session's tmpfs, so
/// leftovers vanish at logout, never at a moment the path is in use).
fn save_png(texture: &gdk::Texture) -> anyhow::Result<PathBuf> {
    let dir = paths::paste_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("paste-{}.png", unique_suffix()));
    std::fs::write(&path, texture.save_to_png_bytes())?;
    Ok(path)
}

/// Unique within the login session: wall-clock seconds (the runtime
/// dir outlives app restarts) plus a per-run counter (several pastes
/// within one second).
fn unique_suffix() -> String {
    thread_local! {
        static SEQ: Cell<u32> = const { Cell::new(0) };
    }
    let seq = SEQ.with(|seq| {
        seq.set(seq.get().wrapping_add(1));
        seq.get()
    });
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format!("{secs:x}-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_paths_stay_bare() {
        assert_eq!(
            quote_for_prompt("/tmp/stashee-1a-1-shot.png"),
            "/tmp/stashee-1a-1-shot.png"
        );
    }

    #[test]
    fn shell_specials_get_quoted() {
        assert_eq!(
            quote_for_prompt("/home/e/Screenshot from 2026-07-21.png"),
            "'/home/e/Screenshot from 2026-07-21.png'"
        );
        assert_eq!(
            quote_for_prompt("/home/e/it's.png"),
            r"'/home/e/it'\''s.png'"
        );
    }
}
