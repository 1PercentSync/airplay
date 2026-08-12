//! Audio pipeline: source (sine / WASAPI loopback) → resample → TPDF dither
//! → 16-bit interleaved blocks of exactly `spf` frames for the RTP pump.
//!
//! Quality discipline (research/04 §4): capture at the device's NATIVE mix
//! rate (no Windows SRC), rubato sinc to 44100, TPDF dither at quantization.

pub mod process;
pub mod source;

#[cfg(windows)]
pub mod wasapi;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use process::Processor;

/// An audio block: exactly `spf * 2` interleaved i16 samples.
pub type AudioBlock = Vec<i16>;

#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    #[error("source failed: {0}")]
    Source(String),
    #[error("resampler: {0}")]
    Resample(String),
    #[error("no virtual audio device found (looked for: {candidates:?}); available: {available:?}")]
    NoDevice {
        candidates: Vec<String>,
        available: Vec<String>,
    },
    #[error("unsupported mix format: {0} (need float32 stereo)")]
    UnsupportedFormat(String),
}

/// Audio source selection.
pub enum SourceKind {
    /// 440 Hz sine (diagnostics; proves the chain before trusting music).
    Sine { freq_hz: f64, rate: u32 },
    /// WASAPI loopback on a virtual device (name substring; None = auto).
    Wasapi { device: Option<String> },
}

/// Pipeline statistics (diagnostics contract).
#[derive(Default)]
pub struct PipeStats {
    pub captured_frames: AtomicU64,
    pub discontinuities: AtomicU64,
    pub dropped_blocks: AtomicU64,
    pub blocks_produced: AtomicU64,
}

/// Running pipeline; drop/Shutdown stops the threads.
pub struct PipeHandle {
    shutdown: Arc<AtomicBool>,
    pub stats: Arc<PipeStats>,
}

impl PipeHandle {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start the pipeline. Returns the block receiver for the pump + a handle.
pub fn start(kind: SourceKind, spf: usize) -> Result<(mpsc::Receiver<AudioBlock>, PipeHandle), PipeError> {
    let (out_tx, out_rx) = mpsc::channel::<AudioBlock>(64);
    let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(PipeStats::default());

    let src_rate = match &kind {
        SourceKind::Sine { rate, .. } => *rate,
        SourceKind::Wasapi { .. } => 48000, // virtual devices are 48k f32 stereo (probe-verified)
    };
    let mut processor = Processor::new(src_rate, spf)?;
    let chunk_frames = processor.input_chunk_frames();

    // ---- source thread ----
    {
        let shutdown = shutdown.clone();
        let kind_name = match &kind {
            SourceKind::Sine { .. } => "sine",
            SourceKind::Wasapi { .. } => "wasapi",
        };
        std::thread::Builder::new()
            .name(format!("audio-source-{kind_name}"))
            .spawn(move || match kind {
                SourceKind::Sine { freq_hz, rate } => {
                    let mut sine = source::SineSource::new(freq_hz, rate, 0.5);
                    let tick = Duration::from_millis(10);
                    while !shutdown.load(Ordering::Relaxed) {
                        let chunk = sine.next_chunk(chunk_frames);
                        if chunk_tx.send(chunk).is_err() {
                            return;
                        }
                        std::thread::sleep(tick);
                    }
                }
                SourceKind::Wasapi { device } => {
                    #[cfg(windows)]
                    {
                        let cap_stats = Arc::new(wasapi::CaptureStats::default());
                        if let Err(e) = wasapi::capture_thread(device, chunk_frames, chunk_tx, cap_stats, shutdown) {
                            tracing::error!(?e, "wasapi capture failed");
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = device;
                        tracing::error!("wasapi source is only available on Windows");
                    }
                }
            })
            .map_err(|e| PipeError::Source(e.to_string()))?;
    }

    // ---- processor thread ----
    {
        let stats = stats.clone();
        std::thread::Builder::new()
            .name("audio-processor".into())
            .spawn(move || {
                while let Ok(chunk) = chunk_rx.recv() {
                    processor.feed(&chunk, &out_tx);
                }
                stats
                    .dropped_blocks
                    .store(processor.dropped, Ordering::Relaxed);
            })
            .map_err(|e| PipeError::Source(e.to_string()))?;
    }

    Ok((out_rx, PipeHandle { shutdown, stats }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sine_pipeline_produces_blocks() {
        let (mut rx, handle) = start(
            SourceKind::Sine {
                freq_hz: 440.0,
                rate: 48000,
            },
            352,
        )
        .unwrap();
        let mut n = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while n < 20 && tokio::time::Instant::now() < deadline {
            if let Some(b) = rx.recv().await {
                assert_eq!(b.len(), 704);
                n += 1;
            }
        }
        handle.shutdown();
        assert!(n >= 20, "got {n} blocks in 2s");
    }
}
