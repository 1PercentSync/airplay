//! TLV8, SRP-6a, and HAP control-channel frames.
//!
//! Formulas and wire layout: `docs/协议实现规范.md` §16, including §16.5.

pub mod hap;
pub mod random;
pub mod srp;
pub mod tlv8;

pub use hap::HapCipher;
pub use random::fill_random;
pub use srp::{SrpClient, N_BYTES};
pub use tlv8::{decode as tlv_decode, encode as tlv_encode, TlvMap};
