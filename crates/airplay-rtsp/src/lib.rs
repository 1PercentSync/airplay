//! RTSP client, binary plist, and transient pairing.

pub mod client;
pub mod pair;
pub mod plist;

pub use client::{RtspClient, RtspResponse, USER_AGENT};
pub use pair::{pair_transient, PairResult};
pub use plist::Value as PlistValue;
