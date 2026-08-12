//! NTP timing: reply server (receiver polls us) + sync sender (we push).
//!
//! Time model (pyatv lineage): the RTP timestamp domain is anchored on wall
//! clock at stream start: `anchor_ts = ntp2ts(ntp_now(), rate)`; packet RTP
//! timestamps start at `LATENCY + frames_sent`; sync packets correlate
//! "ts without latency" ↔ NTP wall time of the head frame.
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:132-137 (ntp2ts/ts2ntp),
//!  288-299 (ntpNow_, rtptime32_=latency+framesSent), 968-975 (anchor),
//!  1957-1976 (sync packet), 2003-2034 (timing reply)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::net::UdpSocket;

/// NTP seconds offset 1900→1970.
const NTP_EPOCH_OFFSET: u64 = 0x83AA7E80;

/// Sender-side playout latency in frames (22050 + 44100 = 1.5 s @44.1k).
///
/// [evidence: raop_sender.h:271 — "fixed RAOP latency (pyatv)"]
pub const LATENCY: u32 = 66150;

/// 64-bit NTP fixed-point wall time (seconds since 1900 << 32 | fraction).
pub fn ntp_now() -> u64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let sec = now.as_secs();
    let frac = ((now.subsec_micros() as u64) << 32) / 1_000_000;
    ((sec + NTP_EPOCH_OFFSET) << 32) | frac
}

/// NTP fixed-point → RTP-timestamp domain at `rate`.
pub fn ntp2ts(ntp: u64, rate: u32) -> u64 {
    ((ntp >> 16) * rate as u64) >> 16
}

/// RTP-timestamp domain → NTP fixed-point.
pub fn ts2ntp(ts: u64, rate: u32) -> u64 {
    ((ts << 16) / rate as u64) << 16
}

/// Shared streaming clock state.
pub struct StreamClock {
    /// RTP-domain anchor captured at stream start.
    pub anchor_ts: u64,
    /// Total frames emitted by the pump.
    pub frames_sent: AtomicU64,
    pub rate: u32,
}

impl StreamClock {
    pub fn new(rate: u32) -> Self {
        Self {
            anchor_ts: ntp2ts(ntp_now(), rate),
            frames_sent: AtomicU64::new(0),
            rate,
        }
    }

    /// RTP timestamp stamped on the head (next) packet.
    pub fn rtptime_head(&self) -> u32 {
        (LATENCY as u64 + self.frames_sent.load(Ordering::Relaxed)) as u32
    }
}

/// Answer timing requests forever. Replies regardless of request subtype
/// (matches the reference implementation; unknowns are counted).
///
/// [evidence: raop_sender.cpp:2003-2034]
pub async fn timing_server(socket: Arc<UdpSocket>, replies: Arc<AtomicU64>) {
    let mut buf = [0u8; 64];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(?e, "timing socket recv failed");
                continue;
            }
        };
        if n < 32 {
            continue;
        }
        let now = ntp_now();
        let mut resp = [0u8; 32];
        resp[0] = buf[0]; // echo proto byte
        resp[1] = 0xD3; // type 0x53 | 0x80
        resp[2..4].copy_from_slice(&0x0007u16.to_be_bytes());
        // resp[4..8] = 0
        resp[8..16].copy_from_slice(&buf[24..32]); // reftime = request sendtime
        let now_hi = (now >> 32) as u32;
        let now_lo = now as u32;
        resp[16..20].copy_from_slice(&now_hi.to_be_bytes());
        resp[20..24].copy_from_slice(&now_lo.to_be_bytes());
        resp[24..28].copy_from_slice(&now_hi.to_be_bytes());
        resp[28..32].copy_from_slice(&now_lo.to_be_bytes());
        if socket.send_to(&resp, from).await.is_ok() {
            replies.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Emit sync packets (~1 s cadence) to the receiver's control port.
///
/// Packet: [0x90|0x80, 0xD4, 0x0007, now−latency, ntp_hi, ntp_lo, now].
/// [evidence: raop_sender.cpp:1957-1976]
pub async fn sync_sender(
    socket: Arc<UdpSocket>,
    dest: SocketAddr,
    clock: Arc<StreamClock>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut first = true;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let frames = clock.frames_sent.load(Ordering::Relaxed);
                let now = (LATENCY as u64 + frames) as u32;
                let now_without_latency = frames as u32;
                let cur_ntp = ts2ntp(clock.anchor_ts + frames, clock.rate);

                let mut pkt = [0u8; 20];
                pkt[0] = if first { 0x90 } else { 0x80 };
                pkt[1] = 0xD4;
                pkt[2..4].copy_from_slice(&0x0007u16.to_be_bytes());
                pkt[4..8].copy_from_slice(&now_without_latency.to_be_bytes());
                pkt[8..12].copy_from_slice(&((cur_ntp >> 32) as u32).to_be_bytes());
                pkt[12..16].copy_from_slice(&(cur_ntp as u32).to_be_bytes());
                pkt[16..20].copy_from_slice(&now.to_be_bytes());
                first = false;
                if let Err(e) = socket.send_to(&pkt, dest).await {
                    tracing::warn!(?e, "sync send failed");
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntp_roundtrip_conversion() {
        let n = ntp_now();
        assert!(n >> 32 > NTP_EPOCH_OFFSET);
        // ts → ntp → ts must be near-identity (quantization ≤ 1/rate).
        let ts = ntp2ts(n, 44100);
        let back = ts2ntp(ts, 44100);
        let diff = n.abs_diff(back);
        assert!(diff < (1u64 << 32) / 44100 * 4, "diff={diff}");
    }

    #[tokio::test]
    async fn timing_server_replies() {
        let server = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_addr = server.local_addr().unwrap();
        let replies = Arc::new(AtomicU64::new(0));
        let r2 = replies.clone();
        let handle = tokio::spawn(async move { timing_server(server, r2).await });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut req = [0u8; 32];
        req[0] = 0x80;
        req[1] = 0xD2; // classic timing request type
        req[24..32].copy_from_slice(&0x1122334455667788u64.to_be_bytes());
        client.send_to(&req, server_addr).await.unwrap();

        let mut resp = [0u8; 32];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut resp))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 32);
        assert_eq!(resp[0], 0x80);
        assert_eq!(resp[1], 0xD3);
        assert_eq!(&resp[2..4], &0x0007u16.to_be_bytes());
        assert_eq!(&resp[8..16], &0x1122334455667788u64.to_be_bytes());
        assert!(replies.load(Ordering::Relaxed) >= 1);
        handle.abort();
    }
}
