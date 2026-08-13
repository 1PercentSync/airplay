//! Shared constants and errors for the probe (and later the sender).
//!
//! User-Agent is `AirPlay/550.10` for every RTSP request, including
//! `/pair-setup`. User wording 2026-08-13: keep 550.

use std::io;

/// [evidence: pyatv support/rtsp.py:22; raop_sender.cpp:547]
pub const USER_AGENT: &str = "AirPlay/550.10";

/// [evidence: owntone airplay.c:902]
pub const CLIENT_NAME: &str = "airplay";

/// [evidence: owntone pair_homekit.c:52; pyatv hap_srp.py:141]
pub const PAIR_USERNAME: &str = "Pair-Setup";

/// Transient PIN. [evidence: owntone pair_homekit.c:1176-1177; pyatv hap_transient.py:30]
pub const TRANSIENT_PIN: &str = "3939";

/// [evidence: owntone airplay.c:2837-2838; pyatv hap_transient.py:26]
pub const HKP_TRANSIENT: &str = "4";

/// Default AirPlay control port. [evidence: airplay-spec service_discovery.md; 06]
pub const AIRPLAY_PORT: u16 = 7000;

/// mDNS browse window. [evidence: pyatv __init__.py:35]
pub const MDNS_BROWSE_SECS: u64 = 5;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("rtsp: {0}")]
    Rtsp(String),
    #[error("plist: {0}")]
    Plist(String),
    #[error("tlv: {0}")]
    Tlv(String),
    #[error("srp: {0}")]
    Srp(String),
    #[error("pairing: {0}")]
    Pair(String),
    #[error("mdns: {0}")]
    Mdns(String),
    #[error("audio: {0}")]
    Audio(String),
}

pub type Result<T> = std::result::Result<T, Error>;
