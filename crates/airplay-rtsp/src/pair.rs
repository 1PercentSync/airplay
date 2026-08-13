//! Transient HAP pair-setup M1–M4 on RTSP/1.0 POST `/pair-setup`.
//!
//! Selected: owntone M1 order State, Method, Flags; skip `/pair-pin-start`;
//! verify HAMK; PIN 3939; HKP 4.
//! [evidence: owntone pair_homekit.c:1219-1258,1291-1299,1422-1514;
//! owntone airplay.c:2836-2838,3697-3698; pyatv hap_transient.py:49-82
//! (pin-start and discard-M4 not followed); airplay2-receiver hap.py:513-541]

use crate::rtsp::{Identity, RtspClient};
use airplay_core::{Error, Result, HKP_TRANSIENT};
use airplay_crypto::srp::SrpClient;
use airplay_crypto::tlv8::{self, TlvType, FLAG_TRANSIENT};
use std::net::SocketAddr;
use tracing::info;

pub async fn transient_pair(addr: SocketAddr, identity: Identity) -> Result<[u8; 64]> {
    let mut rtsp = RtspClient::connect(addr, identity).await?;
    let mut srp = SrpClient::new()?;

    let m1 = tlv8::encode(&[
        (TlvType::STATE, &[1]),
        (TlvType::METHOD, &[0]),
        (TlvType::FLAGS, &[FLAG_TRANSIENT]),
    ]);
    info!("M1 send State=1 Method=0 Flags={}", FLAG_TRANSIENT);
    let r2 = pair_post(&mut rtsp, &m1).await?;
    let t2 = tlv8::decode(&r2)?;
    if let Some(err) = tlv8::get_u8(&t2, TlvType::ERROR) {
        return Err(Error::Pair(format!("M2 TLV error {err}")));
    }
    let salt = tlv8::get(&t2, TlvType::SALT).ok_or_else(|| Error::Pair("M2 missing Salt".into()))?;
    let pk_b =
        tlv8::get(&t2, TlvType::PUBLIC_KEY).ok_or_else(|| Error::Pair("M2 missing PublicKey".into()))?;
    let st = tlv8::get_u8(&t2, TlvType::STATE).unwrap_or(0);
    info!(
        "M2 recv State={st} Salt={}B PublicKey={}B",
        salt.len(),
        pk_b.len()
    );

    srp.process_challenge(salt, pk_b)?;
    let a = srp.public_a();
    let m1_proof = srp.proof_m1()?;
    info!(
        "M3 send State=3 PublicKey={}B Proof={}B",
        a.len(),
        m1_proof.len()
    );
    let m3 = tlv8::encode(&[
        (TlvType::STATE, &[3]),
        (TlvType::PUBLIC_KEY, &a),
        (TlvType::PROOF, m1_proof.as_slice()),
    ]);
    let r4 = pair_post(&mut rtsp, &m3).await?;
    let t4 = tlv8::decode(&r4)?;
    if let Some(err) = tlv8::get_u8(&t4, TlvType::ERROR) {
        return Err(Error::Pair(format!("M4 TLV error {err}")));
    }
    let proof =
        tlv8::get(&t4, TlvType::PROOF).ok_or_else(|| Error::Pair("M4 missing Proof".into()))?;
    let st4 = tlv8::get_u8(&t4, TlvType::STATE).unwrap_or(0);
    info!("M4 recv State={st4} Proof={}B", proof.len());
    srp.verify_hamk(proof)?;
    info!("HAMK ok");
    let key = *srp.session_key()?;
    info!(session_key_len = key.len(), "transient pairing complete");
    Ok(key)
}

async fn pair_post(rtsp: &mut RtspClient, body: &[u8]) -> Result<Vec<u8>> {
    let resp = rtsp
        .request(
            "POST",
            "/pair-setup",
            &[("X-Apple-HKP", HKP_TRANSIENT)],
            Some("application/octet-stream"),
            body,
        )
        .await?;
    Ok(resp.body)
}
