//! Transient pair-setup (HKP 4): fixed PIN "3939", SRP-6a, M1→M2→M3→M4.
//!
//! M1: {Method: 0, State: 1, Flags: 0x10} with `X-Apple-HKP: 4`
//! M3: {State: 3, PublicKey: A, Proof: M1}
//! M4 verified against expected HAMK; TLV Error values surfaced verbatim.
//!
//! [evidence: airplay-cli/src/ap2_hap.c:1064-1071 (M1 uint8 Flags);
//!  airplay2-sender-cpp/src/raop_sender.cpp:1315-1372 (M1/M3 bodies);
//!  airplay2-sender-cpp/src/airplay_crypto.cpp:318 (HAMK "can be checked")]

use airplay_core::TRANSIENT_PIN;
use airplay_crypto::srp;
use airplay_crypto::tlv8::{self, types};
use sha2::{Digest, Sha256};

use crate::client::{ClientError, PlainClient};

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("transport error at {step}: {source}")]
    Transport {
        step: &'static str,
        #[source]
        source: ClientError,
    },
    #[error("HTTP {status} at {step} (response logged)")]
    HttpStatus { step: &'static str, status: u16 },
    #[error("device TLV error {code} at {step}")]
    DeviceTlv { step: &'static str, code: u8 },
    #[error("malformed TLV at {step}: {detail}")]
    Malformed {
        step: &'static str,
        detail: String,
    },
    #[error("missing TLV {field:#04x} at {step}")]
    MissingField { step: &'static str, field: u8 },
    #[error("unexpected state {state} at {step} (expected {expected})")]
    BadState {
        step: &'static str,
        expected: u8,
        state: u8,
    },
    #[error("SRP failure: {0}")]
    Srp(#[from] srp::SrpError),
    #[error("server proof mismatch (HAMK) — derived keys would be wrong")]
    ServerProofMismatch,
}

/// Outcome of a successful transient pair-setup.
pub struct PairOutcome {
    /// SRP session key K (64 bytes). Channel keys derive from it.
    pub session_key: [u8; 64],
    /// Per-step human-readable transcript for diagnostics.
    pub transcript: Vec<String>,
}

impl PairOutcome {
    /// Non-secret fingerprint of the session key for cross-checking logs.
    pub fn key_fingerprint(&self) -> String {
        let h = Sha256::digest(self.session_key);
        h[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Run the full M1..M4 transient pair-setup over a plaintext connection.
pub async fn transient_pair(client: &mut PlainClient) -> Result<PairOutcome, PairError> {
    let mut transcript = Vec::new();

    // ---- M1 ----
    let m1 = tlv8::encode(&[
        (types::METHOD, &[0x00]),
        (types::STATE, &[0x01]),
        (types::FLAGS, &[0x10]),
    ]);
    transcript.push(format!("M1 send: Method=0 State=1 Flags=0x10 ({} bytes)", m1.len()));
    let r1 = post(client, &m1).await.map_err(|e| PairError::Transport {
        step: "M1",
        source: e,
    })?;
    check_status("M1", r1.status)?;
    let m2 = decode_tlv("M2", &r1.body)?;
    check_tlv_error("M2", &m2)?;
    transcript.push(format!("M2 recv: status 200, {} TLV entries", m2.len()));

    let salt = tlv8::get(&m2, types::SALT).ok_or(PairError::MissingField {
        step: "M2",
        field: types::SALT,
    })?;
    let b_pub = tlv8::get(&m2, types::PUBLIC_KEY).ok_or(PairError::MissingField {
        step: "M2",
        field: types::PUBLIC_KEY,
    })?;
    transcript.push(format!(
        "M2 fields: salt={} bytes, B={} bytes",
        salt.len(),
        b_pub.len()
    ));

    // ---- SRP ----
    let srp_session = srp::compute_client(salt, b_pub, TRANSIENT_PIN)?;
    transcript.push("SRP: A/M1/HAMK computed (pin ****)".into());

    // ---- M3 ----
    let m3 = tlv8::encode(&[
        (types::STATE, &[0x03]),
        (types::PUBLIC_KEY, &srp_session.a_pub),
        (types::PROOF, &srp_session.proof_m1),
    ]);
    transcript.push(format!(
        "M3 send: State=3 A={} bytes M1={} bytes ({} total)",
        srp_session.a_pub.len(),
        srp_session.proof_m1.len(),
        m3.len()
    ));
    let r3 = post(client, &m3).await.map_err(|e| PairError::Transport {
        step: "M3",
        source: e,
    })?;
    check_status("M3", r3.status)?;
    let m4 = decode_tlv("M4", &r3.body)?;
    check_tlv_error("M4", &m4)?;
    transcript.push(format!("M4 recv: status 200, {} TLV entries", m4.len()));

    // ---- M4 verify ----
    if let Some(state) = tlv8::get_u8(&m4, types::STATE) {
        if state != 4 {
            return Err(PairError::BadState {
                step: "M4",
                expected: 4,
                state,
            });
        }
    }
    match tlv8::get(&m4, types::PROOF) {
        Some(hamk) if hamk == srp_session.expected_hamk => {
            transcript.push("M4: server proof (HAMK) verified".into());
        }
        Some(_) => return Err(PairError::ServerProofMismatch),
        None => {
            transcript.push("M4: no proof field (device omitted HAMK; continuing)".into());
        }
    }

    Ok(PairOutcome {
        session_key: srp_session.session_key,
        transcript,
    })
}

async fn post(
    client: &mut PlainClient,
    body: &[u8],
) -> Result<crate::client::Response, ClientError> {
    client
        .request(
            "POST",
            "/pair-setup",
            &[
                ("Content-Type".into(), "application/octet-stream".into()),
                ("X-Apple-HKP".into(), "4".into()),
            ],
            body,
        )
        .await
}

fn check_status(step: &'static str, status: u16) -> Result<(), PairError> {
    if status == 200 {
        Ok(())
    } else {
        Err(PairError::HttpStatus { step, status })
    }
}

fn decode_tlv(step: &'static str, body: &[u8]) -> Result<tlv8::TlvMap, PairError> {
    tlv8::decode(body).map_err(|e| PairError::Malformed {
        step,
        detail: e.to_string(),
    })
}

fn check_tlv_error(step: &'static str, map: &tlv8::TlvMap) -> Result<(), PairError> {
    match tlv8::get_u8(map, types::ERROR) {
        Some(code) => Err(PairError::DeviceTlv { step, code }),
        None => Ok(()),
    }
}
