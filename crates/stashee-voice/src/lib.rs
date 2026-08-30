//! Voice input capture (v2 preview). The microphone is read through a
//! `pw-record` subprocess — PipeWire ships with the v1 target
//! (Fedora/GNOME), so this costs no native audio dependency, and a
//! real capture client also lights GNOME's own mic-in-use indicator.
//! No GTK here: the frontend polls levels for its waveform and takes
//! the samples when recording ends.

use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};

#[cfg(feature = "stt")]
pub mod model;
#[cfg(feature = "stt")]
pub mod parakeet;
#[cfg(feature = "stt")]
pub mod stt;

/// What `pw-record` is asked to resample to: 16 kHz mono s16 — the
/// input format every candidate STT model shares.
pub const SAMPLE_RATE: u32 = 16_000;

/// Samples per waveform level update (~30 ms — one level per UI frame
/// at 30 fps).
const CHUNK_SAMPLES: usize = 480;

/// Buffer cap (10 min of audio, ~18 MB). The frontend auto-stops long
/// before this; the cap only guards against a runaway session.
const MAX_SAMPLES: usize = SAMPLE_RATE as usize * 600;

/// How much of `pw-record`'s stderr to keep for the failure toast.
const STDERR_CAP: usize = 4096;

/// A speech-to-text engine (SPEC.md roadmap v2: Parakeet now, GigaAM
/// and a cloud option later, all behind this one seam).
pub trait Backend: Send {
    fn transcribe(&mut self, recording: &Recording) -> Result<String>;
}

/// A finished capture, ready for a [`Backend`].
pub struct Recording {
    /// 16 kHz mono, little-endian source.
    pub samples: Vec<i16>,
}

impl Recording {
    #[must_use]
    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }

    /// The samples as f32 in `[-1, 1]` — what the STT models eat.
    #[must_use]
    pub fn samples_f32(&self) -> Vec<f32> {
        self.samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect()
    }
}

struct Shared {
    /// One RMS level (0..=1) per chunk, drained by the UI's waveform.
    levels: Mutex<Vec<f32>>,
    samples: Mutex<Vec<i16>>,
}

/// A running capture: the `pw-record` child and the thread draining
/// its stdout. Dropping it kills the capture and discards the audio;
/// [`finish`](Self::finish) keeps it.
pub struct Recorder {
    child: Child,
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
    /// What the child complained about, so a failure can say why.
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let mut child = Command::new("pw-record")
            // `--raw` is what makes `-` mean stdout and the bytes mean
            // exactly what --format says. Without it pw-record hands the
            // stream to libsndfile, which writes a big-endian .au file
            // named `-` in the working directory and leaves this pipe empty.
            .args([
                "--raw",
                "--rate",
                "16000",
                "--channels",
                "1",
                "--format",
                "s16",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running pw-record (PipeWire's recorder — pipewire-utils on Fedora)")?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            bail!("pw-record spawned without a stdout pipe");
        };
        let shared = Arc::new(Shared {
            levels: Mutex::new(Vec::new()),
            samples: Mutex::new(Vec::new()),
        });
        let reader = {
            let shared = shared.clone();
            std::thread::spawn(move || read_loop(stdout, &shared))
        };
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_reader = child.stderr.take().map(|pipe| {
            let sink = stderr.clone();
            std::thread::spawn(move || stderr_loop(pipe, &sink))
        });
        Ok(Self {
            child,
            shared,
            reader: Some(reader),
            stderr,
            stderr_reader,
        })
    }

    /// Levels accumulated since the last call (usually one per UI
    /// frame), oldest first.
    pub fn drain_levels(&self) -> Vec<f32> {
        self.shared
            .levels
            .lock()
            .map(|mut levels| std::mem::take(&mut *levels))
            .unwrap_or_default()
    }

    /// A capture that died under us — no microphone, PipeWire stopped,
    /// an argument this build rejects. `None` while recording runs.
    pub fn failed(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(None) => None,
            // pw-record's own last word beats a bare exit status: it is
            // the only thing that says *which* of those it was.
            Ok(Some(status)) => Some(match self.complaint() {
                Some(reason) => describe(&reason),
                None => format!("pw-record exited early ({status})"),
            }),
            Err(err) => Some(format!("pw-record is gone: {err}")),
        }
    }

    /// The last thing the child wrote to stderr, if anything.
    fn complaint(&self) -> Option<String> {
        let text = self.stderr.lock().ok()?;
        let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
        Some(line.to_owned())
    }

    /// Stop capturing and keep the audio.
    pub fn finish(mut self) -> Recording {
        self.shutdown();
        let samples = self
            .shared
            .samples
            .lock()
            .map(|mut samples| std::mem::take(&mut *samples))
            .unwrap_or_default();
        Recording { samples }
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take()
            && reader.join().is_err()
        {
            tracing::warn!("voice capture reader thread panicked");
        }
        if let Some(reader) = self.stderr_reader.take()
            && reader.join().is_err()
        {
            tracing::warn!("voice capture stderr thread panicked");
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_loop(mut stdout: ChildStdout, shared: &Shared) {
    let mut buf = vec![0u8; CHUNK_SAMPLES * 2];
    loop {
        let n = read_full(&mut stdout, &mut buf);
        if n == 0 {
            return;
        }
        let chunk = samples_from_le_bytes(&buf[..n]);
        if let Ok(mut levels) = shared.levels.lock() {
            levels.push(level(&chunk));
        }
        if let Ok(mut samples) = shared.samples.lock()
            && samples.len() < MAX_SAMPLES
        {
            samples.extend_from_slice(&chunk);
        }
        if n < buf.len() {
            return; // EOF mid-chunk: the child was killed
        }
    }
}

/// pw-record's last stderr line, turned into something a toast can say.
/// A muted or absent input reaches us as a stream-node error — by far
/// the most common failure, and the one whose wording says nothing to
/// the person holding the microphone. Everything else passes through as
/// it came, because guessing at it would say less than pw-record does.
fn describe(complaint: &str) -> String {
    if complaint.contains("no node available") {
        return "no microphone — check the input device in Sound settings".to_owned();
    }
    complaint.to_owned()
}

/// Collect the child's stderr so a failure can quote it. Bounded: only
/// the tail matters, and the tail is what a toast shows.
fn stderr_loop(mut stderr: ChildStderr, sink: &Mutex<String>) {
    let mut buf = [0u8; 512];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if let Ok(mut text) = sink.lock() {
                    push_bounded(&mut text, &String::from_utf8_lossy(&buf[..n]));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Append `chunk`, dropping leading text so `text` stays under
/// [`STDERR_CAP`] bytes and still starts on a character.
fn push_bounded(text: &mut String, chunk: &str) {
    text.push_str(chunk);
    if text.len() <= STDERR_CAP {
        return;
    }
    let want = text.len() - STDERR_CAP;
    let start = (want..text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    text.drain(..start);
}

/// Fill `buf` from `reader` as far as possible; short only at EOF (or
/// a broken pipe, which for a killed child is the same thing).
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

/// Little-endian s16 frames; a trailing odd byte is dropped.
fn samples_from_le_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

/// RMS of one chunk, normalized to 0..=1. Raw, not perceptual — the
/// waveform applies its own display scaling.
fn level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&sample| (f64::from(sample) / f64::from(i16::MAX)).powi(2))
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_decode_little_endian_and_drop_an_odd_tail() {
        assert_eq!(
            samples_from_le_bytes(&[0x01, 0x00, 0xff, 0xff, 0x2a]),
            vec![1, -1]
        );
        assert!(samples_from_le_bytes(&[]).is_empty());
    }

    #[test]
    fn silence_is_zero_level() {
        assert_eq!(level(&[0; CHUNK_SAMPLES]), 0.0);
        assert_eq!(level(&[]), 0.0);
    }

    #[test]
    fn full_scale_square_wave_is_unit_level() {
        let square: Vec<i16> = (0..CHUNK_SAMPLES)
            .map(|i| if i % 2 == 0 { i16::MAX } else { -i16::MAX })
            .collect();
        assert!((level(&square) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn half_scale_square_wave_is_half_level() {
        let square: Vec<i16> = (0..CHUNK_SAMPLES)
            .map(|i| {
                if i % 2 == 0 {
                    i16::MAX / 2
                } else {
                    -i16::MAX / 2
                }
            })
            .collect();
        assert!((level(&square) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn recording_reports_duration_from_sample_count() {
        let recording = Recording {
            samples: vec![0; SAMPLE_RATE as usize * 3 / 2],
        };
        assert!((recording.duration_secs() - 1.5).abs() < 1e-6);
    }

    #[test]
    fn a_missing_input_reads_as_a_missing_microphone() {
        assert_eq!(
            describe("stream node 74 error: no node available"),
            "no microphone — check the input device in Sound settings"
        );
    }

    #[test]
    fn an_unrecognized_complaint_is_passed_through() {
        assert_eq!(
            describe("remote error: id=0 seq:1 res:-32 (Broken pipe): connection error"),
            "remote error: id=0 seq:1 res:-32 (Broken pipe): connection error"
        );
    }

    #[test]
    fn stderr_buffer_keeps_the_tail_and_stays_bounded() {
        let mut text = String::new();
        for _ in 0..100 {
            push_bounded(&mut text, &"x".repeat(100));
        }
        push_bounded(&mut text, "sndfile: failed to open");
        assert!(text.len() <= STDERR_CAP);
        assert!(text.ends_with("sndfile: failed to open"));
    }

    #[test]
    fn stderr_buffer_trims_on_a_character_boundary() {
        let mut text = "ы".repeat(STDERR_CAP);
        push_bounded(&mut text, "ok");
        assert!(text.len() <= STDERR_CAP);
        assert!(text.ends_with("ok"));
        assert!(text.starts_with('ы'));
    }
}
