//! The transcription worker: one thread that loads the model once
//! (eagerly, on spawn — the seconds of load time overlap with the
//! user still speaking) and then serves transcription jobs for the
//! rest of the app's life. The GTK side polls the per-job receiver
//! from its frame tick; nothing here blocks the main thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result};

use crate::Recording;
use crate::parakeet::ParakeetEngine;

struct Job {
    recording: Recording,
    reply: Sender<Result<String>>,
}

pub struct Transcriber {
    jobs: Sender<Job>,
}

impl Transcriber {
    /// Start the worker and begin loading the model from `model_dir`
    /// immediately. Returns at once.
    #[must_use]
    pub fn spawn(model_dir: PathBuf) -> Self {
        let (jobs, inbox) = channel::<Job>();
        std::thread::spawn(move || worker(&model_dir, &inbox));
        Self { jobs }
    }

    /// Queue a recording; the result arrives on the returned channel.
    /// Dropping the receiver discards the result — that is how the UI
    /// cancels a transcription it no longer wants.
    #[must_use]
    pub fn submit(&self, recording: Recording) -> Receiver<Result<String>> {
        let (reply, result) = channel();
        if let Err(err) = self.jobs.send(Job { recording, reply }) {
            // Worker gone (engine load panicked?) — surface it on the
            // reply channel the caller is about to poll.
            let Job { reply, .. } = err.0;
            let _ = reply.send(Err(anyhow::anyhow!("transcription worker is gone")));
            return result;
        }
        result
    }
}

fn worker(model_dir: &Path, inbox: &Receiver<Job>) {
    let started = std::time::Instant::now();
    let mut engine = ParakeetEngine::load(model_dir)
        .with_context(|| format!("loading the voice model from {}", model_dir.display()));
    match &engine {
        Ok(_) => tracing::info!("voice model loaded in {:?}", started.elapsed()),
        Err(err) => tracing::warn!("voice model failed to load: {err:#}"),
    }

    while let Ok(job) = inbox.recv() {
        let result = match engine.as_mut() {
            Ok(engine) => {
                let started = std::time::Instant::now();
                let result = engine.transcribe(&job.recording.samples_f32());
                tracing::info!(
                    "transcribed {:.1}s of audio in {:?}",
                    job.recording.duration_secs(),
                    started.elapsed()
                );
                result
            }
            Err(err) => Err(anyhow::anyhow!("{err:#}")),
        };
        let _ = job.reply.send(result); // receiver may have cancelled
    }
}
