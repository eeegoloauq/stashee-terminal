//! Minimal Parakeet-TDT speech-to-text engine over ONNX Runtime,
//! sized for stashee's one job: a dictated utterance in, plain text
//! out. CPU, int8, no timestamps. The greedy transducer decode is
//! ported from transcribe-rs (MIT, © cjpais) against the
//! `istupakov/parakeet-tdt-0.6b-v3-onnx` export; vendoring those
//! ~300 lines costs one dependency (`ort`) instead of six.
//!
//! Pipeline: 16 kHz mono f32 → `nemo128.onnx` (mel features — the
//! DSP ships inside the model, no audio math here) → encoder →
//! per-frame decoder/joint loop emitting vocabulary tokens.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use ort::inputs;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::{Backend, Recording, SAMPLE_RATE};

/// The mel preprocessor attenuates the very start of the audio, so a
/// beat of silence protects the first word (matches transcribe-rs).
const LEADING_SILENCE_MS: usize = 250;

/// Safety valve for the transducer loop: at most this many tokens per
/// encoder frame before the frame is force-advanced.
const MAX_TOKENS_PER_STEP: usize = 10;

pub struct ParakeetEngine {
    preprocessor: Session,
    encoder: Session,
    decoder_joint: Session,
    /// Token id → text, `▁` already turned into a space.
    vocab: Vec<String>,
    blank: usize,
    /// `(layers, hidden)` for the two decoder LSTM state tensors,
    /// read from the model's input metadata.
    state_dims: [(usize, usize); 2],
}

impl ParakeetEngine {
    /// Load the three sessions and the vocabulary from `dir` (the
    /// layout of [`crate::model::FILES`]). Slow — seconds — so callers
    /// keep the engine resident.
    pub fn load(dir: &Path) -> Result<Self> {
        let preprocessor = session(&dir.join("nemo128.onnx"))?;
        let encoder = session(&dir.join("encoder-model.int8.onnx"))?;
        let decoder_joint = session(&dir.join("decoder_joint-model.int8.onnx"))?;

        let vocab_text = std::fs::read_to_string(dir.join("vocab.txt"))
            .with_context(|| format!("reading {}", dir.join("vocab.txt").display()))?;
        let (vocab, blank) = parse_vocab(&vocab_text)?;

        let state_dims = [
            state_dims(&decoder_joint, "input_states_1")?,
            state_dims(&decoder_joint, "input_states_2")?,
        ];

        Ok(Self {
            preprocessor,
            encoder,
            decoder_joint,
            vocab,
            blank,
            state_dims,
        })
    }

    /// 16 kHz mono f32 samples in `[-1, 1]` → transcript.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let mut padded = vec![0.0; SAMPLE_RATE as usize * LEADING_SILENCE_MS / 1000];
        padded.extend_from_slice(samples);

        let (features, features_len) = self.preprocess(padded)?;
        let encoded = self.encode(features, features_len)?;
        let tokens = self.decode(&encoded)?;

        let text: String = tokens
            .iter()
            .filter_map(|&id| self.vocab.get(id).map(String::as_str))
            .collect();
        // SentencePiece word boundaries became spaces in the vocab;
        // normalizing whitespace finishes the detokenization.
        Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// Raw audio → mel features `(data, [1, mel, frames])` ready for
    /// the encoder.
    fn preprocess(&mut self, samples: Vec<f32>) -> Result<(Tensor<f32>, Tensor<i64>)> {
        let len = samples.len();
        let outputs = ort_ok(self.preprocessor.run(inputs![
            "waveforms" => ort_ok(Tensor::from_array(([1, len], samples)))?,
            "waveforms_lens" => ort_ok(Tensor::from_array(([1], vec![len as i64])))?,
        ]))?;
        let (shape, data) = tensor_f32(&outputs, "features")?;
        let features = ort_ok(Tensor::from_array((shape.to_vec(), data.to_vec())))?;
        let (shape, data) = tensor_i64(&outputs, "features_lens")?;
        let features_lens = ort_ok(Tensor::from_array((shape.to_vec(), data.to_vec())))?;
        Ok((features, features_lens))
    }

    /// Mel features → encoder output.
    fn encode(&mut self, features: Tensor<f32>, features_lens: Tensor<i64>) -> Result<Encoded> {
        let outputs = ort_ok(self.encoder.run(inputs![
            "audio_signal" => features,
            "length" => features_lens,
        ]))?;
        let (shape, data) = tensor_f32(&outputs, "outputs")?;
        // (1, dim, frames), row-major — no transpose needed, frames
        // are gathered column-wise in `decode`.
        let [_, dim, stride] = dims::<3>(shape)?;
        let (_, lens) = tensor_i64(&outputs, "encoded_lengths")?;
        let valid = usize::try_from(
            *lens
                .first()
                .ok_or_else(|| anyhow!("empty encoded_lengths"))?,
        )
        .unwrap_or(0)
        .min(stride);
        Ok(Encoded {
            data: data.to_vec(),
            dim,
            stride,
            valid,
        })
    }

    /// Greedy transducer loop: for each encoder frame, emit tokens
    /// until the joint predicts blank (advancing the LSTM state only
    /// on real tokens), then move to the next frame.
    fn decode(&mut self, encoded: &Encoded) -> Result<Vec<usize>> {
        let mut states = [
            vec![0.0f32; self.state_dims[0].0 * self.state_dims[0].1],
            vec![0.0f32; self.state_dims[1].0 * self.state_dims[1].1],
        ];
        let mut tokens: Vec<usize> = Vec::new();

        let mut t = 0;
        let mut emitted_this_step = 0;
        while t < encoded.valid {
            // Column t of the (dim, frames) matrix.
            let frame: Vec<f32> = (0..encoded.dim)
                .map(|d| encoded.data[d * encoded.stride + t])
                .collect();
            let last = tokens.last().copied().unwrap_or(self.blank) as i32;

            let outputs = ort_ok(self.decoder_joint.run(inputs![
                "encoder_outputs" => ort_ok(Tensor::from_array(([1, encoded.dim, 1], frame)))?,
                "targets" => ort_ok(Tensor::from_array(([1, 1], vec![last])))?,
                "target_length" => ort_ok(Tensor::from_array(([1], vec![1i32])))?,
                "input_states_1" => state_tensor(&states[0], self.state_dims[0])?,
                "input_states_2" => state_tensor(&states[1], self.state_dims[1])?,
            ]))?;

            // Logits are (1, vocab + TDT duration bins); duration bins
            // are ignored — greedy label decoding is what transcribe-rs
            // ships and it holds up in practice.
            let (_, logits) = tensor_f32(&outputs, "outputs")?;
            let token = argmax(&logits[..self.vocab.len().min(logits.len())])
                .ok_or_else(|| anyhow!("empty logits from decoder"))?;

            if token != self.blank {
                let (_, s1) = tensor_f32(&outputs, "output_states_1")?;
                let (_, s2) = tensor_f32(&outputs, "output_states_2")?;
                states = [s1.to_vec(), s2.to_vec()];
                tokens.push(token);
                emitted_this_step += 1;
            }
            if token == self.blank || emitted_this_step == MAX_TOKENS_PER_STEP {
                t += 1;
                emitted_this_step = 0;
            }
        }
        Ok(tokens)
    }
}

/// Encoder output: `(1, dim, stride)` row-major, of which the first
/// `valid` frames are real audio.
struct Encoded {
    data: Vec<f32>,
    dim: usize,
    stride: usize,
    valid: usize,
}

impl Backend for ParakeetEngine {
    fn transcribe(&mut self, recording: &Recording) -> Result<String> {
        self.transcribe(&recording.samples_f32())
    }
}

/// ort's error type is not Send+Sync and cannot ride through anyhow —
/// flatten it to a message at the boundary.
fn ort_ok<T>(result: ort::Result<T>) -> Result<T> {
    result.map_err(|err| anyhow!("onnxruntime: {err}"))
}

fn session(path: &Path) -> Result<Session> {
    let build = || -> ort::Result<Session> {
        Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .commit_from_file(path)
    };
    ort_ok(build()).with_context(|| format!("loading {}", path.display()))
}

/// `(layers, hidden)` of a decoder state input, whose declared shape
/// is `(layers, batch, hidden)`.
fn state_dims(session: &Session, input: &str) -> Result<(usize, usize)> {
    let shape = session
        .inputs()
        .iter()
        .find(|outlet| outlet.name() == input)
        .ok_or_else(|| anyhow!("decoder has no input {input}"))?
        .dtype()
        .tensor_shape()
        .ok_or_else(|| anyhow!("decoder input {input} has no static shape"))?;
    let [layers, _, hidden] = dims::<3>(shape)?;
    Ok((layers, hidden))
}

fn state_tensor(data: &[f32], (layers, hidden): (usize, usize)) -> Result<Tensor<f32>> {
    ort_ok(Tensor::from_array(([layers, 1, hidden], data.to_vec())))
}

fn tensor_f32<'a>(
    outputs: &'a ort::session::SessionOutputs<'_>,
    name: &str,
) -> Result<(&'a [i64], &'a [f32])> {
    let (shape, data) = ort_ok(
        outputs
            .get(name)
            .ok_or_else(|| anyhow!("model returned no output {name}"))?
            .try_extract_tensor::<f32>(),
    )?;
    Ok((shape, data))
}

fn tensor_i64<'a>(
    outputs: &'a ort::session::SessionOutputs<'_>,
    name: &str,
) -> Result<(&'a [i64], &'a [i64])> {
    let (shape, data) = ort_ok(
        outputs
            .get(name)
            .ok_or_else(|| anyhow!("model returned no output {name}"))?
            .try_extract_tensor::<i64>(),
    )?;
    Ok((shape, data))
}

/// A shape's dims as usizes, insisting on exactly `N` axes.
fn dims<const N: usize>(shape: &[i64]) -> Result<[usize; N]> {
    let got: Vec<usize> = shape
        .iter()
        .map(|&d| usize::try_from(d).unwrap_or(0))
        .collect();
    <[usize; N]>::try_from(got).map_err(|_| anyhow!("expected {N}-D tensor, got {shape:?}"))
}

fn argmax(values: &[f32]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(index, _)| index)
}

/// `token id` per line; `▁` (U+2581) marks a word start and becomes a
/// leading space. Returns the id-indexed table and the blank id.
fn parse_vocab(text: &str) -> Result<(Vec<String>, usize)> {
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut blank = None;
    for line in text.lines() {
        let Some((token, id)) = line.trim_end().rsplit_once(' ') else {
            continue;
        };
        let Ok(id) = id.parse::<usize>() else {
            continue;
        };
        if token == "<blk>" {
            blank = Some(id);
        }
        entries.push((id, token.replace('\u{2581}', " ")));
    }
    let size = entries.iter().map(|(id, _)| id + 1).max().unwrap_or(0);
    let mut vocab = vec![String::new(); size];
    for (id, token) in entries {
        vocab[id] = token;
    }
    let blank = blank.ok_or_else(|| anyhow!("vocab.txt has no <blk> token"))?;
    Ok((vocab, blank))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn vocab_parses_ids_spaces_and_blank() {
        let (vocab, blank) = parse_vocab("<unk> 0\n\u{2581}the 1\ning 2\n<blk> 3\n").unwrap();
        assert_eq!(vocab, vec!["<unk>", " the", "ing", "<blk>"]);
        assert_eq!(blank, 3);
    }

    #[test]
    fn vocab_without_blank_is_an_error() {
        assert!(parse_vocab("a 0\nb 1\n").is_err());
    }

    #[test]
    fn vocab_skips_malformed_lines() {
        let (vocab, _) = parse_vocab("a 0\ngarbage\nb x\n<blk> 1\n").unwrap();
        assert_eq!(vocab.len(), 2);
    }

    #[test]
    fn argmax_picks_the_largest_and_survives_nan() {
        assert_eq!(argmax(&[0.1, 0.9, 0.5]), Some(1));
        assert_eq!(argmax(&[f32::NAN, 1.0, 0.0]), Some(1));
        assert_eq!(argmax(&[]), None);
    }

    #[test]
    fn dims_enforces_rank() {
        assert_eq!(dims::<3>(&[1, 2, 3]).unwrap(), [1, 2, 3]);
        assert!(dims::<2>(&[1, 2, 3]).is_err());
    }
}
