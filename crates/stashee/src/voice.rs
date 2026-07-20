//! Voice input: the record toggle, the pill overlaid on the focused
//! pane, and the flow around it — first-use model download (with
//! consent), transcription in a worker, transcript fed to the pane's
//! pty. Capture and recognition live in `stashee-voice`; this module
//! is the GTK wiring. Built without the `stt` feature, recording
//! still works and stopping explains that recognition is absent.

// The no-stt build leaves the transcription-side widgets and imports
// dangling; it is an escape hatch for restricted build hosts, not a
// first-class configuration worth cfg-gating field by field.
#![cfg_attr(
    not(feature = "stt"),
    allow(dead_code, unused_imports, unused_variables, unused_mut)
)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;
use vte4::prelude::*;

use stashee_voice::Recorder;

use crate::window::{Ctx, focused_pane, toast};

/// Waveform bars kept on screen (~0.8 s of history).
const BARS: usize = 24;
/// UI refresh; also the level cadence (stashee-voice chunks ~30 ms).
const TICK: Duration = Duration::from_millis(33);
/// A toggle left on this long was forgotten, not dictated into.
const AUTO_STOP: Duration = Duration::from_secs(300);

thread_local! {
    /// Distinguishes a stale tick callback from the session that
    /// replaced it — sessions can be stopped and restarted between
    /// two fires of the old tick.
    static EPOCH: Cell<u64> = const { Cell::new(0) };
}

/// All voice state hanging off [`Ctx`]: the in-flight session, plus —
/// with `stt` — the resident transcription worker and a running model
/// download.
#[derive(Default)]
pub(crate) struct VoiceCtl {
    session: Option<Session>,
    #[cfg(feature = "stt")]
    transcriber: Option<stashee_voice::stt::Transcriber>,
    #[cfg(feature = "stt")]
    download: Option<stashee_voice::model::Download>,
}

enum Phase {
    Recording {
        recorder: Recorder,
        started: Instant,
    },
    #[cfg(feature = "stt")]
    Transcribing {
        reply: std::sync::mpsc::Receiver<anyhow::Result<String>>,
    },
}

struct Session {
    epoch: u64,
    phase: Phase,
    pill: gtk::Widget,
    /// Shown while recording; swapped for `busy` during transcription.
    live: gtk::Box,
    busy: gtk::Box,
    wave: gtk::DrawingArea,
    overlay: glib::WeakRef<gtk::Overlay>,
    terminal: glib::WeakRef<vte4::Terminal>,
    history: Rc<RefCell<VecDeque<f64>>>,
}

impl Session {
    fn teardown(&mut self) {
        remove_pill(&self.overlay, &self.pill);
    }
}

/// Field-level teardown, callable while `session.phase` is moved out.
fn remove_pill(overlay: &glib::WeakRef<gtk::Overlay>, pill: &gtk::Widget) {
    if let Some(overlay) = overlay.upgrade() {
        overlay.remove_overlay(pill);
    }
}

pub(crate) fn active(ctx: &Rc<Ctx>) -> bool {
    ctx.voice.borrow().session.is_some()
}

/// The voice key. In order: stop a running recording, explain a
/// disabled feature, report a running download, offer the model
/// download, or start recording.
pub(crate) fn toggle(ctx: &Rc<Ctx>) {
    let recording = matches!(
        ctx.voice.borrow().session,
        Some(Session {
            phase: Phase::Recording { .. },
            ..
        })
    );
    if recording {
        stop(ctx);
        return;
    }
    if active(ctx) {
        toast(ctx, "Still transcribing the previous recording");
        return;
    }
    if !ctx.config.borrow().voice.enabled {
        toast(
            ctx,
            "Voice input is off — set [voice] enabled = true in config.toml (experimental)",
        );
        return;
    }
    #[cfg(feature = "stt")]
    {
        if report_download(ctx) {
            return;
        }
        if !stashee_voice::model::is_complete(&model_dir()) {
            offer_download(ctx);
            return;
        }
    }
    start(ctx);
}

/// Esc while a session is up: kill it, discard the audio (or the
/// pending transcript).
pub(crate) fn cancel(ctx: &Rc<Ctx>) {
    if let Some(mut session) = ctx.voice.borrow_mut().session.take() {
        session.teardown();
    }
}

fn start(ctx: &Rc<Ctx>) {
    let Some((overlay, terminal)) = focused_pane(ctx) else {
        toast(ctx, "No pane to dictate into");
        return;
    };
    let recorder = match Recorder::start() {
        Ok(recorder) => recorder,
        Err(err) => {
            toast(ctx, &format!("Could not start recording: {err:#}"));
            return;
        }
    };
    // Get the model loading while the user is still speaking — by the
    // time they stop, the worker is usually warm.
    #[cfg(feature = "stt")]
    ensure_transcriber(ctx);

    let history = Rc::new(RefCell::new(VecDeque::from(vec![0.0; BARS])));
    let (pill, live, busy, wave) = build_pill(&history);
    overlay.add_overlay(&pill);

    let epoch = EPOCH.with(|epoch| {
        epoch.set(epoch.get() + 1);
        epoch.get()
    });
    ctx.voice.borrow_mut().session = Some(Session {
        epoch,
        phase: Phase::Recording {
            recorder,
            started: Instant::now(),
        },
        pill,
        live,
        busy,
        wave,
        overlay: overlay.downgrade(),
        terminal: terminal.downgrade(),
        history,
    });

    let ctx = ctx.clone();
    glib::timeout_add_local(TICK, move || tick(&ctx, epoch));
}

/// The voice key while recording: finish the capture and hand it to
/// the transcriber (or, without `stt`, report and discard).
fn stop(ctx: &Rc<Ctx>) {
    let Some(mut session) = ctx.voice.borrow_mut().session.take() else {
        return;
    };
    if !matches!(session.phase, Phase::Recording { .. }) {
        ctx.voice.borrow_mut().session = Some(session); // still transcribing
        return;
    }
    #[allow(irrefutable_let_patterns)] // the only variant without stt
    let Phase::Recording { recorder, .. } = session.phase else {
        return;
    };
    let recording = recorder.finish();

    #[cfg(feature = "stt")]
    {
        if recording.duration_secs() < 0.3 {
            remove_pill(&session.overlay, &session.pill);
            return; // a tap, not an utterance
        }
        ensure_transcriber(ctx);
        let reply = ctx
            .voice
            .borrow()
            .transcriber
            .as_ref()
            .map(|transcriber| transcriber.submit(recording));
        let Some(reply) = reply else {
            remove_pill(&session.overlay, &session.pill);
            return;
        };
        session.phase = Phase::Transcribing { reply };
        session.live.set_visible(false);
        session.busy.set_visible(true);
        ctx.voice.borrow_mut().session = Some(session);
    }

    #[cfg(not(feature = "stt"))]
    {
        remove_pill(&session.overlay, &session.pill);
        toast(
            ctx,
            &format!(
                "Recorded {:.1} s — this build has no speech recognition (stt feature off)",
                recording.duration_secs()
            ),
        );
    }
}

/// One frame: waveform while recording, result polling while
/// transcribing; also where a dead capture, a closed pane, or a
/// forgotten toggle is noticed.
fn tick(ctx: &Rc<Ctx>, epoch: u64) -> glib::ControlFlow {
    enum Outcome {
        Continue,
        AutoStop,
        Abort(Option<String>),
        #[cfg(feature = "stt")]
        Deliver(String),
    }
    let outcome = {
        let mut ctl = ctx.voice.borrow_mut();
        let Some(session) = ctl
            .session
            .as_mut()
            .filter(|session| session.epoch == epoch)
        else {
            return glib::ControlFlow::Break;
        };
        if session.pill.root().is_none() {
            // The pane the pill lived on was closed.
            Outcome::Abort(None)
        } else {
            match &mut session.phase {
                Phase::Recording { recorder, started } => {
                    if let Some(error) = recorder.failed() {
                        Outcome::Abort(Some(format!("Recording failed: {error}")))
                    } else if started.elapsed() >= AUTO_STOP {
                        Outcome::AutoStop
                    } else {
                        let levels = recorder.drain_levels();
                        let mut history = session.history.borrow_mut();
                        for level in levels {
                            history.push_back(display_level(level));
                            while history.len() > BARS {
                                history.pop_front();
                            }
                        }
                        drop(history);
                        session.wave.queue_draw();
                        Outcome::Continue
                    }
                }
                #[cfg(feature = "stt")]
                Phase::Transcribing { reply } => match reply.try_recv() {
                    Ok(Ok(text)) => Outcome::Deliver(text),
                    Ok(Err(err)) => Outcome::Abort(Some(format!("Transcription failed: {err:#}"))),
                    Err(std::sync::mpsc::TryRecvError::Empty) => Outcome::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Outcome::Abort(Some("Transcription worker died".to_owned()))
                    }
                },
            }
        }
    };
    match outcome {
        Outcome::Continue => glib::ControlFlow::Continue,
        Outcome::AutoStop => {
            stop(ctx);
            toast(ctx, "Recording stopped after 5 minutes — transcribing");
            glib::ControlFlow::Continue
        }
        Outcome::Abort(message) => {
            cancel(ctx);
            if let Some(message) = message {
                toast(ctx, &message);
            }
            glib::ControlFlow::Break
        }
        #[cfg(feature = "stt")]
        Outcome::Deliver(text) => {
            let session = ctx.voice.borrow_mut().session.take();
            if let Some(mut session) = session {
                session.teardown();
                deliver(ctx, &session, &text);
            }
            glib::ControlFlow::Break
        }
    }
}

/// Type the transcript into the pane it was dictated into — our own
/// terminal, so it goes straight down the pty, no Wayland input
/// injection. No trailing newline: the user reviews, then hits Enter.
#[cfg(feature = "stt")]
fn deliver(ctx: &Rc<Ctx>, session: &Session, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        toast(ctx, "Did not catch anything");
        return;
    }
    let Some(terminal) = session.terminal.upgrade() else {
        toast(ctx, "The pane closed — transcript discarded");
        return;
    };
    terminal.feed_child(text.as_bytes());
}

#[cfg(feature = "stt")]
fn model_dir() -> std::path::PathBuf {
    crate::paths::models_dir().join(stashee_voice::model::DIR_NAME)
}

#[cfg(feature = "stt")]
fn ensure_transcriber(ctx: &Rc<Ctx>) {
    let mut ctl = ctx.voice.borrow_mut();
    if ctl.transcriber.is_none() {
        ctl.transcriber = Some(stashee_voice::stt::Transcriber::spawn(model_dir()));
    }
}

/// If a download is running (or just finished/failed), toast its
/// state. Returns true when the key press is thereby handled.
#[cfg(feature = "stt")]
fn report_download(ctx: &Rc<Ctx>) -> bool {
    use stashee_voice::model::{DownloadState, downloaded_bytes, total_bytes};
    let state = match ctx.voice.borrow().download.as_ref() {
        Some(download) => download.state(),
        None => return false,
    };
    match state {
        DownloadState::Running => {
            let percent = downloaded_bytes(&model_dir()) * 100 / total_bytes().max(1);
            toast(ctx, &format!("Downloading the voice model — {percent}%"));
            true
        }
        // Done/Failed are announced by the watcher; treat the key
        // press as a fresh attempt.
        DownloadState::Done | DownloadState::Failed(_) => {
            ctx.voice.borrow_mut().download = None;
            false
        }
    }
}

/// First use: ask before pulling half a gigabyte, per SPEC ("explicit
/// consent; nothing in packages").
#[cfg(feature = "stt")]
fn offer_download(ctx: &Rc<Ctx>) {
    use stashee_voice::model::total_bytes;
    let dialog = libadwaita::AlertDialog::builder()
        .heading("Download the voice model?")
        .body(format!(
            "Dictation runs locally with NVIDIA's Parakeet model \
             ({} MB, 25 languages).\nOne-time download from huggingface.co into\n{}",
            total_bytes() / 1_000_000,
            model_dir().display(),
        ))
        .build();
    dialog.add_response("cancel", "Not Now");
    dialog.add_response("download", "Download");
    dialog.set_response_appearance("download", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("download"));
    dialog.set_close_response("cancel");
    let handler_ctx = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "download" {
            begin_download(&handler_ctx);
        }
    });
    dialog.present(Some(&ctx.toasts));
}

#[cfg(feature = "stt")]
fn begin_download(ctx: &Rc<Ctx>) {
    use stashee_voice::model::{Download, DownloadState, total_bytes};
    match Download::start(&model_dir()) {
        Ok(download) => {
            ctx.voice.borrow_mut().download = Some(download);
            toast(
                ctx,
                &format!(
                    "Downloading the voice model ({} MB) — the voice key shows progress",
                    total_bytes() / 1_000_000
                ),
            );
        }
        Err(err) => {
            toast(ctx, &format!("Could not start the download: {err:#}"));
            return;
        }
    }
    let ctx = ctx.clone();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let state = match ctx.voice.borrow().download.as_ref() {
            Some(download) => download.state(),
            None => return glib::ControlFlow::Break, // superseded
        };
        match state {
            DownloadState::Running => glib::ControlFlow::Continue,
            DownloadState::Done => {
                ctx.voice.borrow_mut().download = None;
                ensure_transcriber(&ctx);
                toast(&ctx, "Voice model ready — press the voice key to dictate");
                glib::ControlFlow::Break
            }
            DownloadState::Failed(message) => {
                ctx.voice.borrow_mut().download = None;
                toast(&ctx, &format!("Voice model download failed: {message}"));
                glib::ControlFlow::Break
            }
        }
    });
}

/// The pill: pulsing dot + waveform + hint while recording; spinner +
/// label while transcribing. An OSD chip centered at the bottom of
/// the pane, same glass family as the drag handle.
fn build_pill(
    history: &Rc<RefCell<VecDeque<f64>>>,
) -> (gtk::Widget, gtk::Box, gtk::Box, gtk::DrawingArea) {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("voice-dot");
    dot.set_valign(gtk::Align::Center);

    let wave = gtk::DrawingArea::new();
    wave.set_content_width(150);
    wave.set_content_height(24);
    wave.set_valign(gtk::Align::Center);
    let bars = history.clone();
    wave.set_draw_func(move |_, cr, width, height| {
        let bars = bars.borrow();
        let slot = f64::from(width) / BARS as f64;
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
        for (i, level) in bars.iter().enumerate() {
            let bar = (level * f64::from(height)).max(2.0);
            cr.rectangle(
                i as f64 * slot + slot * 0.25,
                (f64::from(height) - bar) / 2.0,
                slot * 0.5,
                bar,
            );
        }
        if let Err(err) = cr.fill() {
            tracing::warn!("waveform draw failed: {err}");
        }
    });

    let hint = gtk::Label::new(Some("Esc to cancel"));
    hint.add_css_class("voice-hint");
    hint.set_valign(gtk::Align::Center);

    let live = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    live.append(&dot);
    live.append(&wave);
    live.append(&hint);

    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let label = gtk::Label::new(Some("Transcribing…"));
    label.add_css_class("voice-hint");
    let busy = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    busy.append(&spinner);
    busy.append(&label);
    busy.set_visible(false);

    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pill.add_css_class("voice-pill");
    pill.set_halign(gtk::Align::Center);
    pill.set_valign(gtk::Align::End);
    pill.set_margin_bottom(14);
    pill.set_tooltip_text(Some("Recording — the voice key stops, Esc cancels"));
    pill.append(&live);
    pill.append(&busy);
    (pill.upcast(), live, busy, wave)
}

/// Bar height for a raw RMS level: square root stretches the quiet
/// range speech actually lives in (~0.02..0.3 RMS), the floor keeps an
/// idle mic visible as a thin line.
fn display_level(level: f32) -> f64 {
    (f64::from(level).max(0.0).sqrt() * 1.6).clamp(0.06, 1.0)
}
