//! Audio sources: a portable sine-wave generator (diagnostics + non-Windows
//! testing) and the WASAPI loopback capture (Windows-only, in `wasapi`).

/// 440 Hz sine at a configurable sample rate/amplitude (stereo, both channels).
pub struct SineSource {
    phase: f64,
    step: f64,
    amplitude: f32,
}

impl SineSource {
    pub fn new(freq_hz: f64, sample_rate: u32, amplitude: f32) -> Self {
        Self {
            phase: 0.0,
            step: freq_hz * 2.0 * std::f64::consts::PI / sample_rate as f64,
            amplitude,
        }
    }

    /// Next chunk of interleaved f32 (len = frames * 2).
    pub fn next_chunk(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let s = (self.phase.sin() as f32) * self.amplitude;
            out.push(s);
            out.push(s);
            self.phase += self.step;
            if self.phase > 2.0 * std::f64::consts::PI {
                self.phase -= 2.0 * std::f64::consts::PI;
            }
        }
        out
    }
}
