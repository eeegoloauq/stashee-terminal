//! Voice input (v2 preview): the record toggle, the pill overlaid on
//! the focused pane, and the waveform drawn from mic levels. Capture
//! itself is `stashee-voice` (a `pw-record` subprocess); this module
//! is only the GTK wiring. Speech recognition is not attached yet —
//! stopping a recording currently reports its length and discards it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use stashee_voice::Recorder;

use crate::window::{Ctx, focused_pane_overlay, toast};

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

pub(crate) struct Session {
    epoch: u64,
    recorder: Option<Recorder>,
    pill: gtk::Widget,
    wave: gtk::DrawingArea,
    overlay: glib::WeakRef<gtk::Overlay>,
    history: Rc<RefCell<VecDeque<f64>>>,
    started: Instant,
}

impl Session {
    fn teardown(&mut self) {
        if let Some(overlay) = self.overlay.upgrade() {
            overlay.remove_overlay(&self.pill);
        }
    }
}

pub(crate) fn active(ctx: &Rc<Ctx>) -> bool {
    ctx.voice.borrow().is_some()
}

/// The voice key: stop a running recording, otherwise start one.
pub(crate) fn toggle(ctx: &Rc<Ctx>) {
    if active(ctx) {
        stop(ctx);
    } else if !ctx.config.borrow().voice.enabled {
        toast(
            ctx,
            "Voice input is off — set [voice] enabled = true in config.toml (experimental)",
        );
    } else {
        start(ctx);
    }
}

/// Esc while recording: kill the capture, discard the audio.
pub(crate) fn cancel(ctx: &Rc<Ctx>) {
    if let Some(mut session) = ctx.voice.borrow_mut().take() {
        session.teardown();
    }
}

fn start(ctx: &Rc<Ctx>) {
    let Some(overlay) = focused_pane_overlay(ctx) else {
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

    let history = Rc::new(RefCell::new(VecDeque::from(vec![0.0; BARS])));
    let (pill, wave) = build_pill(&history);
    overlay.add_overlay(&pill);

    let epoch = EPOCH.with(|epoch| {
        epoch.set(epoch.get() + 1);
        epoch.get()
    });
    *ctx.voice.borrow_mut() = Some(Session {
        epoch,
        recorder: Some(recorder),
        pill,
        wave,
        overlay: overlay.downgrade(),
        history,
        started: Instant::now(),
    });

    let ctx = ctx.clone();
    glib::timeout_add_local(TICK, move || tick(&ctx, epoch));
}

/// One waveform frame; also where a dead capture, a closed pane, or a
/// forgotten toggle is noticed.
fn tick(ctx: &Rc<Ctx>, epoch: u64) -> glib::ControlFlow {
    enum Outcome {
        Continue,
        AutoStop,
        Abort(Option<String>),
    }
    let outcome = {
        let mut guard = ctx.voice.borrow_mut();
        let Some(session) = guard.as_mut().filter(|session| session.epoch == epoch) else {
            return glib::ControlFlow::Break;
        };
        if session.pill.root().is_none() {
            // The pane the pill lived on was closed.
            Outcome::Abort(None)
        } else if let Some(error) = session.recorder.as_mut().and_then(Recorder::failed) {
            Outcome::Abort(Some(format!("Recording failed: {error}")))
        } else if session.started.elapsed() >= AUTO_STOP {
            Outcome::AutoStop
        } else {
            let levels = session
                .recorder
                .as_ref()
                .map(Recorder::drain_levels)
                .unwrap_or_default();
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
    };
    match outcome {
        Outcome::Continue => glib::ControlFlow::Continue,
        Outcome::AutoStop => {
            stop(ctx);
            toast(ctx, "Voice recording auto-stopped after 5 minutes");
            glib::ControlFlow::Break
        }
        Outcome::Abort(message) => {
            cancel(ctx);
            if let Some(message) = message {
                toast(ctx, &message);
            }
            glib::ControlFlow::Break
        }
    }
}

fn stop(ctx: &Rc<Ctx>) {
    let Some(mut session) = ctx.voice.borrow_mut().take() else {
        return;
    };
    session.teardown();
    let Some(recorder) = session.recorder.take() else {
        return;
    };
    let recording = recorder.finish();
    // The milestone boundary: capture works end to end, recognition
    // backends are next (SPEC.md roadmap v2).
    toast(
        ctx,
        &format!(
            "Recorded {:.1} s — speech recognition is not wired up yet, so the clip was discarded",
            recording.duration_secs()
        ),
    );
}

/// The recording pill: pulsing dot, waveform, cancel hint — an OSD
/// chip centered at the bottom of the pane, same glass family as the
/// drag handle.
fn build_pill(history: &Rc<RefCell<VecDeque<f64>>>) -> (gtk::Widget, gtk::DrawingArea) {
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

    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    pill.add_css_class("voice-pill");
    pill.set_halign(gtk::Align::Center);
    pill.set_valign(gtk::Align::End);
    pill.set_margin_bottom(14);
    pill.set_tooltip_text(Some("Recording — the voice key stops, Esc cancels"));
    pill.append(&dot);
    pill.append(&wave);
    pill.append(&hint);
    (pill.upcast(), wave)
}

/// Bar height for a raw RMS level: square root stretches the quiet
/// range speech actually lives in (~0.02..0.3 RMS), the floor keeps an
/// idle mic visible as a thin line.
fn display_level(level: f32) -> f64 {
    (f64::from(level).max(0.0).sqrt() * 1.6).clamp(0.06, 1.0)
}
