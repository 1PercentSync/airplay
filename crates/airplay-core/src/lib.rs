//! Core shared types: device model, audio formats, stream configuration.
//!
//! Protocol facts are annotated with evidence references in the form
//! `[evidence: <file>:<line>]` pointing into `reference/repos/` clones.

use std::net::IpAddr;

/// AirPlay 2 realtime ALAC format codes for the `audioFormat` stream SETUP key.
///
/// [evidence: airplay-cli/DESIGN.md §7; airplay-cli/src/ap2_client.c:1320]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlacFormat {
    /// 0x40000 — 44.1 kHz / 16-bit / stereo (baseline, decision C).
    L44100_16_2,
    /// 0x100000 — 48 kHz / 16-bit / stereo.
    L48000_16_2,
    /// 0x80000 — 44.1 kHz / 24-bit / stereo.
    L44100_24_2,
    /// 0x200000 — 48 kHz / 24-bit / stereo.
    L48000_24_2,
}

impl AlacFormat {
    pub fn code(self) -> u64 {
        match self {
            Self::L44100_16_2 => 0x40000,
            Self::L48000_16_2 => 0x100000,
            Self::L44100_24_2 => 0x80000,
            Self::L48000_24_2 => 0x200000,
        }
    }

    pub fn sample_rate(self) -> u32 {
        match self {
            Self::L44100_16_2 | Self::L44100_24_2 => 44100,
            Self::L48000_16_2 | Self::L48000_24_2 => 48000,
        }
    }

    pub fn bit_depth(self) -> u8 {
        match self {
            Self::L44100_16_2 | Self::L48000_16_2 => 16,
            Self::L44100_24_2 | Self::L48000_24_2 => 24,
        }
    }

    /// Decode an `audioFormat` code back into a known format, if any.
    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            0x40000 => Some(Self::L44100_16_2),
            0x100000 => Some(Self::L48000_16_2),
            0x80000 => Some(Self::L44100_24_2),
            0x200000 => Some(Self::L48000_24_2),
            _ => None,
        }
    }
}

/// A discovered (or manually specified) AirPlay receiver.
#[derive(Debug, Clone)]
pub struct Device {
    pub ip: IpAddr,
    /// RTSP port (from mDNS SRV or manual; AirPlay commonly 7000).
    pub port: u16,
    pub name: Option<String>,
    /// mDNS `model` TXT, e.g. `AudioAccessory5,1`.
    pub model: Option<String>,
    /// mDNS `features` TXT, e.g. `0x4A7FCA00,0x3C354BD0`.
    pub features: Option<u64>,
    /// mDNS `flags`/`sf` TXT (status flags).
    pub status_flags: Option<u64>,
}

impl Device {
    pub fn manual(ip: IpAddr, port: u16) -> Self {
        Self {
            ip,
            port,
            name: None,
            model: None,
            features: None,
            status_flags: None,
        }
    }
}

/// Fixed transient-pairing PIN (HKP 4), baked into the HAP spec.
///
/// [evidence: airplay-cli/src/ap2_hap.c:76; AirSend/crates/airplay-core/src/pairing.rs:13]
pub const TRANSIENT_PIN: &str = "3939";

/// Samples per ALAC packet on the realtime stream (`spf`).
///
/// [evidence: owntone-server/src/outputs/airplay.c:85; shairport-sync/rtsp.c:2878]
pub const FRAMES_PER_PACKET: usize = 352;
