//! Shared error type and constants.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Protocol(String),
    Pairing(String),
    Plist(String),
    Crypto(String),
    Audio(String),
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Protocol(s) => write!(f, "protocol: {s}"),
            Error::Pairing(s) => write!(f, "pairing: {s}"),
            Error::Plist(s) => write!(f, "plist: {s}"),
            Error::Crypto(s) => write!(f, "crypto: {s}"),
            Error::Audio(s) => write!(f, "audio: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Default AirPlay 2 control port.
///
/// `[evidence: pyatv/conf.py:195 default 7000; airplay2-receiver ap2-receiver.py:1199]`
pub const AIRPLAY_CONTROL_PORT: u16 = 7000;

/// Transient pair-setup PIN. HAP fixed value, no UI.
///
/// `[evidence: owntone pair_homekit.c:1176-1177; pyatv hap_transient.py:30 TRANSIENT_PIN = 3939]`
pub const TRANSIENT_PIN: &str = "3939";
