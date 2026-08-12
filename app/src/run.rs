//! `run`: capture → pair → session → stream, with reconnect recovery and
//! the diagnostics contract ([STATUS] / [STATS] lines).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use airplay_crypto::hap::derive_keys;
use airplay_rtsp::client::PlainClient;
use airplay_rtsp::pairing::transient_pair;
use airplay_rtsp::session::{self, SessionConfig};
use airplay_stream::ntp::{self, StreamClock};
use airplay_stream::pump::{self, PumpConfig, PumpStats};
use audio_pipe::{self, PipeHandle, SourceKind};

pub struct RunArgs {
    pub addr: SocketAddr,
    pub source: SourceKind,
    pub volume_pct: f32,
}

/// Run forever until Ctrl+C (or a fatal error). Returns the process exit code.
pub async fn run(args: RunArgs) -> i32 {
    // The audio pipeline runs across reconnects.
    let volume_pct = args.volume_pct;
    let addr = args.addr;
    let (block_rx, pipe) = match audio_pipe::start(args.source, 352) {
        Ok(v) => v,
        Err(e) => {
            println!("[STATUS] pipe_failed: {e}");
            return 1;
        }
    };
    let mut block_rx = Some(block_rx);

    let mut backoff = Duration::from_secs(1);
    loop {
        match one_session(addr, volume_pct, &mut block_rx, &pipe).await {
            Ok(()) => {
                println!("[STATUS] stopped (clean)");
                return 0;
            }
            Err(e) => {
                println!("[STATUS] session_lost: {e}; reconnecting in {}s", backoff.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = tokio::signal::ctrl_c() => {
                        println!("[STATUS] stopped (ctrl-c during backoff)");
                        return 0;
                    }
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// One full session lifetime. `Ok(())` means the user stopped it.
async fn one_session(
    addr: SocketAddr,
    volume_pct: f32,
    block_rx: &mut Option<tokio::sync::mpsc::Receiver<audio_pipe::AudioBlock>>,
    pipe: &PipeHandle,
) -> Result<(), String> {
    println!("[STATUS] connecting {addr}");
    let mut plain = PlainClient::connect(addr)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    println!("[STATUS] pairing (transient)");
    let outcome = transient_pair(&mut plain)
        .await
        .map_err(|e| format!("pair: {e}"))?;
    for line in &outcome.transcript {
        tracing::debug!(%line, "pair step");
    }
    println!("[STATUS] paired fingerprint={}", outcome.key_fingerprint());

    let keys = derive_keys(&outcome.session_key);
    let cfg = SessionConfig {
        volume_pct,
        ..Default::default()
    };
    println!("[STATUS] establishing session");
    let session = session::establish(plain, keys, &cfg)
        .await
        .map_err(|e| format!("session: {e}"))?;
    println!(
        "[STATUS] session_ok data_port={} control_port={} rtsp_session={}",
        session.data_port, session.control_port, session.rtsp_session
    );

    // Streaming stack. The NTP timing server is already running (started
    // inside establish() — required before stream SETUP completes).
    let clock = Arc::new(StreamClock::new(44100));
    let stats = Arc::new(PumpStats::default());
    let (sd_tx, sd_rx) = watch::channel(false);
    let sync_task = {
        let s = session.control_socket.clone();
        let dest = session.sync_dest();
        let c = clock.clone();
        let sd = sd_rx.clone();
        tokio::spawn(async move { ntp::sync_sender(s, dest, c, sd).await })
    };
    let rx = block_rx.take().expect("one block receiver per pipeline");
    let pump_task = {
        let data_socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| e.to_string())?;
        let cfgp = PumpConfig {
            dest_data: session.data_dest(),
            control_socket: session.control_socket.clone(),
            ssrc: session.session_id,
            audio_key: session.audio_key,
            spf: 352,
            rate: 44100,
        };
        tokio::spawn(pump::run(
            cfgp,
            data_socket,
            clock.clone(),
            rx,
            stats.clone(),
            sd_rx.clone(),
        ))
    };
    println!("[STATUS] streaming");

    // Status reporter (every 10 s).
    let stats_task = {
        let stats = stats.clone();
        let pipe_stats = pipe.stats.clone();
        let clock = clock.clone();
        let mut sd = sd_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        println!(
                            "[STATS] rtp_sent={} silence={} rtx_req={} rtx_sent={} rtx_missed={} frames={} cap_frames={} disc={} dropped_blocks={}",
                            stats.sent.load(Ordering::Relaxed),
                            stats.silence_filled.load(Ordering::Relaxed),
                            stats.rtx_requests.load(Ordering::Relaxed),
                            stats.rtx_sent.load(Ordering::Relaxed),
                            stats.rtx_missed.load(Ordering::Relaxed),
                            clock.frames_sent.load(Ordering::Relaxed),
                            pipe_stats.captured_frames.load(Ordering::Relaxed),
                            pipe_stats.discontinuities.load(Ordering::Relaxed),
                            pipe_stats.dropped_blocks.load(Ordering::Relaxed),
                        );
                    }
                    _ = sd.changed() => return,
                }
            }
        })
    };

    // Wait for: session death, Ctrl+C.
    let mut dead = session.dead_rx.clone();
    let reason = tokio::select! {
        r = dead.changed() => {
            match r {
                Ok(()) => format!("dead: {}", dead.borrow().as_str()),
                Err(_) => "dead: channel closed".into(),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("[STATUS] stopping (ctrl-c)");
            "stop".into()
        }
    };

    sd_tx.send(true).ok();
    let _ = pump_task.await;
    sync_task.abort();
    stats_task.abort();
    session.teardown().await;

    if reason == "stop" {
        Ok(())
    } else {
        Err(reason)
    }
}
