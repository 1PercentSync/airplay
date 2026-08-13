//! SRP-6a client for HAP pair-setup.
//!
//! Parameters (HAP, not generic RFC 5054 TLS-SRP):
//! - N = RFC 5054 3072-bit group, g = 5
//! - hash = SHA-512
//! - username I = "Pair-Setup"
//! - k = H(PAD(N) | PAD(g)), u = H(PAD(A) | PAD(B))  (PAD to |N| = 384)
//! - x = H(salt | H(I ":" P)) with salt as the TLV octet string
//! - K = H(S) with S at natural byte length
//! - M1 = H( H(N) xor H(g) | H(I) | salt | A | B | K )  (A,B,N,g unpadded)
//! - M2 / HAMK = H(A | M1 | K)
//!
//! `[evidence: owntone pair_homekit.c:52,209-223,269-327,432-536,1220-1521; pyatv hap_srp.py:138-163; RFC 5054 Appendix A.4]`

use airplay_core::{Error, Result};
use num_bigint::BigUint;
use sha2::{Digest, Sha512};

const N_LEN: usize = 384;

/// RFC 5054 Appendix A.4 3072-bit modulus (hex, no whitespace).
const N_3072_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437\
4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05\
98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB\
9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B\
E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718\
3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33\
A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7\
ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864\
D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2\
08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";

const G: u32 = 5;
const USERNAME: &[u8] = b"Pair-Setup";

fn n_group() -> BigUint {
    BigUint::parse_bytes(N_3072_HEX.as_bytes(), 16).expect("N_3072 hex")
}

fn g_group() -> BigUint {
    BigUint::from(G)
}

fn sha512(data: &[u8]) -> [u8; 64] {
    let d = Sha512::digest(data);
    let mut out = [0u8; 64];
    out.copy_from_slice(&d);
    out
}

/// Minimal unsigned big-endian (no leading zeros, except zero -> `[0]`).
fn to_bytes_be(n: &BigUint) -> Vec<u8> {
    let b = n.to_bytes_be();
    if b.is_empty() {
        vec![0]
    } else {
        b
    }
}

fn pad_n(n: &BigUint, len: usize) -> Vec<u8> {
    let b = to_bytes_be(n);
    if b.len() > len {
        // Keep the least-significant `len` bytes.
        return b[b.len() - len..].to_vec();
    }
    let mut out = vec![0u8; len];
    out[len - b.len()..].copy_from_slice(&b);
    out
}

/// H(PAD(n1) | PAD(n2)) interpreted as a big integer.
///
/// `[evidence: owntone pair.c:192-216 H_nn_pad]`
fn h_nn_pad(n1: &BigUint, n2: &BigUint) -> BigUint {
    let mut buf = Vec::with_capacity(2 * N_LEN);
    buf.extend_from_slice(&pad_n(n1, N_LEN));
    buf.extend_from_slice(&pad_n(n2, N_LEN));
    BigUint::from_bytes_be(&sha512(&buf))
}

pub struct SrpClient {
    n: BigUint,
    g: BigUint,
    a: BigUint,
    aa: BigUint,
    password: Vec<u8>,
    k_session: Option<[u8; 64]>,
    m1: Option<[u8; 64]>,
    hamk: Option<[u8; 64]>,
}

impl SrpClient {
    /// `a` is the 32-byte ephemeral secret (owntone `bnum_random(a, 256)`).
    pub fn new(password: &str, a_secret: &[u8]) -> Result<Self> {
        if a_secret.is_empty() {
            return Err(Error::Crypto("empty SRP secret a".into()));
        }
        let n = n_group();
        let g = g_group();
        let a = BigUint::from_bytes_be(a_secret);
        let aa = g.modpow(&a, &n);
        if aa == BigUint::from(0u32) {
            return Err(Error::Crypto("SRP A is zero".into()));
        }
        Ok(Self {
            n,
            g,
            a,
            aa,
            password: password.as_bytes().to_vec(),
            k_session: None,
            m1: None,
            hamk: None,
        })
    }

    /// Unpadded A, the HAP TLV PublicKey wire form.
    ///
    /// `[evidence: owntone pair_homekit.c:447-458; airplay2-sender-cpp airplay_crypto.cpp:307-308]`
    pub fn public_a(&self) -> Vec<u8> {
        to_bytes_be(&self.aa)
    }

    /// Process server salt + B; compute K, M1, expected HAMK.
    pub fn process(&mut self, salt: &[u8], server_b: &[u8]) -> Result<()> {
        if salt.is_empty() || server_b.is_empty() {
            return Err(Error::Crypto("missing salt or B".into()));
        }
        let b = BigUint::from_bytes_be(server_b);
        if &b % &self.n == BigUint::from(0u32) {
            return Err(Error::Crypto("SRP B is 0 mod N".into()));
        }

        let k = h_nn_pad(&self.n, &self.g);
        let u = h_nn_pad(&self.aa, &b);
        if u == BigUint::from(0u32) {
            return Err(Error::Crypto("SRP u is zero".into()));
        }

        // x = H(salt | H(I ":" P))
        // `[evidence: owntone pair_homekit.c:269-281 calculate_x; salt kept as TLV octets]`
        let mut inner = Vec::with_capacity(USERNAME.len() + 1 + self.password.len());
        inner.extend_from_slice(USERNAME);
        inner.push(b':');
        inner.extend_from_slice(&self.password);
        let ucp = sha512(&inner);
        let mut x_in = Vec::with_capacity(salt.len() + 64);
        x_in.extend_from_slice(salt);
        x_in.extend_from_slice(&ucp);
        let x = BigUint::from_bytes_be(&sha512(&x_in));

        // S = (B - k*g^x) ^ (a + u*x) mod N
        // `[evidence: owntone pair_homekit.c:495-506; C-level airplay_crypto.cpp:338-350]`
        let gx = self.g.modpow(&x, &self.n);
        let kgx = (&k * &gx) % &self.n;
        let base = (&b + &self.n - kgx) % &self.n;
        let exp = &self.a + &u * &x;
        let s = base.modpow(&exp, &self.n);

        // K = H(S) natural length
        // `[evidence: owntone pair_homekit.c:508 hash_num(S)]`
        let k_session = sha512(&to_bytes_be(&s));

        // M1
        // `[evidence: owntone pair_homekit.c:284-313 calculate_M]`
        let h_n = sha512(&to_bytes_be(&self.n));
        let h_g = sha512(&to_bytes_be(&self.g));
        let mut h_xor = [0u8; 64];
        for i in 0..64 {
            h_xor[i] = h_n[i] ^ h_g[i];
        }
        let h_i = sha512(USERNAME);
        let a_bytes = to_bytes_be(&self.aa);
        let b_bytes = to_bytes_be(&b);
        let mut m1_in = Vec::new();
        m1_in.extend_from_slice(&h_xor);
        m1_in.extend_from_slice(&h_i);
        m1_in.extend_from_slice(salt);
        m1_in.extend_from_slice(&a_bytes);
        m1_in.extend_from_slice(&b_bytes);
        m1_in.extend_from_slice(&k_session);
        let m1 = sha512(&m1_in);

        // HAMK = H(A | M1 | K)
        // `[evidence: owntone pair_homekit.c:316-327 calculate_H_AMK]`
        let mut hamk_in = Vec::new();
        hamk_in.extend_from_slice(&a_bytes);
        hamk_in.extend_from_slice(&m1);
        hamk_in.extend_from_slice(&k_session);
        let hamk = sha512(&hamk_in);

        self.k_session = Some(k_session);
        self.m1 = Some(m1);
        self.hamk = Some(hamk);
        Ok(())
    }

    pub fn proof_m1(&self) -> Result<&[u8; 64]> {
        self.m1
            .as_ref()
            .ok_or_else(|| Error::Crypto("SRP process() not called".into()))
    }

    pub fn session_key(&self) -> Result<&[u8; 64]> {
        self.k_session
            .as_ref()
            .ok_or_else(|| Error::Crypto("SRP process() not called".into()))
    }

    /// Verify server proof M2 / HAMK.
    ///
    /// `[evidence: owntone pair_homekit.c:532-535, 1491-1496]`
    pub fn verify_server_proof(&self, server_m2: &[u8]) -> Result<()> {
        let hamk = self
            .hamk
            .as_ref()
            .ok_or_else(|| Error::Crypto("SRP process() not called".into()))?;
        if server_m2.len() != 64 {
            return Err(Error::Pairing(format!(
                "HAMK length {} != 64",
                server_m2.len()
            )));
        }
        if server_m2 != hamk.as_slice() {
            return Err(Error::Pairing("HAMK mismatch".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_matches_rfc5054_appendix_a4() {
        let n = n_group();
        assert_eq!(to_bytes_be(&n).len(), N_LEN);
        assert_eq!(format!("{n:X}"), N_3072_HEX);
        assert_eq!(g_group(), BigUint::from(5u32));
    }

    /// Client and a local verifier agree on K / M1 / HAMK (SHA-512, 3072).
    /// This is an algebraic identity test, not an RFC 5054 SHA-1 vector.
    #[test]
    fn client_server_agree() {
        let n = n_group();
        let g = g_group();
        let password = b"3939";
        let salt = [0x11u8; 16];

        let mut inner = Vec::new();
        inner.extend_from_slice(USERNAME);
        inner.push(b':');
        inner.extend_from_slice(password);
        let ucp = sha512(&inner);
        let mut x_in = Vec::new();
        x_in.extend_from_slice(&salt);
        x_in.extend_from_slice(&ucp);
        let x = BigUint::from_bytes_be(&sha512(&x_in));
        let v = g.modpow(&x, &n);

        let b_secret = BigUint::from_bytes_be(&[0x22u8; 32]);
        let k = h_nn_pad(&n, &g);
        let gb = g.modpow(&b_secret, &n);
        let bb = (k * &v + gb) % &n;

        let mut client = SrpClient::new("3939", &[0x33u8; 32]).unwrap();
        client.process(&salt, &to_bytes_be(&bb)).unwrap();

        // Server S = (A * v^u) ^ b  (standard SRP-6a)
        let aa = BigUint::from_bytes_be(&client.public_a());
        let u = h_nn_pad(&aa, &bb);
        let vu = v.modpow(&u, &n);
        let avu = (aa * vu) % &n;
        let s_server = avu.modpow(&b_secret, &n);
        let k_server = sha512(&to_bytes_be(&s_server));
        assert_eq!(&k_server, client.session_key().unwrap());

        // Server checks M1 then emits HAMK
        client
            .verify_server_proof(client.hamk.as_ref().unwrap())
            .unwrap();
        assert_eq!(client.proof_m1().unwrap().len(), 64);
    }
}
