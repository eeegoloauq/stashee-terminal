//! Dev harness: download the model if needed, then transcribe raw
//! 16 kHz mono s16le audio from a file.
//!
//! cargo run -p stashee-voice --example transcribe -- <model-dir> <audio.raw>

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(model_dir), Some(audio)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: transcribe <model-dir> <audio.raw>");
    };
    let model_dir = PathBuf::from(model_dir);

    if !stashee_voice::model::is_complete(&model_dir) {
        eprintln!(
            "downloading model ({} MB) to {}",
            stashee_voice::model::total_bytes() / 1_000_000,
            model_dir.display()
        );
        let download = stashee_voice::model::Download::start(&model_dir)?;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            match download.state() {
                stashee_voice::model::DownloadState::Running => {
                    eprintln!(
                        "  {} / {} MB",
                        stashee_voice::model::downloaded_bytes(&model_dir) / 1_000_000,
                        stashee_voice::model::total_bytes() / 1_000_000
                    );
                }
                stashee_voice::model::DownloadState::Done => break,
                stashee_voice::model::DownloadState::Failed(message) => {
                    anyhow::bail!("download failed: {message}")
                }
            }
        }
    }

    let bytes = std::fs::read(&audio)?;
    let samples: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / f32::from(i16::MAX))
        .collect();
    eprintln!("audio: {:.1}s", samples.len() as f32 / 16_000.0);

    let started = std::time::Instant::now();
    let mut engine = stashee_voice::parakeet::ParakeetEngine::load(&model_dir)?;
    eprintln!("model loaded in {:?}", started.elapsed());

    let started = std::time::Instant::now();
    let text = engine.transcribe(&samples)?;
    eprintln!("transcribed in {:?}", started.elapsed());
    println!("{text}");
    Ok(())
}
