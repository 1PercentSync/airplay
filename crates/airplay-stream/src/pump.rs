//! Realtime RTP audio pump: token-bucket pacing at 44100 frames/s, ALAC
//! encode → RTP header → ChaCha20-Poly1305 → UDP, with a retransmit backlog
//! and the splice discipline (the wire NEVER runs dry: starved input is
//! replaced by encoded silence on the same unfrozen timeline).
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1772-1787 (encrypt:
//!  AAD=hdr[4..12], trailing 8B LE nonce), 1791-1812 (token bucket, burst
//!  cap), 1839-1875 (header/packet), 1979-2001 (0x55→0xD6 retransmit)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use crate::alac;
use crate::ntp::StreamClock;

const BACKLOG: usize = 1024;
const TICK: Duration = Duration::from_millis(2);
const BURST_PER_TICK: usize = 8;

#[derive(Default)]
pub struct PumpStats {
    pub sent: AtomicU64,
    pub silence_filled: AtomicU64,
    pub rtx_requests: AtomicU64,
    pub rtx_sent: AtomicU64,
    pub rtx_missed: AtomicU64,
}

pub struct PumpConfig {
    /// Receiver data port (destination of audio RTP).
    pub dest_data: SocketAddr,
    /// Our control socket (receives 0x55, sends 0xD6 + sync packets).
    pub control_socket: Arc<UdpSocket>,
    /// SSRC = numeric session id (NTP timing mode).
    pub ssrc: u32,
    /// Audio cipher key == plist shk value.
    pub audio_key: [u8; 32],
    /// Samples per packet (352) and rate (44100).
    pub spf: usize,
    pub rate: u32,
}

/// An audio block: exactly `spf * 2` interleaved i16 samples.
pub type AudioBlock = Vec<i16>;

/// Run the pump until shutdown is signalled. Drains `block_rx`; when starved,
/// emits ALAC silence so the timeline never breaks (splice discipline).
pub async fn run(
    cfg: PumpConfig,
    data_socket: UdpSocket,
    clock: Arc<StreamClock>,
    mut block_rx: mpsc::Receiver<AudioBlock>,
    stats: Arc<PumpStats>,
    mut shutdown: watch::Receiver<bool>,
) {
    let spf = cfg.spf;
    let silence_block: AudioBlock = vec![0i16; spf * 2];
    let mut backlog: Vec<Option<(u16, Vec<u8>)>> = vec![None; BACKLOG];
    let mut state = PacketState {
        seq: 0,
        audio_nonce: 0,
        first: true,
    };

    let (rtx_tx, mut rtx_rx) = mpsc::channel::<(u16, SocketAddr)>(256);
    // Control listener task: 0x55 → queue resends.
    let control = cfg.control_socket.clone();
    let control_listener = control.clone();
    let stats_ctrl = stats.clone();
    let mut shutdown_ctrl = shutdown.clone();
    let ctrl_task = tokio::spawn(async move {
        let mut buf = [0u8; 256];
        loop {
            tokio::select! {
                recv = control_listener.recv_from(&mut buf) => {
                    let Ok((n, from)) = recv else { continue };
                    if n < 8 || buf[1] & 0x7F != 0x55 {
                        continue;
                    }
                    let lost = u16::from_be_bytes([buf[4], buf[5]]);
                    let count = u16::from_be_bytes([buf[6], buf[7]]);
                    stats_ctrl.rtx_requests.fetch_add(1, Ordering::Relaxed);
                    for i in 0..count {
                        let _ = rtx_tx.try_send((lost.wrapping_add(i), from));
                    }
                }
                _ = shutdown_ctrl.changed() => {
                    if *shutdown_ctrl.borrow() { return; }
                }
            }
        }
    });

    let cipher = ChaCha20Poly1305::new((&cfg.audio_key).into());
    let start = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                // Token bucket: how many frames SHOULD be on the wire by now.
                let elapsed_ns = start.elapsed().as_nanos() as u64;
                let target = elapsed_ns * cfg.rate as u64 / 1_000_000_000;
                let mut burst = 0;
                loop {
                    let frames = clock.frames_sent.load(Ordering::Relaxed);
                    if frames + spf as u64 > target || burst >= BURST_PER_TICK {
                        break;
                    }
                    // Splice discipline: starved → silence, timeline intact.
                    let block = match block_rx.try_recv() {
                        Ok(b) if b.len() == spf * 2 => b,
                        _ => {
                            stats.silence_filled.fetch_add(1, Ordering::Relaxed);
                            silence_block.clone()
                        }
                    };
                    let pkt = build_packet(
                        &cipher,
                        &block,
                        spf,
                        &mut state,
                        clock.rtptime_head(),
                        cfg.ssrc,
                    );
                    let sent_seq = state.seq.wrapping_sub(1);
                    backlog[(sent_seq as usize) & (BACKLOG - 1)] = Some((sent_seq, pkt.clone()));
                    if let Err(e) = data_socket.send_to(&pkt, cfg.dest_data).await {
                        tracing::warn!(?e, "audio send failed");
                    } else {
                        stats.sent.fetch_add(1, Ordering::Relaxed);
                    }
                    clock.frames_sent.fetch_add(spf as u64, Ordering::Relaxed);
                    burst += 1;
                }
            }
            Some((lost_seq, from)) = rtx_rx.recv() => {
                let slot = (lost_seq as usize) & (BACKLOG - 1);
                match &backlog[slot] {
                    Some((s, pkt)) if *s == lost_seq => {
                        let mut resp = Vec::with_capacity(4 + pkt.len());
                        resp.extend_from_slice(&[0x80, 0xD6]);
                        resp.extend_from_slice(&lost_seq.to_be_bytes());
                        resp.extend_from_slice(pkt);
                        if control.send_to(&resp, from).await.is_ok() {
                            stats.rtx_sent.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    _ => {
                        stats.rtx_missed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    ctrl_task.abort();
                    return;
                }
            }
        }
    }
}

/// Mutable per-session packet counters.
struct PacketState {
    seq: u16,
    audio_nonce: u64,
    first: bool,
}

/// Build one wire packet: [12B RTP hdr][ALAC ct][16B tag][8B LE nonce].
fn build_packet(
    cipher: &ChaCha20Poly1305,
    block: &[i16],
    spf: usize,
    state: &mut PacketState,
    rtptime: u32,
    ssrc: u32,
) -> Vec<u8> {
    let payload = alac::encode_frame(block, spf);

    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = if state.first { 0xE0 } else { 0x60 };
    header[2..4].copy_from_slice(&state.seq.to_be_bytes());
    header[4..8].copy_from_slice(&rtptime.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());

    let nonce8 = state.audio_nonce.to_le_bytes();
    let mut nonce12 = [0u8; 12];
    nonce12[4..].copy_from_slice(&nonce8);
    let ct = cipher
        .encrypt(
            (&nonce12).into(),
            Payload {
                msg: &payload,
                aad: &header[4..12],
            },
        )
        .expect("chacha encrypt");

    state.first = false;
    state.seq = state.seq.wrapping_add(1);
    state.audio_nonce += 1;

    let mut pkt = Vec::with_capacity(12 + ct.len() + 8);
    pkt.extend_from_slice(&header);
    pkt.extend_from_slice(&ct);
    pkt.extend_from_slice(&nonce8);
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntp;

    /// Wire-shape check of one built packet (decrypts back to the ALAC frame).
    #[test]
    fn packet_layout_and_decrypt() {
        let key = [0x42u8; 32];
        let cipher = ChaCha20Poly1305::new((&key).into());
        let block: AudioBlock = (0..352 * 2).map(|i| i as i16).collect();
        let mut state = PacketState {
            seq: 7,
            audio_nonce: 3,
            first: true,
        };
        let pkt = build_packet(&cipher, &block, 352, &mut state, 66150, 0xDEADBEEF);

        // Header.
        assert_eq!(pkt[0], 0x80);
        assert_eq!(pkt[1], 0xE0); // marker on first
        assert_eq!(&pkt[2..4], &7u16.to_be_bytes());
        assert_eq!(&pkt[4..8], &66150u32.to_be_bytes());
        assert_eq!(&pkt[8..12], &0xDEADBEEFu32.to_be_bytes());
        // Trailing nonce = 8-byte LE counter.
        assert_eq!(&pkt[pkt.len() - 8..], &3u64.to_le_bytes());
        assert_eq!(state.seq, 8);
        assert_eq!(state.audio_nonce, 4);

        // Decrypt round-trip with AAD = header[4..12].
        let mut nonce12 = [0u8; 12];
        nonce12[4..].copy_from_slice(&3u64.to_le_bytes());
        let pt = cipher
            .decrypt(
                (&nonce12).into(),
                Payload {
                    msg: &pkt[12..pkt.len() - 8],
                    aad: &pkt[4..12],
                },
            )
            .unwrap();
        assert_eq!(pt, alac::encode_frame(&block, 352));
    }

    /// Pacing: with no audio input, the pump still emits ~44100 frames/s of
    /// silence (splice discipline) with advancing seq/ts.
    #[tokio::test]
    async fn starved_pump_emits_silence_at_rate() {
        let data = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let control = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let clock = Arc::new(ntp::StreamClock::new(44100));
        let stats = Arc::new(PumpStats::default());
        let (_rtx, block_rx) = mpsc::channel::<AudioBlock>(64);
        let (sd_tx, sd_rx) = watch::channel(false);

        let cfg = PumpConfig {
            dest_data: recv.local_addr().unwrap(),
            control_socket: control,
            ssrc: 1,
            audio_key: [0; 32],
            spf: 352,
            rate: 44100,
        };
        let clock2 = clock.clone();
        let stats2 = stats.clone();
        let task = tokio::spawn(run(cfg, data, clock2, block_rx, stats2, sd_rx));

        tokio::time::sleep(Duration::from_millis(500)).await;
        sd_tx.send(true).unwrap();
        let _ = task.await;

        let sent = stats.sent.load(Ordering::Relaxed);
        let silence = stats.silence_filled.load(Ordering::Relaxed);
        // 0.5 s ≈ 62 packets; allow generous scheduling slack.
        assert!(sent >= 45, "sent={sent}");
        assert_eq!(sent, silence, "all packets should be silence-filled");
        assert_eq!(clock.frames_sent.load(Ordering::Relaxed), sent * 352);
    }
}
