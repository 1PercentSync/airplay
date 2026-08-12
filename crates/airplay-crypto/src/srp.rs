//! SRP-6a client (3072-bit group, SHA-512) for HAP pair-setup.
//!
//! Byte conventions follow the HAP variant exactly:
//!   k = SHA512(PAD(N) ‖ PAD(g)), u = SHA512(PAD(A) ‖ PAD(B))  (384-byte pads)
//!   x = SHA512(salt ‖ SHA512("Pair-Setup" ":" pin))
//!   S = (B − k·g^x)^(a+u·x) mod N ; K = SHA512(S)
//!   M1  = SHA512((SHA512(N)⊕SHA512(g)) ‖ SHA512("Pair-Setup") ‖ salt ‖ A ‖ B ‖ K)
//!   HAMK = SHA512(A ‖ M1 ‖ K)
//! Values hashed into x/K/M1/HAMK use minimal big-endian encoding.
//!
//! [evidence: airplay-cli/src/ap2_hap.c:340-474 — conventions verified
//!  against real devices ("matching pair_ap/owntone")]

use num_bigint::BigUint;
use sha2::{Digest, Sha512};

const N_BYTES: usize = 384; // 3072-bit modulus
const HASH_LEN: usize = 64; // SHA-512
const USERNAME: &str = "Pair-Setup";

/// RFC 5054 3072-bit group modulus.
///
/// [evidence: airplay-cli/src/ap2_hap.c:82-97]
const SRP_N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
    "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437",
    "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05",
    "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB",
    "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718",
    "3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33",
    "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864",
    "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
    "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"
);

#[derive(Debug, thiserror::Error)]
pub enum SrpError {
    #[error("server public key B is zero mod N")]
    InvalidServerKey,
    #[error("scrambling parameter u is zero")]
    ZeroScrambler,
    #[error("RNG failure: {0}")]
    Rng(String),
}

/// Result of the client-side SRP computation (M3 inputs + expected proofs).
pub struct SrpClientSession {
    /// Our public key A (minimal big-endian).
    pub a_pub: Vec<u8>,
    /// Client proof M1 (sent in M3).
    pub proof_m1: [u8; HASH_LEN],
    /// Expected server proof HAMK (verified against M4's Proof).
    pub expected_hamk: [u8; HASH_LEN],
    /// Session key K = SHA512(S) — 64 bytes; audio/control keys derive from it.
    pub session_key: [u8; HASH_LEN],
}

fn sha512(parts: &[&[u8]]) -> [u8; HASH_LEN] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Left-pad a big-endian number to `len` bytes (RFC 5054 PAD()).
fn pad_be(mut v: Vec<u8>, len: usize) -> Vec<u8> {
    if v.len() < len {
        let mut out = vec![0u8; len - v.len()];
        out.append(&mut v);
        out
    } else {
        v
    }
}

fn modulus() -> BigUint {
    BigUint::parse_bytes(SRP_N_HEX.as_bytes(), 16).expect("valid hex modulus")
}

/// Compute client-side SRP-6a from the device's salt + public key B and PIN.
pub fn compute_client(
    salt: &[u8],
    b_pub: &[u8],
    pin: &str,
) -> Result<SrpClientSession, SrpError> {
    let n = modulus();
    let g = BigUint::from(5u32);
    let b = BigUint::from_bytes_be(b_pub);

    // Safety: reject B == 0 (mod N) [evidence: ap2_hap.c:387-391]
    if (&b % &n) == BigUint::from(0u32) {
        return Err(SrpError::InvalidServerKey);
    }

    // a: 256-bit random; A = g^a mod N
    let mut a_bytes = [0u8; 32];
    getrandom::fill(&mut a_bytes).map_err(|e| SrpError::Rng(e.to_string()))?;
    let a = BigUint::from_bytes_be(&a_bytes);
    let a_pub_bn = g.modpow(&a, &n);
    let a_pub = a_pub_bn.to_bytes_be();

    // k = SHA512(PAD(N) ‖ PAD(g)); u = SHA512(PAD(A) ‖ PAD(B))
    let k_digest = sha512(&[
        &pad_be(n.to_bytes_be(), N_BYTES),
        &pad_be(g.to_bytes_be(), N_BYTES),
    ]);
    let k = BigUint::from_bytes_be(&k_digest);
    let u_digest = sha512(&[
        &pad_be(a_pub.clone(), N_BYTES),
        &pad_be(b_pub.to_vec(), N_BYTES),
    ]);
    let u = BigUint::from_bytes_be(&u_digest);
    if u == BigUint::from(0u32) {
        return Err(SrpError::ZeroScrambler);
    }

    // x = SHA512(salt ‖ SHA512("Pair-Setup:pin"))
    let userpass = sha512(&[format!("{USERNAME}:{pin}").as_bytes()]);
    let x = BigUint::from_bytes_be(&sha512(&[salt, &userpass]));

    // S = (B − k·g^x)^(a+u·x) mod N
    let gx = g.modpow(&x, &n);
    let kgx = (&k * &gx) % &n;
    let base = if b >= kgx { &b - &kgx } else { &b + &n - &kgx };
    let exp = &a + &u * &x;
    let s = base.modpow(&exp, &n);

    // K = SHA512(S) (minimal big-endian)
    let session_key = sha512(&[&s.to_bytes_be()]);

    // M1 = SHA512((H(N)⊕H(g)) ‖ H(user) ‖ salt ‖ A ‖ B ‖ K)
    let h_n = sha512(&[&n.to_bytes_be()]);
    let h_g = sha512(&[&g.to_bytes_be()]);
    let h_xor: Vec<u8> = h_n.iter().zip(h_g.iter()).map(|(x, y)| x ^ y).collect();
    let h_user = sha512(&[USERNAME.as_bytes()]);
    let proof_m1 = sha512(&[
        &h_xor,
        &h_user,
        salt,
        &a_pub,
        b_pub,
        &session_key,
    ]);

    // HAMK = SHA512(A ‖ M1 ‖ K)
    let expected_hamk = sha512(&[&a_pub, &proof_m1, &session_key]);

    Ok(SrpClientSession {
        a_pub,
        proof_m1,
        expected_hamk,
        session_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-consistency: simulate the server side and require agreement on S.
    /// Server: v = g^x, B = k·v + g^b; checks client M1 and S equality.
    #[test]
    fn client_server_agree_on_session_key() {
        let n = modulus();
        let g = BigUint::from(5u32);
        let salt = b"0123456789abcdef";
        let pin = "3939";

        // Server derives v from the same x.
        let userpass = sha512(&[format!("{USERNAME}:{pin}").as_bytes()]);
        let x = BigUint::from_bytes_be(&sha512(&[salt.as_ref(), &userpass]));
        let v = g.modpow(&x, &n);

        // Server keypair b/B.
        let mut b_bytes = [0u8; 32];
        getrandom::fill(&mut b_bytes).unwrap();
        let b = BigUint::from_bytes_be(&b_bytes);
        let k_digest = sha512(&[
            &pad_be(n.to_bytes_be(), N_BYTES),
            &pad_be(g.to_bytes_be(), N_BYTES),
        ]);
        let k = BigUint::from_bytes_be(&k_digest);
        let gb = g.modpow(&b, &n);
        let b_pub_bn = (&k * &v + &gb) % &n;
        let b_pub = b_pub_bn.to_bytes_be();

        // Client.
        let client = compute_client(salt, &b_pub, pin).unwrap();

        // Server: u, S = (A·v^u)^b mod N, K.
        let a_pub_bn = BigUint::from_bytes_be(&client.a_pub);
        let u_digest = sha512(&[
            &pad_be(client.a_pub.clone(), N_BYTES),
            &pad_be(b_pub.clone(), N_BYTES),
        ]);
        let u = BigUint::from_bytes_be(&u_digest);
        let vu = v.modpow(&u, &n);
        let s_server = (&a_pub_bn * &vu).modpow(&b, &n);
        let k_server = sha512(&[&s_server.to_bytes_be()]);

        assert_eq!(client.session_key, k_server, "session keys must agree");

        // Server verifies M1 with the shared formula.
        let h_n = sha512(&[&n.to_bytes_be()]);
        let h_g = sha512(&[&g.to_bytes_be()]);
        let h_xor: Vec<u8> = h_n.iter().zip(h_g.iter()).map(|(x, y)| x ^ y).collect();
        let h_user = sha512(&[USERNAME.as_bytes()]);
        let m1_server = sha512(&[
            &h_xor,
            &h_user,
            salt.as_ref(),
            &client.a_pub,
            &b_pub,
            &k_server,
        ]);
        assert_eq!(client.proof_m1, m1_server, "M1 must verify server-side");

        // Client's expected HAMK matches server-computed proof.
        let hamk_server = sha512(&[&client.a_pub, &m1_server, &k_server]);
        assert_eq!(client.expected_hamk, hamk_server);
    }

    #[test]
    fn rejects_zero_server_key() {
        let n = modulus();
        let b = n.to_bytes_be(); // B == N ≡ 0 (mod N)
        assert!(compute_client(b"salt", &b, "3939").is_err());
    }
}
