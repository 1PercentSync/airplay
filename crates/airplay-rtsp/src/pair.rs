//! Transient HAP pair-setup (M1–M4) on an existing RTSP connection.
//!
//! `[evidence: owntone pair_homekit.c:1220-1521 + airplay.c:3696-3698; pyatv hap_transient.py:45-82; spec §2]`

use airplay_core::{Error, Result, TRANSIENT_PIN};
use airplay_crypto::srp::SrpClient;
use airplay_crypto::tlv8::{TlvMap, TlvValue};
use tracing::{info, warn};

use crate::client::RtspClient;

const PAIR_HEADERS: &[(&str, &str)] = &[
    ("X-Apple-HKP", "4"),
    ("Content-Type", "application/octet-stream"),
];

pub struct PairResult {
    /// 64-byte SRP K. Needed later as HKDF IKM; probe only checks HAMK.
    pub session_key: [u8; 64],
}

/// Run M1–M4 on `rtsp`. Leaves the TCP connection in the post-pair plaintext
/// state (encryption is not enabled here).
pub async fn pair_transient(rtsp: &mut RtspClient, a_secret: &[u8]) -> Result<PairResult> {
    let mut srp = SrpClient::new(TRANSIENT_PIN, a_secret)?;

    // M1: State=1, Method=0, Flags=0x10. Field order: State, Method, Flags (owntone).
    // `[evidence: owntone pair_homekit.c:1249-1257; pyatv hap_transient.py:51-56; true device 06: order is not semantic]`
    let mut m1 = TlvMap::new();
    m1.insert_u8(TlvValue::State, 0x01);
    m1.insert_u8(TlvValue::Method, 0x00);
    m1.insert_u8(TlvValue::Flags, 0x10);
    info!("pair M1 send {}", m1.summary());
    let r1 = rtsp
        .exchange("POST", "/pair-setup", PAIR_HEADERS, &m1.encode())
        .await?;
    if !r1.is_success() {
        return Err(Error::Pairing(format!(
            "M1 status {} {} (transient refused?)",
            r1.status, r1.reason
        )));
    }
    let t2 = TlvMap::decode(&r1.body)?;
    info!("pair M2 recv {}", t2.summary());
    if let Some(e) = t2.error_code() {
        return Err(Error::Pairing(format!("M2 TLV error {e}")));
    }
    let salt = t2
        .get(TlvValue::Salt)
        .ok_or_else(|| Error::Pairing("M2 missing salt".into()))?;
    let pk_b = t2
        .get(TlvValue::PublicKey)
        .ok_or_else(|| Error::Pairing("M2 missing public key".into()))?;
    if salt.len() != 16 {
        warn!("M2 salt length {} (expected 16)", salt.len());
    }

    srp.process(salt, pk_b)?;

    // M3: State=3, PublicKey=A, Proof=M1
    // `[evidence: owntone pair_homekit.c:1297-1299; pyatv hap_transient.py:73-77]`
    let mut m3 = TlvMap::new();
    m3.insert_u8(TlvValue::State, 0x03);
    m3.insert(TlvValue::PublicKey, srp.public_a());
    m3.insert(TlvValue::Proof, srp.proof_m1()?.to_vec());
    info!("pair M3 send {}", m3.summary());
    let r3 = rtsp
        .exchange("POST", "/pair-setup", PAIR_HEADERS, &m3.encode())
        .await?;
    if !r3.is_success() {
        return Err(Error::Pairing(format!(
            "M3 status {} {}",
            r3.status, r3.reason
        )));
    }
    let t4 = TlvMap::decode(&r3.body)?;
    info!("pair M4 recv {}", t4.summary());
    if let Some(e) = t4.error_code() {
        return Err(Error::Pairing(format!("M4 TLV error {e}")));
    }
    let proof = t4
        .get(TlvValue::Proof)
        .ok_or_else(|| Error::Pairing("M4 missing proof (HAMK)".into()))?;
    srp.verify_server_proof(proof)?;
    info!("pair HAMK ok, session_key_len=64");

    Ok(PairResult {
        session_key: *srp.session_key()?,
    })
}
