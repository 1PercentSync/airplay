//! HAP SRP-6a client: RFC 5054 3072-bit N, g=5, SHA-512, I=`Pair-Setup`.
//!
//! Formulas: `docs/协议实现规范.md` §16.2.
//! [evidence: owntone pair.c:192-254; pair_homekit.c:269-327, 432-536;
//! raop_sender airplay_crypto.cpp:266-398; airplay2-receiver srp.py:32-121;
//! RFC 5054 A.4 in research/references/rfc5054_srp.md]

use airplay_core::{Error, Result, PAIR_USERNAME, TRANSIENT_PIN};
use num_bigint::{BigInt, BigUint, Sign};
use sha2::{Digest, Sha512};
use std::sync::OnceLock;

use crate::random::fill_random;

pub const N_BYTES: usize = 384;

const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B",
    "139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485",
    "B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1F",
    "E649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23",
    "DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32",
    "905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF69558",
    "17183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521",
    "ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D7",
    "1E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B1817",
    "7B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82",
    "D120A93AD2CAFFFFFFFFFFFFFFFF",
);

static N: OnceLock<BigUint> = OnceLock::new();
static G: OnceLock<BigUint> = OnceLock::new();

fn n() -> &'static BigUint {
    N.get_or_init(|| BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("RFC 5054 3072-bit N"))
}

fn g() -> &'static BigUint {
    G.get_or_init(|| BigUint::from(5u32))
}

fn sha512(data: &[u8]) -> [u8; 64] {
    Sha512::digest(data).into()
}

/// Natural-length unsigned bytes (leading zeros stripped). Zero → empty,
/// matching OpenSSL `BN_num_bytes(0) == 0` used by owntone `hash_num`.
fn mpi_bytes(n: &BigUint) -> Vec<u8> {
    if n.bits() == 0 {
        Vec::new()
    } else {
        n.to_bytes_be()
    }
}

fn pad_left(n: &BigUint, len: usize) -> Vec<u8> {
    let b = mpi_bytes(n);
    if b.len() > len {
        panic!("SRP PAD operand longer than N");
    }
    let mut out = vec![0u8; len - b.len()];
    out.extend_from_slice(&b);
    out
}

/// RFC 5054 PAD: both operands left-padded to `N_BYTES`, then SHA-512.
fn h_nn_pad(n1: &BigUint, n2: &BigUint) -> BigUint {
    let mut buf = pad_left(n1, N_BYTES);
    buf.extend_from_slice(&pad_left(n2, N_BYTES));
    BigUint::from_bytes_be(&sha512(&buf))
}

fn hash_num(n: &BigUint) -> [u8; 64] {
    sha512(&mpi_bytes(n))
}

fn calculate_x(salt: &[u8], username: &str, password: &[u8]) -> BigUint {
    let mut inner = Sha512::new();
    inner.update(username.as_bytes());
    inner.update(b":");
    inner.update(password);
    let ucp = inner.finalize();
    // salt as bignum then strip leading zeros [evidence: owntone pair.c H_ns + BN_bn2bin]
    let salt_bn = BigUint::from_bytes_be(salt);
    let mut buf = mpi_bytes(&salt_bn);
    buf.extend_from_slice(&ucp);
    BigUint::from_bytes_be(&sha512(&buf))
}

pub struct SrpClient {
    a: BigUint,
    a_pub: BigUint,
    username: String,
    password: Vec<u8>,
    session_key: Option<[u8; 64]>,
    m1: Option<[u8; 64]>,
    hamk: Option<[u8; 64]>,
}

impl SrpClient {
    pub fn new() -> Result<Self> {
        Self::with_password(TRANSIENT_PIN)
    }

    pub fn with_password(password: &str) -> Result<Self> {
        let mut a_bytes = [0u8; 32];
        fill_random(&mut a_bytes)?;
        Self::from_a(BigUint::from_bytes_be(&a_bytes), password)
    }

    fn from_a(a: BigUint, password: &str) -> Result<Self> {
        let a_pub = g().modpow(&a, n());
        Ok(Self {
            a,
            a_pub,
            username: PAIR_USERNAME.to_string(),
            password: password.as_bytes().to_vec(),
            session_key: None,
            m1: None,
            hamk: None,
        })
    }

    /// A on the wire: natural length, not padded to 384.
    /// [evidence: owntone pair_homekit.c:447-458]
    pub fn public_a(&self) -> Vec<u8> {
        mpi_bytes(&self.a_pub)
    }

    pub fn process_challenge(&mut self, salt: &[u8], server_b: &[u8]) -> Result<()> {
        if salt.len() != 16 {
            return Err(Error::Srp(format!(
                "salt must be 16 bytes, got {}",
                salt.len()
            )));
        }
        if server_b.len() > N_BYTES {
            return Err(Error::Srp(format!("B longer than {N_BYTES} bytes")));
        }
        let b = BigUint::from_bytes_be(server_b);
        let b_mod = &b % n();
        if b_mod.bits() == 0 {
            return Err(Error::Srp("B ≡ 0 (mod N)".into()));
        }
        let k = h_nn_pad(n(), g());
        let u = h_nn_pad(&self.a_pub, &b);
        if u.bits() == 0 {
            return Err(Error::Srp("u == 0".into()));
        }
        let x = calculate_x(salt, &self.username, &self.password);
        // S = (B - k*g^x) ^ (a + u*x)  mod N, base reduced mod N first
        // [evidence: raop_sender airplay_crypto.cpp:338-350; RFC 5054]
        let gx = g().modpow(&x, n());
        let kgx = (&k * &gx) % n();
        let base_i =
            BigInt::from_biguint(Sign::Plus, b.clone()) - BigInt::from_biguint(Sign::Plus, kgx);
        let n_i = BigInt::from_biguint(Sign::Plus, n().clone());
        let mut base_i = base_i % &n_i;
        if base_i.sign() == Sign::Minus {
            base_i += &n_i;
        }
        let base = base_i
            .to_biguint()
            .ok_or_else(|| Error::Srp("S base not unsigned".into()))?;
        let exp = &self.a + &u * &x;
        let s = base.modpow(&exp, n());
        let k_sess = hash_num(&s);

        let mut h_xor = hash_num(n());
        let h_g = hash_num(g());
        for (xbyte, gbyte) in h_xor.iter_mut().zip(h_g.iter()) {
            *xbyte ^= *gbyte;
        }
        let h_i = sha512(self.username.as_bytes());
        let mut m1_in = Vec::new();
        m1_in.extend_from_slice(&h_xor);
        m1_in.extend_from_slice(&h_i);
        m1_in.extend_from_slice(&mpi_bytes(&BigUint::from_bytes_be(salt)));
        m1_in.extend_from_slice(&mpi_bytes(&self.a_pub));
        m1_in.extend_from_slice(&mpi_bytes(&b));
        m1_in.extend_from_slice(&k_sess);
        let m1: [u8; 64] = sha512(&m1_in);

        let mut hamk_in = mpi_bytes(&self.a_pub);
        hamk_in.extend_from_slice(&m1);
        hamk_in.extend_from_slice(&k_sess);
        let hamk: [u8; 64] = sha512(&hamk_in);

        self.session_key = Some(k_sess);
        self.m1 = Some(m1);
        self.hamk = Some(hamk);
        Ok(())
    }

    pub fn proof_m1(&self) -> Result<&[u8; 64]> {
        self.m1
            .as_ref()
            .ok_or_else(|| Error::Srp("process_challenge not called".into()))
    }

    pub fn session_key(&self) -> Result<&[u8; 64]> {
        self.session_key
            .as_ref()
            .ok_or_else(|| Error::Srp("process_challenge not called".into()))
    }

    /// Ordinary `==` (LAN + public PIN). User: do not change to constant-time.
    pub fn verify_hamk(&self, server: &[u8]) -> Result<()> {
        let hamk = self
            .hamk
            .as_ref()
            .ok_or_else(|| Error::Srp("process_challenge not called".into()))?;
        if server != hamk.as_slice() {
            return Err(Error::Srp("HAMK mismatch".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_is_3072_bits() {
        assert_eq!(n().bits(), 3072);
        assert_eq!(mpi_bytes(n()).len(), 384);
    }

    #[test]
    fn reject_b_zero() {
        let mut client = SrpClient::from_a(BigUint::from(7u32), TRANSIENT_PIN).unwrap();
        let err = client.process_challenge(&[0u8; 16], &[0u8]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("B ≡ 0"), "{msg}");
    }
}
