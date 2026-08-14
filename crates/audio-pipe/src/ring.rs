//! Bounded interleaved f32 ring. Overflow drops the oldest samples.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct SampleRing {
    inner: Mutex<VecDeque<f32>>,
    max: Mutex<usize>,
    pub rate: AtomicU32,
    pub disc: AtomicU64,
    pub drops: AtomicU64,
    /// IAudioEndpointVolume scalar as f32 bits. 1.0 = full (loopback tap is
    /// typically pre-master-volume). [evidence: MS Volume Controls; spec §11]
    pub endpoint_gain: AtomicU32,
}

impl SampleRing {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            max: Mutex::new(48000 * 2 * 64 / 1000),
            rate: AtomicU32::new(0),
            disc: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            endpoint_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    pub fn set_endpoint_gain(&self, gain: f32) {
        self.endpoint_gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn endpoint_gain(&self) -> f32 {
        f32::from_bits(self.endpoint_gain.load(Ordering::Relaxed))
    }

    pub fn set_format(&self, rate: u32) {
        self.rate.store(rate, Ordering::SeqCst);
        let cap = (rate as usize) * 2 * 64 / 1000;
        *self.max.lock().unwrap() = cap.max(1024);
    }

    pub fn push_stereo(&self, samples: &[f32]) {
        let cap = *self.max.lock().unwrap();
        let mut q = self.inner.lock().unwrap();
        for &s in samples {
            if q.len() >= cap {
                q.pop_front();
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
            q.push_back(s);
        }
    }

    pub fn len_frames(&self) -> usize {
        self.inner.lock().unwrap().len() / 2
    }

    pub fn pop_stereo_frames(&self, frames: usize, dest: &mut [f32]) -> usize {
        let need = frames * 2;
        if dest.len() < need {
            return 0;
        }
        let mut q = self.inner.lock().unwrap();
        if q.len() < need {
            return 0;
        }
        for slot in dest.iter_mut().take(need) {
            *slot = q.pop_front().unwrap_or(0.0);
        }
        frames
    }
}

impl Default for SampleRing {
    fn default() -> Self {
        Self::new()
    }
}

/// 352-frame PCM packets. Overflow drops the oldest packet so capture/process
/// never block on a slow send pump.
pub struct PacketQueue {
    inner: Mutex<VecDeque<Vec<i16>>>,
    cap: usize,
    pub drops: AtomicU64,
}

impl PacketQueue {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            cap: cap.max(1),
            drops: AtomicU64::new(0),
        }
    }

    pub fn push(&self, pkt: Vec<i16>) {
        let mut q = self.inner.lock().unwrap();
        while q.len() >= self.cap {
            q.pop_front();
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(pkt);
    }

    pub fn pop(&self) -> Option<Vec<i16>> {
        self.inner.lock().unwrap().pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_queue_drops_oldest() {
        let q = PacketQueue::new(2);
        q.push(vec![1]);
        q.push(vec![2]);
        q.push(vec![3]);
        assert_eq!(q.drops.load(Ordering::Relaxed), 1);
        assert_eq!(q.pop(), Some(vec![2]));
        assert_eq!(q.pop(), Some(vec![3]));
        assert_eq!(q.pop(), None);
    }
}
