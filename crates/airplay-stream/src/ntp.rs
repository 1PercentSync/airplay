//! NTP / RTP timestamp conversion.
//! [evidence: pyatv raop/timing.py:11-31]

use std::time::{SystemTime, UNIX_EPOCH};

const NTP_UNIX: u64 = 0x83AA7E80;

pub fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = d.as_secs() + NTP_UNIX;
    let frac = (u128::from(d.subsec_micros()) << 32) / 1_000_000;
    (seconds << 32) | (frac as u64)
}

pub fn ntp_parts(ntp: u64) -> (u32, u32) {
    ((ntp >> 32) as u32, ntp as u32)
}

pub fn ntp2ts(ntp: u64, rate: u32) -> u64 {
    ((ntp >> 16).saturating_mul(u64::from(rate))) >> 16
}

pub fn ts2ntp(timestamp: u64, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    ((timestamp << 16) / u64::from(rate)) << 16
}
