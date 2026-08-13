//! HAP control-channel HKDF keys and ChaCha20-Poly1305 frames.
//!
//! `docs/协议实现规范.md` §3.1, §4, §16.5.
//! [evidence: owntone pair_homekit.c:107-108,876-932,2944-3051; pair.h:66;
//! owntone airplay.c:1005-1045,1438-1473;
//! pyatv hap_srp.py:32-41; hap_session.py:17-66; chacha20.py:12-73;
//! pyatv auth/__init__.py:36-38,107-115; hap_transient.py:91-99;
//! shairport-sync rtsp.c:483-545; pair_homekit.c:109-110,2942-2964;
//! airplay2-receiver hap.py:1359-1506; ap2-receiver.py:1181-1188,1267-1272;
//! raop_sender.cpp:579-605,1394-1402; airplay_crypto.cpp:131-189]

use airplay_core::{Error, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use sha2::Sha512;

pub const FRAME_MAX: usize = 1024;
const TAG_LEN: usize = 16;

const SALT: &[u8] = b"Control-Salt";
const WRITE_INFO: &[u8] = b"Control-Write-Encryption-Key";
const READ_INFO: &[u8] = b"Control-Read-Encryption-Key";

fn hkdf32(ikm: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha512>::new(Some(SALT), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .map_err(|_| Error::Crypto("HKDF expand failed".into()))?;
    Ok(okm)
}

fn nonce12(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..12].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}

pub struct HapCipher {
    write: ChaCha20Poly1305,
    read: ChaCha20Poly1305,
    send_ctr: u64,
    recv_ctr: u64,
}

impl HapCipher {
    /// Client control channel. IKM must be the full 64-byte SRP K.
    pub fn control(ikm: &[u8]) -> Result<Self> {
        if ikm.len() != 64 {
            return Err(Error::Crypto(format!(
                "control IKM must be 64 bytes, got {}",
                ikm.len()
            )));
        }
        let w = hkdf32(ikm, WRITE_INFO)?;
        let r = hkdf32(ikm, READ_INFO)?;
        Ok(Self {
            write: ChaCha20Poly1305::new((&w).into()),
            read: ChaCha20Poly1305::new((&r).into()),
            send_ctr: 0,
            recv_ctr: 0,
        })
    }

    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.is_empty() {
            return Err(Error::Crypto("HAP encrypt empty plaintext".into()));
        }
        let mut out = Vec::new();
        let mut off = 0;
        while off < plaintext.len() {
            let n = (plaintext.len() - off).min(FRAME_MAX);
            let chunk = &plaintext[off..off + n];
            let lenb = (n as u16).to_le_bytes();
            let nonce = nonce12(self.send_ctr);
            let ct = self
                .write
                .encrypt(
                    &nonce,
                    Payload {
                        msg: chunk,
                        aad: &lenb,
                    },
                )
                .map_err(|_| Error::Crypto("HAP encrypt failed".into()))?;
            self.send_ctr += 1;
            out.extend_from_slice(&lenb);
            out.extend_from_slice(&ct);
            off += n;
        }
        Ok(out)
    }

    /// One complete wire frame after the 2-byte length: `n` ciphertext bytes + 16-byte tag.
    pub fn decrypt_frame(&mut self, n_le: [u8; 2], ct_and_tag: &[u8]) -> Result<Vec<u8>> {
        let n = u16::from_le_bytes(n_le) as usize;
        if n > FRAME_MAX {
            return Err(Error::Crypto(format!("HAP frame plaintext {n} > {FRAME_MAX}")));
        }
        if ct_and_tag.len() != n + TAG_LEN {
            return Err(Error::Crypto(format!(
                "HAP frame size {} != n+16 {}",
                ct_and_tag.len(),
                n + TAG_LEN
            )));
        }
        let nonce = nonce12(self.recv_ctr);
        let plain = self
            .read
            .decrypt(
                &nonce,
                Payload {
                    msg: ct_and_tag,
                    aad: &n_le,
                },
            )
            .map_err(|_| Error::Crypto("HAP decrypt failed".into()))?;
        self.recv_ctr += 1;
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8439 §2.8.2 IETF ChaCha20-Poly1305 AEAD vector.
    /// [evidence: research/references/rfc8439_aead.md]
    /// Proves the crate is the IETF construction, not that HomePod accepts our frames.
    #[test]
    fn rfc8439_section_2_8_2() {
        let key: [u8; 32] = [
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
            0x9c, 0x9d, 0x9e, 0x9f,
        ];
        let nonce = Nonce::from([
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ]);
        let aad = [
            0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ];
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let want = [
            0xd3, 0x1a, 0x8d, 0x34, 0x64, 0x8e, 0x60, 0xdb, 0x7b, 0x86, 0xaf, 0xbc, 0x53, 0xef,
            0x7e, 0xc2, 0xa4, 0xad, 0xed, 0x51, 0x29, 0x6e, 0x08, 0xfe, 0xa9, 0xe2, 0xb5, 0xa7,
            0x36, 0xee, 0x62, 0xd6, 0x3d, 0xbe, 0xa4, 0x5e, 0x8c, 0xa9, 0x67, 0x12, 0x82, 0xfa,
            0xfb, 0x69, 0xda, 0x92, 0x72, 0x8b, 0x1a, 0x71, 0xde, 0x0a, 0x9e, 0x06, 0x0b, 0x29,
            0x05, 0xd6, 0xa5, 0xb6, 0x7e, 0xcd, 0x3b, 0x36, 0x92, 0xdd, 0xbd, 0x7f, 0x2d, 0x77,
            0x8b, 0x8c, 0x98, 0x03, 0xae, 0xe3, 0x28, 0x09, 0x1b, 0x58, 0xfa, 0xb3, 0x24, 0xe4,
            0xfa, 0xd6, 0x75, 0x94, 0x55, 0x85, 0x80, 0x8b, 0x48, 0x31, 0xd7, 0xbc, 0x3f, 0xf4,
            0xde, 0xf0, 0x8e, 0x4b, 0x7a, 0x9d, 0xe5, 0x76, 0xd2, 0x65, 0x86, 0xce, 0xc6, 0x4b,
            0x61, 0x16, 0x1a, 0xe1, 0x0b, 0x59, 0x4f, 0x09, 0xe2, 0x6a, 0x7e, 0x90, 0x2e, 0xcb,
            0xd0, 0x60, 0x06, 0x91,
        ];
        let cipher = ChaCha20Poly1305::new((&key).into());
        let got = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: pt,
                    aad: &aad,
                },
            )
            .expect("encrypt");
        assert_eq!(got, want);
    }
}
