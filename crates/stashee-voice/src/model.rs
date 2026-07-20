//! The Parakeet model on disk: a pinned manifest (names, sizes,
//! sha256), and a downloader that fetches it once — with the user's
//! consent, never from a package. Downloads run through a `curl`
//! subprocess (same thin-client pattern as capture); progress is the
//! byte count on disk, so the UI just polls [`downloaded_bytes`].

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};

/// Directory name under the frontend's models dir.
pub const DIR_NAME: &str = "parakeet-tdt-0.6b-v3-int8";

const BASE_URL: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    sha256: &'static str,
}

/// Pinned to the upstream revision current as of 2026-07-20; hashes
/// are Hugging Face's own LFS checksums (vocab.txt hashed by hand —
/// it is not an LFS file).
pub const FILES: &[ModelFile] = &[
    ModelFile {
        name: "encoder-model.int8.onnx",
        size: 652_183_999,
        sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
    },
    ModelFile {
        name: "decoder_joint-model.int8.onnx",
        size: 18_202_004,
        sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
    },
    ModelFile {
        name: "nemo128.onnx",
        size: 139_764,
        sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
    },
    ModelFile {
        name: "vocab.txt",
        size: 93_939,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
];

#[must_use]
pub fn total_bytes() -> u64 {
    FILES.iter().map(|file| file.size).sum()
}

/// Every file present at its exact manifest size.
#[must_use]
pub fn is_complete(dir: &Path) -> bool {
    FILES.iter().all(|file| {
        dir.join(file.name)
            .metadata()
            .is_ok_and(|meta| meta.len() == file.size)
    })
}

/// Bytes on disk so far, finished files and the in-flight `.part`
/// alike — the UI's progress numerator.
#[must_use]
pub fn downloaded_bytes(dir: &Path) -> u64 {
    FILES
        .iter()
        .map(|file| {
            let done = dir.join(file.name).metadata().map_or(0, |meta| meta.len());
            let part = dir
                .join(format!("{}.part", file.name))
                .metadata()
                .map_or(0, |meta| meta.len());
            done.max(part)
        })
        .sum()
}

#[derive(Clone, Debug)]
pub enum DownloadState {
    Running,
    Done,
    Failed(String),
}

/// A running model download. Files land as `.part`, are checksummed,
/// then renamed — a crash mid-download never leaves a file that
/// passes [`is_complete`]. Dropping cancels.
pub struct Download {
    state: Arc<Mutex<DownloadState>>,
    current: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<Mutex<bool>>,
}

impl Download {
    pub fn start(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let state = Arc::new(Mutex::new(DownloadState::Running));
        let current: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(Mutex::new(false));
        {
            let dir = dir.to_owned();
            let state = state.clone();
            let current = current.clone();
            let cancelled = cancelled.clone();
            std::thread::spawn(move || {
                let result = run(&dir, &current, &cancelled);
                if let Ok(mut state) = state.lock() {
                    *state = match result {
                        Ok(()) => DownloadState::Done,
                        Err(err) => DownloadState::Failed(format!("{err:#}")),
                    };
                }
            });
        }
        Ok(Self {
            state,
            current,
            cancelled,
        })
    }

    #[must_use]
    pub fn state(&self) -> DownloadState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or(DownloadState::Failed("download state poisoned".to_owned()))
    }

    fn cancel(&self) {
        if let Ok(mut cancelled) = self.cancelled.lock() {
            *cancelled = true;
        }
        if let Ok(mut current) = self.current.lock()
            && let Some(child) = current.as_mut()
        {
            let _ = child.kill();
        }
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run(dir: &Path, current: &Mutex<Option<Child>>, cancelled: &Mutex<bool>) -> Result<()> {
    for file in FILES {
        let dest = dir.join(file.name);
        if dest.metadata().is_ok_and(|meta| meta.len() == file.size) {
            continue; // resumed download: this one already landed
        }
        let part = dir.join(format!("{}.part", file.name));
        fetch(&format!("{BASE_URL}/{}", file.name), &part, current)?;
        if cancelled.lock().is_ok_and(|flag| *flag) {
            let _ = std::fs::remove_file(&part);
            bail!("cancelled");
        }
        verify(&part, file)?;
        std::fs::rename(&part, &dest)
            .with_context(|| format!("moving {} into place", file.name))?;
        tracing::info!("voice model: downloaded {}", file.name);
    }
    Ok(())
}

/// One file via curl, parked in `current` so `cancel` can kill it
/// mid-transfer. `-C -` resumes a partial `.part` from an earlier
/// attempt; the checksum still gates the rename.
fn fetch(url: &str, part: &Path, current: &Mutex<Option<Child>>) -> Result<()> {
    let child = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "-C",
            "-",
        ])
        .arg("--output")
        .arg(part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running curl")?;
    match current.lock() {
        Ok(mut slot) => *slot = Some(child),
        Err(_) => bail!("download bookkeeping poisoned"),
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let Ok(mut slot) = current.lock() else {
            bail!("download bookkeeping poisoned");
        };
        let Some(child) = slot.as_mut() else {
            bail!("download cancelled");
        };
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                let mut child = slot.take().ok_or_else(|| anyhow!("download cancelled"))?;
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(pipe) = child.stderr.as_mut() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                bail!("curl failed for {url}: {}", stderr.trim());
            }
            Err(err) => {
                let _ = slot.take();
                bail!("curl vanished: {err}");
            }
        }
    }
}

fn verify(path: &Path, file: &ModelFile) -> Result<()> {
    let size = path
        .metadata()
        .with_context(|| format!("checking {}", file.name))?
        .len();
    if size != file.size {
        bail!("{}: got {size} bytes, expected {}", file.name, file.size);
    }
    let output = Command::new("sha256sum")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("running sha256sum")?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", file.name);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let digest = stdout.split_whitespace().next().unwrap_or_default();
    if digest != file.sha256 {
        bail!("{}: checksum mismatch — refusing the file", file.name);
    }
    Ok(())
}
