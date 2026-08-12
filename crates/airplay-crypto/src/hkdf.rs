//! HKDF-SHA512 helpers for HAP key derivation.
//!
//! Salt/info strings per HAP convention, e.g.
//! `HKDF-SHA512("Control-Salt", "Control-Write-Encryption-Key", K, 32)`.
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1389-1397]

use sha2::Sha512;

/// Derive `len` bytes: HKDF-SHA512(salt_str, info_str, ikm).
pub fn hkdf_sha512(salt: &str, info: &str, ikm: &[u8], len: usize) -> Vec<u8> {
    let hk = hkdf::Hkdf::<Sha512>::new(Some(salt.as_bytes()), ikm);
    let mut out = vec![0u8; len];
    hk.expand(info.as_bytes(), &mut out)
        .expect("HKDF expand: output length within limits");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5869 does not cover SHA-512 vectors here; assert basic properties
    /// instead: determinism, length, and domain separation by info string.
    #[test]
    fn deterministic_and_domain_separated() {
        let k1 = hkdf_sha512("Control-Salt", "Control-Write-Encryption-Key", b"secret", 32);
        let k2 = hkdf_sha512("Control-Salt", "Control-Write-Encryption-Key", b"secret", 32);
        let k3 = hkdf_sha512("Control-Salt", "Control-Read-Encryption-Key", b"secret", 32);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
        assert_ne!(k1, k3);
    }
}
