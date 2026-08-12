//! Platform-independent processing: f32 stereo @ `src_rate` → rubato sinc →
//! 44100 → TPDF dither → i16 blocks of `spf` frames.
//!
//! [evidence: research/04 §4 (rubato + TPDF decision);
//!  rubato 5.x Async::new_sinc usage — Context7 /henquist/rubato]

use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use tokio::sync::mpsc;

use crate::AudioBlock;

pub const TARGET_RATE: u32 = 44100;

/// xorshift64 PRNG for TPDF dither (no crypto randomness needed).
struct XorShift(u64);

impl XorShift {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }

    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f32 {
        self.next_u32() as f32 / 4294967296.0
    }
}

/// The processing stage: consumes interleaved f32 stereo chunks at `src_rate`
/// from `in_rx`, produces 16-bit blocks of `spf` frames into `out_tx`.
/// Runs until `in_rx` closes; drops NEW blocks when `out_tx` is full (the
/// pump's silence discipline fills the gap; counted via `dropped`).
pub struct Processor {
    resampler: Async<f32>,
    /// Pending resampled f32 (interleaved) awaiting block formation.
    pending: Vec<f32>,
    spf: usize,
    dither: XorShift,
    pub dropped: u64,
}

impl Processor {
    pub fn new(src_rate: u32, spf: usize) -> Result<Self, crate::PipeError> {
        let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
            .oversampling_factor(256)
            .interpolation(SincInterpolationType::Quadratic);
        // Input chunk = 10 ms at the source rate.
        let chunk = (src_rate / 100) as usize;
        let resampler = Async::<f32>::new_sinc(
            TARGET_RATE as f64 / src_rate as f64,
            1.1,
            &params,
            chunk,
            2,
            FixedAsync::Input,
        )
        .map_err(|e| crate::PipeError::Resample(e.to_string()))?;
        Ok(Self {
            resampler,
            pending: Vec::with_capacity(spf * 4),
            spf,
            dither: XorShift(0x9E3779B97F4A7C15),
            dropped: 0,
        })
    }

    pub fn input_chunk_frames(&self) -> usize {
        self.resampler.input_frames_next()
    }

    /// Feed one chunk of interleaved f32 (len = 2 * input_chunk_frames()).
    pub fn feed(&mut self, chunk: &[f32], out_tx: &mpsc::Sender<AudioBlock>) {
        use audioadapter_buffers::direct::InterleavedSlice;
        let frames = chunk.len() / 2;
        let cap = self.resampler.output_frames_max();
        let mut out = vec![0.0f32; cap * 2];
        let input = match InterleavedSlice::new(chunk, 2, frames) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(?e, "resampler input adapter failed");
                return;
            }
        };
        let mut output = match InterleavedSlice::new_mut(&mut out, 2, cap) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(?e, "resampler output adapter failed");
                return;
            }
        };
        let (_, written) = match self
            .resampler
            .process_into_buffer(&input, &mut output, None)
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(?e, "resample failed");
                return;
            }
        };
        self.pending.extend_from_slice(&out[..written * 2]);

        while self.pending.len() >= self.spf * 2 {
            let block: Vec<f32> = self.pending.drain(..self.spf * 2).collect();
            let block = self.quantize(&block);
            if out_tx.try_send(block).is_err() {
                self.dropped += 1;
            }
        }
    }

    /// f32 [-1,1] → i16 with TPDF dither (±1 LSB triangular).
    fn quantize(&mut self, samples: &[f32]) -> AudioBlock {
        samples
            .iter()
            .map(|&x| {
                let d = self.dither.uniform() - self.dither.uniform();
                let v = (x * 32768.0 + d).round();
                v.clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SineSource;

    #[tokio::test]
    async fn processes_sine_at_rate_with_continuity() {
        let spf = 352usize;
        let (out_tx, mut out_rx) = mpsc::channel::<AudioBlock>(128);
        let mut proc_ = Processor::new(48000, spf).unwrap();
        let chunk_frames = proc_.input_chunk_frames();
        let mut sine = SineSource::new(440.0, 48000, 0.5);

        // ~0.5 s of source audio.
        for _ in 0..50 {
            let chunk = sine.next_chunk(chunk_frames);
            proc_.feed(&chunk, &out_tx);
        }
        drop(out_tx);

        let mut blocks = 0usize;
        let mut prev_last = None;
        let mut discontinuities = 0usize;
        while let Some(b) = out_rx.recv().await {
            assert_eq!(b.len(), spf * 2);
            // Zero-crossing sanity: sine must have sign changes within blocks.
            if let Some(p) = prev_last {
                if (p >= 0) != (b[0] >= 0) {
                    discontinuities += 1;
                }
            }
            prev_last = Some(*b.last().unwrap());
            blocks += 1;
        }
        // 0.5 s ≈ 62 blocks; resampler latency trims a few.
        assert!(blocks >= 50, "blocks={blocks}");
        assert!(discontinuities > 0, "sine should cross zero between blocks");
    }

    #[tokio::test]
    async fn quantizes_full_scale_without_overflow() {
        let (out_tx, mut out_rx) = mpsc::channel::<AudioBlock>(16);
        let mut proc_ = Processor::new(44100, 352).unwrap(); // 1:1 rate
        let chunk_frames = proc_.input_chunk_frames();
        let chunk = vec![1.0f32; chunk_frames * 2];
        for _ in 0..10 {
            proc_.feed(&chunk, &out_tx);
        }
        drop(out_tx);
        while let Some(b) = out_rx.recv().await {
            assert!(b.iter().all(|&s| (i16::MIN..=i16::MAX).contains(&s)));
        }
    }
}
