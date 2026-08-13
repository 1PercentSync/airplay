//! Crypto primitives for AirPlay 2: TLV8 and SRP-6a (HAP pair-setup).

pub mod srp;
pub mod tlv8;

pub use srp::SrpClient;
pub use tlv8::{TlvMap, TlvValue};
