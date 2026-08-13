//! rubato sinc 48k→44.1k (or native→44.1) plus TPDF to i16, 352-frame packets.
//! [evidence: research/05; docs/协议实现规范.md §12.2]
//!
//! TPDF uses a CSPRNG-seeded xorshift, not `fill_random` per sample. Per-sample
//! BCrypt on Windows stalled this thread; the capture ring then overflowed.

use crate::ring::{PacketQueue, SampleRing};
use airplay_crypto::fill_random;
use airplay_stream::FRAMES_PER_PACKET;
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{error, info};

pub fn spawn_processor(ring: Arc<SampleRing>, packets: Arc<PacketQueue>) {
    thread::Builder::new()
        .name("audio-process".into())
        .spawn(move || {
            if let Err(e) = process_loop(ring, packets) {
                error!("audio processor stopped: {e}");
            }
        })
        .expect("spawn audio-process");
}

fn process_loop(ring: Arc<SampleRing>, packets: Arc<PacketQueue>) -> airplay_core::Result<()> {
    let mut rate = 0u32;
    for _ in 0..200 {
        rate = ring.rate.load(Ordering::SeqCst);
        if rate > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    if rate == 0 {
        return Err(airplay_core::Error::Audio(
            "capture never published mix rate".into(),
        ));
    }
    info!(rate, "processor mix rate");

    let mut rng = XorShift64::seeded()?;
    let target = 44100u32;
    let mut resampler = if rate == target {
        None
    } else {
        let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
            .oversampling_factor(256)
            .interpolation(SincInterpolationType::Quadratic);
        let ratio = f64::from(target) / f64::from(rate);
        Some(
            Async::<f32>::new_sinc(ratio, 1.1, &params, 1024, 2, FixedAsync::Input)
                .map_err(|e| airplay_core::Error::Audio(format!("rubato: {e}")))?,
        )
    };

    let mut acc = Vec::<i16>::new();
    let mut in_buf = vec![0.0f32; 1024 * 2];
    loop {
        match &mut resampler {
            None => {
                if ring.pop_stereo_frames(FRAMES_PER_PACKET, &mut in_buf) == FRAMES_PER_PACKET {
                    apply_endpoint_gain(&ring, &mut in_buf[..FRAMES_PER_PACKET * 2]);
                    packets.push(tpdf_packet(&mut rng, &in_buf[..FRAMES_PER_PACKET * 2]));
                } else {
                    thread::sleep(Duration::from_millis(4));
                }
            }
            Some(rs) => {
                let need = rs.input_frames_next();
                if ring.pop_stereo_frames(need, &mut in_buf) != need {
                    thread::sleep(Duration::from_millis(4));
                    continue;
                }
                apply_endpoint_gain(&ring, &mut in_buf[..need * 2]);
                let input = InterleavedSlice::new(&in_buf[..need * 2], 2, need)
                    .map_err(|e| airplay_core::Error::Audio(format!("adapter in: {e:?}")))?;
                let owned = rs
                    .process(&input, None)
                    .map_err(|e| airplay_core::Error::Audio(format!("resample: {e}")))?;
                let data = owned.take_data();
                acc.extend_from_slice(&tpdf_packet(&mut rng, &data));
                while acc.len() >= FRAMES_PER_PACKET * 2 {
                    let pkt: Vec<i16> = acc.drain(..FRAMES_PER_PACKET * 2).collect();
                    packets.push(pkt);
                }
            }
        }
    }
}

struct XorShift64(u64);

impl XorShift64 {
    fn seeded() -> airplay_core::Result<Self> {
        let mut b = [0u8; 8];
        fill_random(&mut b)?;
        let mut s = u64::from_le_bytes(b);
        if s == 0 {
            s = 0x9E37_79B9_7F4A_7C15;
        }
        Ok(Self(s))
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }

    fn tpdf(&mut self) -> f32 {
        let a = (self.next_u32() >> 16) as f32 / 65535.0;
        let b = (self.next_u32() >> 16) as f32 / 65535.0;
        a - b
    }
}

fn apply_endpoint_gain(ring: &SampleRing, samples: &mut [f32]) {
    let g = ring.endpoint_gain();
    if (g - 1.0).abs() < 1e-4 {
        return;
    }
    for s in samples {
        *s *= g;
    }
}

fn tpdf_packet(rng: &mut XorShift64, samples: &[f32]) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples.len());
    for &s in samples {
        let x = (s * 32767.0 + rng.tpdf()).round().clamp(-32768.0, 32767.0);
        out.push(x as i16);
    }
    out
}
