//! App-side receiver for OSC 52 copies caught by the pane proxies.
//!
//! The clipboard is set here, through GDK, because this process owns
//! the window. A headless helper (`wl-copy` and friends) has to create
//! an invisible surface and wait for focus to claim the selection;
//! under GNOME's focus-stealing prevention that wait can hang forever
//! — the proxy's relay loop once blocked on it and froze the pane.
//! Instead each proxy hands its copies over a unix socket: one
//! connection per copy, the payload is the bytes until EOF.

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gdk::prelude::DisplayExt;
use gtk4::glib;

use stashee_core::osc52;

use crate::paths;

/// A proxy writes its payload and closes immediately; a peer that
/// stalls longer than this is broken and must not hold up the queue.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Bind the socket and serve copies for the life of the process. A
/// leftover socket from a crashed run is replaced — the D-Bus
/// single-instance guarantee means no other live listener exists.
/// On failure the app still runs; copies are lost and the loss is
/// logged.
pub fn serve() {
    let path = paths::clipboard_socket();
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::error!("clipboard socket dir failed, copies will not land: {err}");
        return;
    }
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("clipboard socket failed, copies will not land: {err}");
            return;
        }
    };
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            match connection {
                Ok(stream) => receive(stream),
                Err(err) => tracing::warn!("clipboard connection failed: {err}"),
            }
        }
    });
}

fn receive(stream: UnixStream) {
    if let Err(err) = stream.set_read_timeout(Some(READ_TIMEOUT)) {
        tracing::warn!("clipboard receive failed: {err}");
        return;
    }
    let mut payload = Vec::new();
    let limit = osc52::MAX_PAYLOAD as u64;
    if let Err(err) = stream.take(limit + 1).read_to_end(&mut payload) {
        tracing::warn!("clipboard receive failed: {err}");
        return;
    }
    if payload.is_empty() || payload.len() as u64 > limit {
        return; // an empty copy must not clobber the clipboard
    }
    // GDK objects live on the main thread; only the text crosses over.
    let text = String::from_utf8_lossy(&payload).into_owned();
    glib::MainContext::default().invoke(move || match gtk::gdk::Display::default() {
        Some(display) => display.clipboard().set_text(&text),
        None => tracing::warn!("no display; dropping an OSC 52 copy"),
    });
}
