//! RTP audio encrypt, sync 0xD4, timing 0xD3, retransmit 0xD6 wrap.
//!
//! [evidence: owntone airplay.c:1916-1948,2262-2338,2347-2394;
//! pyatv stream_client.py:107-168,581-587; raop_sender.cpp:1842-2002]

use crate::alac::encode_alac_frame;
use crate::ntp::{ntp_now, ntp_parts, ts2ntp};
use airplay_core::{Error, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};

pub const SAMPLE_RATE: u32 = 44100;
pub const FRAMES_PER_PACKET: usize = 352;
/// Safe default lead (1.5 s). Also the last tray preset.
pub const LATENCY_FRAMES: u32 = 22050 + SAMPLE_RATE;
/// Tray / config presets: 120/180/240/300/360 ms, then the 1.5 s default.
pub const LATENCY_PRESETS: [u32; 6] = [5292, 7938, 10584, 13230, 15876, LATENCY_FRAMES];
pub const BACKLOG: usize = 512;

/// SETUP window for a lead. 66150 keeps 11025/88200; every other preset is min 0, max = lead * 2.
pub fn latency_window(lead: u32) -> (u32, u32) {
    if lead == LATENCY_FRAMES {
        (11025, 88200)
    } else {
        (0, lead.saturating_mul(2))
    }
}

/// Snap a persisted or typed value onto the nearest preset.
pub fn nearest_latency_preset(lead: u32) -> u32 {
    LATENCY_PRESETS
        .into_iter()
        .min_by_key(|p| p.abs_diff(lead))
        .unwrap_or(LATENCY_FRAMES)
}

/// Tray / log label, e.g. `120 ms (5292 frames)`.
pub fn latency_preset_label(lead: u32) -> String {
    let ms = u64::from(lead) * 10 / 441;
    format!("{ms} ms ({lead} frames)")
}

pub fn rtp_header(first: bool, seq: u16, rtptime: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = 0x80;
    h[1] = if first { 0xE0 } else { 0x60 };
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&rtptime.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// nonce[4..6] = seq as host-endian uint16 (LE on Windows/x86); trailing 8 = nonce[4..12].
/// [evidence: owntone airplay.c:1929-1948]
pub fn encrypt_audio(shk: &[u8; 32], header: &[u8; 12], pcm: &[i16], seq: u16) -> Result<Vec<u8>> {
    let alac = encode_alac_frame(pcm);
    let mut nonce = [0u8; 12];
    nonce[4..6].copy_from_slice(&seq.to_le_bytes());
    let cipher = ChaCha20Poly1305::new(shk.into());
    let ct = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &alac,
                aad: &header[4..12],
            },
        )
        .map_err(|_| Error::Crypto("audio chacha encrypt failed".into()))?;
    let mut out = Vec::with_capacity(12 + ct.len() + 8);
    out.extend_from_slice(header);
    out.extend_from_slice(&ct);
    out.extend_from_slice(&nonce[4..12]);
    Ok(out)
}

pub fn sync_packet(first: bool, rtptime: u32, head_ts: u64, lead: u32) -> [u8; 20] {
    let mut p = [0u8; 20];
    p[0] = if first { 0x90 } else { 0x80 };
    p[1] = 0xD4;
    p[2..4].copy_from_slice(&0x0007u16.to_be_bytes());
    let pos = rtptime.wrapping_sub(lead);
    p[4..8].copy_from_slice(&pos.to_be_bytes());
    let (sec, frac) = ntp_parts(ts2ntp(head_ts, SAMPLE_RATE));
    p[8..12].copy_from_slice(&sec.to_be_bytes());
    p[12..16].copy_from_slice(&frac.to_be_bytes());
    p[16..20].copy_from_slice(&rtptime.to_be_bytes());
    p
}

pub fn timing_reply(req: &[u8; 32]) -> [u8; 32] {
    let mut res = [0u8; 32];
    res[0] = 0x80;
    res[1] = 0xd3;
    res[2] = req[2];
    res[8..16].copy_from_slice(&req[24..32]);
    let (sec, frac) = ntp_parts(ntp_now());
    res[16..20].copy_from_slice(&sec.to_be_bytes());
    res[20..24].copy_from_slice(&frac.to_be_bytes());
    let (sec2, frac2) = ntp_parts(ntp_now());
    res[24..28].copy_from_slice(&sec2.to_be_bytes());
    res[28..32].copy_from_slice(&frac2.to_be_bytes());
    res
}

pub fn retransmit_wrap(seq: u16, original: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + original.len());
    out.extend_from_slice(&[0x80, 0xD6]);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(original);
    out
}

pub struct Backlog {
    slots: Vec<Option<(u16, Vec<u8>)>>,
}

impl Backlog {
    pub fn new() -> Self {
        Self {
            slots: vec![None; BACKLOG],
        }
    }

    pub fn store(&mut self, seq: u16, pkt: Vec<u8>) {
        let i = seq as usize & (BACKLOG - 1);
        self.slots[i] = Some((seq, pkt));
    }

    pub fn get(&self, seq: u16) -> Option<&[u8]> {
        let i = seq as usize & (BACKLOG - 1);
        match &self.slots[i] {
            Some((s, p)) if *s == seq => Some(p),
            _ => None,
        }
    }
}

impl Default for Backlog {
    fn default() -> Self {
        Self::new()
    }
}
