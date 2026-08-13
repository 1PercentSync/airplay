//! RTP audio, ALAC uncompressed frames, NTP timing, sync, retransmit wrap.
//!
//! Wire layouts: `docs/协议实现规范.md` §7–§9.

mod alac;
mod ntp;
mod rtp;

pub use alac::encode_alac_frame;
pub use ntp::{ntp2ts, ntp_now, ntp_parts, ts2ntp};
pub use rtp::{
    encrypt_audio, retransmit_wrap, rtp_header, sync_packet, timing_reply, Backlog,
    FRAMES_PER_PACKET, LATENCY_FRAMES, SAMPLE_RATE,
};
