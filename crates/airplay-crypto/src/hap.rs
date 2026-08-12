//! HAP encrypted channel framing (ChaCha20-Poly1305).
//!
//! Wire format per frame: `[2B LE plaintext_len][ciphertext][16B tag]`,
//! plaintext ≤ 1024 bytes; nonce = 4 zero bytes ‖ 8B LE per-direction
//! counter; AAD = the 2 length bytes.
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:610-615, 695-740;
//!  airplay_crypto.cpp:155-159 (counterNonce8, LE), 171-179 (pad12)]

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

pub const MAX_PLAINTEXT: usize = 1024;
pub const TAG_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum HapError {
    #[error("decrypt failed (tag mismatch — wrong key or nonce desync)")]
    Decrypt,
    #[error("frame length field {0} exceeds maximum {1}")]
    Oversize(usize, usize),
}

/// Bidirectional HAP channel state: independent keys + counters per direction.
pub struct HapChannel {
    write_key: [u8; 32],
    read_key: [u8; 32],
    write_ctr: u64,
    read_ctr: u64,
}

fn nonce12(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::try_from(&n[..]).expect("12-byte nonce")
}

impl HapChannel {
    pub fn new(write_key: [u8; 32], read_key: [u8; 32]) -> Self {
        Self {
            write_key,
            read_key,
            write_ctr: 0,
            read_ctr: 0,
        }
    }

    /// Encrypt `plaintext` into one or more wire frames (split at 1024).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plaintext.len() + 64);
        for chunk in plaintext.chunks(MAX_PLAINTEXT) {
            let cipher = ChaCha20Poly1305::new((&self.write_key).into());
            let len = chunk.len() as u16;
            let aad = len.to_le_bytes();
            let ct = cipher
                .encrypt(
                    &nonce12(self.write_ctr),
                    Payload {
                        msg: chunk,
                        aad: &aad,
                    },
                )
                .expect("chacha20poly1305 encrypt is infallible");
            self.write_ctr += 1;
            out.extend_from_slice(&aad);
            out.extend_from_slice(&ct);
        }
        out
    }

    /// Encrypt a single frame (payload must already be ≤ MAX_PLAINTEXT).
    pub fn encrypt_frame(&mut self, plaintext: &[u8]) -> Vec<u8> {
        assert!(plaintext.len() <= MAX_PLAINTEXT);
        self.encrypt(plaintext)
    }

    /// Decrypt one frame: `len_prefix` = 2 AAD bytes, `body` = ciphertext+tag.
    pub fn decrypt(&mut self, len_prefix: [u8; 2], body: &[u8]) -> Result<Vec<u8>, HapError> {
        let declared = u16::from_le_bytes(len_prefix) as usize;
        if declared > MAX_PLAINTEXT {
            return Err(HapError::Oversize(declared, MAX_PLAINTEXT));
        }
        let cipher = ChaCha20Poly1305::new((&self.read_key).into());
        let pt = cipher
            .decrypt(
                &nonce12(self.read_ctr),
                Payload {
                    msg: body,
                    aad: &len_prefix,
                },
            )
            .map_err(|_| HapError::Decrypt)?;
        self.read_ctr += 1;
        Ok(pt)
    }

    /// Current read counter (diagnostics: nonce desync detection).
    pub fn read_counter(&self) -> u64 {
        self.read_ctr
    }
}

/// All keys derived from the SRP session key K (64 bytes) after transient
/// pair-setup. Convention: only the FIRST 32 bytes of K are used as the
/// shared secret input (HKDF ikm and the audio cipher key).
///
/// [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1565-1580,
///  1692-1709 (key_len 64 → first 32 for audio & shk)]
pub struct DerivedKeys {
    /// Control channel (RTSP): we encrypt with write, decrypt with read.
    pub control: HapChannel,
    /// Event channel (reverse connection): receiver's pushes decrypt with
    /// "Events-Write" key, our responses encrypt with "Events-Read" key.
    pub events_write_key: [u8; 32],
    pub events_read_key: [u8; 32],
    /// Audio payload cipher key == plist `shk` value (no HKDF).
    pub audio_key: [u8; 32],
}

/// [evidence: airplay-cli/src/ap2_hap.c:1172-1190] — HKDF input is the
/// FULL 64-byte SRP session key ("matching pair_ap/owntone"); the audio key
/// is separately the first 32 bytes of the session key.
pub fn derive_keys(session_key: &[u8; 64]) -> DerivedKeys {
    let ikm = session_key.as_slice();
    let ctrl_write = crate::hkdf::hkdf_sha512("Control-Salt", "Control-Write-Encryption-Key", ikm, 32);
    let ctrl_read = crate::hkdf::hkdf_sha512("Control-Salt", "Control-Read-Encryption-Key", ikm, 32);
    let ev_in = crate::hkdf::hkdf_sha512("Events-Salt", "Events-Write-Encryption-Key", ikm, 32);
    let ev_out = crate::hkdf::hkdf_sha512("Events-Salt", "Events-Read-Encryption-Key", ikm, 32);
    let k = |v: Vec<u8>| -> [u8; 32] { v.try_into().expect("HKDF-32") };
    DerivedKeys {
        control: HapChannel::new(k(ctrl_write), k(ctrl_read)),
        events_write_key: k(ev_in),
        events_read_key: k(ev_out),
        audio_key: session_key[..32].try_into().expect("audio key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small_and_split() {
        let mut a = HapChannel::new([0x11; 32], [0x22; 32]);
        let mut b = HapChannel::new([0x22; 32], [0x11; 32]); // mirror

        // Small message.
        let wire = a.encrypt(b"hello");
        assert_eq!(&wire[..2], &5u16.to_le_bytes());
        let pt = b.decrypt([5, 0], &wire[2..]).unwrap();
        assert_eq!(pt, b"hello");

        // Large message → two frames (1024 + 976).
        let big = vec![0xAB; 2000];
        let wire = a.encrypt(&big);
        let f1_len = 2 + 1024 + 16;
        let f2_len = 2 + (2000 - 1024) + 16;
        assert_eq!(wire.len(), f1_len + f2_len);
        let l1: [u8; 2] = wire[..2].try_into().unwrap();
        let pt1 = b.decrypt(l1, &wire[2..f1_len]).unwrap();
        let l2: [u8; 2] = wire[f1_len..f1_len + 2].try_into().unwrap();
        let pt2 = b.decrypt(l2, &wire[f1_len + 2..]).unwrap();
        assert_eq!([pt1, pt2].concat(), big);
    }

    #[test]
    fn tampered_tag_fails() {
        let mut a = HapChannel::new([0x11; 32], [0x22; 32]);
        let mut b = HapChannel::new([0x22; 32], [0x11; 32]);
        let mut wire = a.encrypt(b"secret");
        let last = wire.len() - 1;
        wire[last] ^= 1;
        assert!(b.decrypt([6, 0], &wire[2..]).is_err());
    }

    #[test]
    fn nonce_counter_progresses() {
        // Same key+plaintext twice must give different ciphertext (counter++).
        let mut a = HapChannel::new([0x11; 32], [0x22; 32]);
        let w1 = a.encrypt(b"same");
        let w2 = a.encrypt(b"same");
        assert_ne!(w1, w2);
    }
}
