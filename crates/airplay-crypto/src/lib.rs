//! TLV8 and SRP-6a used by HAP transient pair-setup.
//!
//! Formulas and wire layout: `docs/协议实现规范.md` §16.1–16.2.

pub mod random;
pub mod srp;
pub mod tlv8;

pub use random::fill_random;
pub use srp::{SrpClient, N_BYTES};
pub use tlv8::{decode as tlv_decode, encode as tlv_encode, TlvMap};
