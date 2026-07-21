//! The transcription worker: one thread that owns the model for the
//! app's life, loading it on demand and dropping it again after a
//! caller-configured stretch of no dictation — the app can stay open
//! for weeks without parking the model's RAM forever. A warm-up message is sent when
//! recording starts, so the seconds of load time overlap with the
//! user still speaking. The GTK side polls the per-job receiver from
//! its frame tick; nothing here blocks the main thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::Recording;
use crate::parakeet::ParakeetEngine;

enum Msg {
    /// Recording just started: get the model loading now.
    Warm,
    /// The idle-unload timeout changed (live config reload); `None`
    /// keeps the model loaded for the app's life.
    SetIdleUnload(Option<Duration>),
    Job(Job),
}

struct Job {
    recording: Recording,
    reply: Sender<Result<String>>,
}

pub struct Transcriber {
    msgs: Sender<Msg>,
}

impl Transcriber {
    /// Start the worker and begin loading the model from `model_dir`
    /// immediately. Returns at once. `idle_unload` is how long the
    /// model survives without dictation before being dropped to free
    /// its RAM (`None` = for the app's life) — policy belongs to the
    /// caller's config, this crate just obeys it.
    #[must_use]
    pub fn spawn(model_dir: PathBuf, idle_unload: Option<Duration>) -> Self {
        let (msgs, inbox) = channel::<Msg>();
        std::thread::spawn(move || worker(&model_dir, &inbox, idle_unload));
        let this = Self { msgs };
        this.warm();
        this
    }

    /// Apply a changed idle-unload timeout to the running worker —
    /// the live config-reload path.
    pub fn set_idle_unload(&self, idle_unload: Option<Duration>) {
        let _ = self.msgs.send(Msg::SetIdleUnload(idle_unload));
    }

    /// Nudge the worker to (re)load the model — called when recording
    /// starts, so an unloaded model is warm again by the time the
    /// user stops speaking.
    pub fn warm(&self) {
        let _ = self.msgs.send(Msg::Warm);
    }

    /// Queue a recording; the result arrives on the returned channel.
    /// Dropping the receiver discards the result — that is how the UI
    /// cancels a transcription it no longer wants.
    #[must_use]
    pub fn submit(&self, recording: Recording) -> Receiver<Result<String>> {
        let (reply, result) = channel();
        if let Err(err) = self.msgs.send(Msg::Job(Job { recording, reply })) {
            // Worker gone (engine load panicked?) — surface it on the
            // reply channel the caller is about to poll.
            let Msg::Job(Job { reply, .. }) = err.0 else {
                return result;
            };
            let _ = reply.send(Err(anyhow!("transcription worker is gone")));
            return result;
        }
        result
    }
}

fn worker(model_dir: &Path, inbox: &Receiver<Msg>, mut idle_unload: Option<Duration>) {
    let mut engine: Option<ParakeetEngine> = None;
    loop {
        let msg = match idle_unload {
            Some(timeout) => match inbox.recv_timeout(timeout) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => {
                    if engine.take().is_some() {
                        // Freed buffers land in malloc arenas, which
                        // keep the pages resident; without the trim
                        // the model's ~1 GB never actually leaves the
                        // process.
                        stashee_pty::trim_malloc();
                        tracing::info!("voice model unloaded after {timeout:?} idle");
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            },
            // Unload disabled: block until the next message. A later
            // SetIdleUnload is itself a message, so re-enabling wakes
            // this arm and the new timeout takes over immediately.
            None => match inbox.recv() {
                Ok(msg) => msg,
                Err(_) => return,
            },
        };
        match msg {
            // A failed load is not cached: the next warm-up retries,
            // so e.g. a repaired model directory heals without a
            // restart.
            Msg::Warm => {
                let _ = ensure_loaded(&mut engine, model_dir);
            }
            Msg::SetIdleUnload(timeout) => idle_unload = timeout,
            Msg::Job(job) => {
                let result = ensure_loaded(&mut engine, model_dir).and_then(|engine| {
                    let started = std::time::Instant::now();
                    let result = engine.transcribe(&job.recording.samples_f32());
                    tracing::info!(
                        "transcribed {:.1}s of audio in {:?}",
                        job.recording.duration_secs(),
                        started.elapsed()
                    );
                    result
                });
                let _ = job.reply.send(result); // receiver may have cancelled
            }
        }
    }
}

fn ensure_loaded<'a>(
    engine: &'a mut Option<ParakeetEngine>,
    model_dir: &Path,
) -> Result<&'a mut ParakeetEngine> {
    if engine.is_none() {
        let started = std::time::Instant::now();
        match ParakeetEngine::load(model_dir)
            .with_context(|| format!("loading the voice model from {}", model_dir.display()))
        {
            Ok(loaded) => {
                tracing::info!("voice model loaded in {:?}", started.elapsed());
                *engine = Some(loaded);
            }
            Err(err) => {
                tracing::warn!("voice model failed to load: {err:#}");
                return Err(err);
            }
        }
    }
    engine
        .as_mut()
        .ok_or_else(|| anyhow!("voice engine missing right after loading"))
}
