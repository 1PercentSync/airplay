//! Playable CLI: pair, session/stream SETUP, capture, RTP.
//!
//! Order: `docs/协议实现规范.md` 链路总览. Success is HomePod sound on the
//! user's Windows run; this file does not claim that.

use airplay_core::{AIRPLAY_PORT, CLIENT_NAME};
use airplay_crypto::random::{fill_random, random_u32};
use airplay_rtsp::{
    connect_events, parse_host_port, plist_decode, plist_encode, pretty_print_value, Identity,
    PlistInt, RtspClient, Value,
};
use airplay_stream::{
    encrypt_audio, ntp2ts, ntp_now, retransmit_wrap, rtp_header, sync_packet, timing_reply,
    Backlog, FRAMES_PER_PACKET, LATENCY_FRAMES, SAMPLE_RATE,
};
use anyhow::{Context, Result};
use audio_pipe::{Capture, PacketQueue};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::{interval, Instant};
use tracing::{info, warn};

const BPLIST: &str = "application/x-apple-binary-plist";

pub async fn run(target: &str, device_hint: Option<&str>) -> Result<()> {
    println!("[STATUS] probing");
    let addr = parse_host_port(target, AIRPLAY_PORT).map_err(|e| anyhow::anyhow!("{e}"))?;
    let identity = Identity::generate().map_err(|e| anyhow::anyhow!("{e}"))?;

    {
        let mut probe = RtspClient::connect(addr, identity.clone())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let resp = probe
            .request("GET", "/info", &[], None, &[])
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Ok(v) = plist_decode(&resp.body) {
            let mut s = String::new();
            pretty_print_value(&v, 0, &mut s);
            info!("GET /info\n{s}");
        }
    }

    let mut rtsp = RtspClient::connect(addr, identity)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let key = airplay_rtsp::transient_pair(&mut rtsp)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    rtsp.enable_control_encryption(&key)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("[STATUS] paired");

    let local = rtsp.local_addr().map_err(|e| anyhow::anyhow!("{e}"))?;
    let session_id = random_u32().map_err(|e| anyhow::anyhow!("{e}"))?;
    let uri = session_uri(local, session_id);
    let device_id = random_mac()?;
    let uuid = random_uuid()?;
    let mut shk = [0u8; 32];
    shk.copy_from_slice(&key[..32]);

    let timing_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let control_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let audio_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let timing_port = timing_sock.local_addr()?.port();
    let local_control = control_sock.local_addr()?.port();
    info!(timing_port, local_control, "UDP ports bound");

    let t = timing_sock.clone();
    tokio::spawn(async move {
        if let Err(e) = timing_loop(t).await {
            warn!("timing server: {e}");
        }
    });

    let session_body = session_setup_plist(&device_id, &uuid, timing_port)?;
    log_plist("session SETUP request", &session_body);
    let resp = rtsp
        .request("SETUP", &uri, &[], Some(BPLIST), &session_body)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    log_plist("session SETUP response", &resp.body);
    let session_hdr = resp
        .headers
        .get("session")
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
    let root = plist_decode(&resp.body).map_err(|e| anyhow::anyhow!("{e}"))?;
    let event_port = root
        .dict_get("eventPort")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u16;
    info!(event_port, session = ?session_hdr, "session SETUP ok");
    if matches!(root.dict_get("skipRecord"), Some(Value::Bool(true))) {
        info!("skipRecord=true; sending RECORD anyway (field not in registry)");
    }

    if event_port != 0 {
        let ev_host = addr;
        tokio::spawn(async move {
            match connect_events(ev_host, event_port, &key).await {
                Ok(conn) => {
                    if let Err(e) = conn.serve().await {
                        warn!("event channel: {e}");
                    }
                }
                Err(e) => warn!("event channel not connected, continuing: {e}"),
            }
        });
    } else {
        warn!("no eventPort in session SETUP, continuing");
    }

    let stream_body = stream_setup_plist(local_control, &shk, session_id)?;
    log_plist("stream SETUP request", &stream_body);
    let extra = session_extra(&session_hdr);
    let extra_ref: Vec<(&str, &str)> = extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let resp = rtsp
        .request("SETUP", &uri, &extra_ref, Some(BPLIST), &stream_body)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    log_plist("stream SETUP response", &resp.body);
    let (data_port, remote_control) = parse_stream_ports(&resp.body)?;
    info!(data_port, remote_control, "stream SETUP ok");

    let vol = volume_body(0.5);
    rtsp.request(
        "SET_PARAMETER",
        &uri,
        &extra_ref,
        Some("text/parameters"),
        vol.as_bytes(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    rtsp.request("RECORD", &uri, &extra_ref, None, &[])
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("[STATUS] setup_ok");

    let capture = Capture::start(device_hint).map_err(|e| anyhow::anyhow!("{e}"))?;
    let packets = Arc::new(PacketQueue::new(64));
    audio_pipe::spawn_processor(capture.ring(), packets.clone());

    let rtp_sent = Arc::new(AtomicU64::new(0));
    let rtx = Arc::new(AtomicU64::new(0));
    let ka_miss = Arc::new(AtomicU64::new(0));
    let backlog = Arc::new(StdMutex::new(Backlog::new()));

    let host_ip = addr.ip();
    let c_sock = control_sock.clone();
    let bl = backlog.clone();
    let rtx_c = rtx.clone();
    tokio::spawn(async move {
        if let Err(e) = control_loop(c_sock, bl, rtx_c).await {
            warn!("control UDP: {e}");
        }
    });

    let rtsp = Arc::new(Mutex::new(rtsp));
    let rtsp_fb = rtsp.clone();
    let extra_fb = extra.clone();
    let ka = ka_miss.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let extra_ref: Vec<(&str, &str)> = extra_fb
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let mut g = rtsp_fb.lock().await;
            match g.exchange("POST", "/feedback", &extra_ref, None, &[]).await {
                Ok(r) if (200..300).contains(&r.code) => {}
                Ok(r) => {
                    ka.fetch_add(1, Ordering::Relaxed);
                    warn!(code = r.code, "feedback not ok");
                }
                Err(e) => {
                    ka.fetch_add(1, Ordering::Relaxed);
                    warn!("feedback: {e}");
                }
            }
        }
    });

    let stats_ring = capture.ring();
    let stats_pkt = packets.clone();
    let rtp_s = rtp_sent.clone();
    let rtx_s = rtx.clone();
    let ka_s = ka_miss.clone();
    tokio::spawn(async move {
        let mut last_rtp = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let sent = rtp_s.load(Ordering::Relaxed);
            let rtp_10s = sent.saturating_sub(last_rtp);
            last_rtp = sent;
            println!(
                "[STATS] cap_disc={} q_drop={} rtx={} ka_miss={} reconnect=0 rtp_sent={} rtp_10s={} pkt_drop={}",
                stats_ring.disc.load(Ordering::Relaxed),
                stats_ring.drops.load(Ordering::Relaxed),
                rtx_s.load(Ordering::Relaxed),
                ka_s.load(Ordering::Relaxed),
                sent,
                rtp_10s,
                stats_pkt.drops.load(Ordering::Relaxed),
            );
        }
    });

    println!("[STATUS] streaming");
    let data_addr = SocketAddr::new(host_ip, data_port);
    let ctl_addr = SocketAddr::new(
        host_ip,
        if remote_control == 0 {
            data_port
        } else {
            remote_control
        },
    );

    let pump = send_pump(
        audio_sock,
        control_sock,
        data_addr,
        ctl_addr,
        shk,
        session_id,
        packets,
        backlog,
        rtp_sent,
    );
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("ctrl-c, TEARDOWN");
        }
        r = pump => {
            if let Err(e) = r {
                warn!("send pump: {e}");
            }
        }
    }

    let empty = plist_encode(&Value::Dict(vec![])).map_err(|e| anyhow::anyhow!("{e}"))?;
    {
        let extra_ref: Vec<(&str, &str)> = extra
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        match tokio::time::timeout(Duration::from_millis(250), rtsp.lock()).await {
            Ok(mut g) => {
                match tokio::time::timeout(
                    Duration::from_millis(250),
                    g.exchange("TEARDOWN", &uri, &extra_ref, Some(BPLIST), &empty),
                )
                .await
                {
                    Ok(Ok(r)) => info!(code = r.code, "TEARDOWN"),
                    Ok(Err(e)) => warn!("TEARDOWN: {e}"),
                    Err(_) => warn!("TEARDOWN timed out"),
                }
            }
            Err(_) => warn!("TEARDOWN skipped, control channel busy"),
        }
    }
    drop(capture);
    println!("[STATUS] dead(teardown)");
    Ok(())
}

async fn send_pump(
    audio: Arc<UdpSocket>,
    control: Arc<UdpSocket>,
    data_addr: SocketAddr,
    ctl_addr: SocketAddr,
    shk: [u8; 32],
    ssrc: u32,
    packets: Arc<PacketQueue>,
    backlog: Arc<StdMutex<Backlog>>,
    rtp_sent: Arc<AtomicU64>,
) -> Result<()> {
    // pyatv stream_client.py: monotonic clock vs frames sent; catch-up capped.
    // Architecture §7: ≤8 packets per tick. Do not use tokio interval + Skip:
    // Windows 15.6ms default timer made that ~65 pkt/s (half of 44100/352).
    const MAX_BURST: u32 = 8;
    let start_ts = ntp2ts(ntp_now(), SAMPLE_RATE);
    let mut frames_sent: u64 = 0;
    let mut seq = random_u32().map_err(|e| anyhow::anyhow!("{e}"))? as u16;
    let mut first_audio = true;
    let mut first_sync = true;
    let silence = vec![0i16; FRAMES_PER_PACKET * 2];
    let origin = Instant::now();
    let mut last_sync = origin - Duration::from_secs(2);

    loop {
        send_one(
            &audio,
            &control,
            data_addr,
            ctl_addr,
            &shk,
            ssrc,
            &packets,
            &backlog,
            &rtp_sent,
            start_ts,
            &mut frames_sent,
            &mut seq,
            &mut first_audio,
            &mut first_sync,
            &mut last_sync,
            &silence,
        )
        .await?;

        let elapsed = origin.elapsed().as_secs_f64();
        let expected = (elapsed * f64::from(SAMPLE_RATE)) as u64;
        let mut extra = 0u32;
        while frames_sent + FRAMES_PER_PACKET as u64 <= expected && extra < MAX_BURST - 1 {
            send_one(
                &audio,
                &control,
                data_addr,
                ctl_addr,
                &shk,
                ssrc,
                &packets,
                &backlog,
                &rtp_sent,
                start_ts,
                &mut frames_sent,
                &mut seq,
                &mut first_audio,
                &mut first_sync,
                &mut last_sync,
                &silence,
            )
            .await?;
            extra += 1;
        }

        let next_frames = frames_sent + FRAMES_PER_PACKET as u64;
        let next = origin
            + Duration::from_secs_f64(next_frames as f64 / f64::from(SAMPLE_RATE));
        tokio::time::sleep_until(next).await;
    }
}

async fn send_one(
    audio: &UdpSocket,
    control: &UdpSocket,
    data_addr: SocketAddr,
    ctl_addr: SocketAddr,
    shk: &[u8; 32],
    ssrc: u32,
    packets: &PacketQueue,
    backlog: &StdMutex<Backlog>,
    rtp_sent: &AtomicU64,
    start_ts: u64,
    frames_sent: &mut u64,
    seq: &mut u16,
    first_audio: &mut bool,
    first_sync: &mut bool,
    last_sync: &mut Instant,
    silence: &[i16],
) -> Result<()> {
    let pcm = match packets.pop() {
        Some(p) if p.len() == FRAMES_PER_PACKET * 2 => p,
        _ => silence.to_vec(),
    };
    let rtptime = LATENCY_FRAMES.wrapping_add(*frames_sent as u32);
    let header = rtp_header(*first_audio, *seq, rtptime, ssrc);
    let pkt = encrypt_audio(shk, &header, &pcm, *seq).map_err(|e| anyhow::anyhow!("{e}"))?;
    audio.send_to(&pkt, data_addr).await?;
    {
        let mut b = backlog.lock().unwrap();
        b.store(*seq, pkt);
    }
    if *first_audio {
        info!(addr = %data_addr, seq = *seq, "first RTP sent");
    }
    *first_audio = false;
    *frames_sent += FRAMES_PER_PACKET as u64;
    *seq = seq.wrapping_add(1);
    rtp_sent.fetch_add(1, Ordering::Relaxed);

    if *first_sync || last_sync.elapsed() >= Duration::from_secs(1) {
        let head_ts = start_ts + *frames_sent;
        let sync = sync_packet(*first_sync, rtptime, head_ts);
        control.send_to(&sync, ctl_addr).await?;
        *first_sync = false;
        *last_sync = Instant::now();
    }
    Ok(())
}

async fn timing_loop(sock: Arc<UdpSocket>) -> Result<()> {
    let mut buf = [0u8; 64];
    let mut first = true;
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        if n != 32 {
            continue;
        }
        if buf[0] != 0x80 || buf[1] != 0xd2 {
            continue;
        }
        let mut req = [0u8; 32];
        req.copy_from_slice(&buf[..32]);
        let res = timing_reply(&req);
        sock.send_to(&res, peer).await?;
        if first {
            info!(peer = %peer, "first timing 0xD2");
            first = false;
        }
    }
}

async fn control_loop(
    sock: Arc<UdpSocket>,
    backlog: Arc<StdMutex<Backlog>>,
    rtx: Arc<AtomicU64>,
) -> Result<()> {
    let mut buf = [0u8; 64];
    let mut first_d5 = true;
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        if n != 8 {
            continue;
        }
        if buf[0] != 0x80 {
            continue;
        }
        let ty = buf[1] & 0x7f;
        if ty != 0x55 {
            continue;
        }
        if first_d5 {
            info!(peer = %peer, "first retransmit 0xD5");
            first_d5 = false;
        }
        let start = u16::from_be_bytes([buf[4], buf[5]]);
        let count = u16::from_be_bytes([buf[6], buf[7]]);
        for i in 0..count {
            let seq = start.wrapping_add(i);
            let wrapped = {
                let b = backlog.lock().unwrap();
                b.get(seq).map(|p| retransmit_wrap(seq, p))
            };
            match wrapped {
                Some(pkt) => {
                    sock.send_to(&pkt, peer).await?;
                    rtx.fetch_add(1, Ordering::Relaxed);
                }
                None => info!(seq, "retransmit miss"),
            }
        }
    }
}

fn session_uri(local: SocketAddr, session_id: u32) -> String {
    match local.ip() {
        std::net::IpAddr::V4(ip) => format!("rtsp://{ip}/{session_id}"),
        std::net::IpAddr::V6(ip) => format!("rtsp://[{ip}]/{session_id}"),
    }
}

fn session_extra(session: &Option<String>) -> Vec<(String, String)> {
    match session {
        Some(s) => vec![("Session".into(), s.clone())],
        None => vec![],
    }
}

fn v_int(n: i64) -> Value {
    Value::Int(PlistInt::from_i64(n))
}

fn session_setup_plist(device_id: &str, uuid: &str, timing_port: u16) -> Result<Vec<u8>> {
    let dict = Value::Dict(vec![
        ("deviceID".into(), Value::String(device_id.into())),
        ("sessionUUID".into(), Value::String(uuid.into())),
        ("timingPort".into(), v_int(i64::from(timing_port))),
        ("timingProtocol".into(), Value::String("NTP".into())),
        ("isMultiSelectAirPlay".into(), Value::Bool(true)),
        ("groupContainsGroupLeader".into(), Value::Bool(false)),
        ("macAddress".into(), Value::String(device_id.into())),
        ("model".into(), Value::String("iPhone14,3".into())),
        ("name".into(), Value::String(CLIENT_NAME.into())),
        ("osBuildVersion".into(), Value::String("20F66".into())),
        ("osName".into(), Value::String("iPhone OS".into())),
        ("osVersion".into(), Value::String("16.5".into())),
        ("senderSupportsRelay".into(), Value::Bool(false)),
        ("sourceVersion".into(), Value::String("690.7.1".into())),
        ("statsCollectionEnabled".into(), Value::Bool(false)),
    ]);
    plist_encode(&dict).map_err(|e| anyhow::anyhow!("{e}"))
}

fn stream_setup_plist(control_port: u16, shk: &[u8; 32], session_id: u32) -> Result<Vec<u8>> {
    let stream = Value::Dict(vec![
        ("audioFormat".into(), v_int(0x40000)),
        ("audioMode".into(), Value::String("default".into())),
        ("controlPort".into(), v_int(i64::from(control_port))),
        ("ct".into(), v_int(2)),
        ("isMedia".into(), Value::Bool(true)),
        ("latencyMax".into(), v_int(88200)),
        ("latencyMin".into(), v_int(11025)),
        ("shk".into(), Value::Data(shk.to_vec())),
        ("spf".into(), v_int(FRAMES_PER_PACKET as i64)),
        ("sr".into(), v_int(i64::from(SAMPLE_RATE))),
        ("type".into(), v_int(0x60)),
        ("supportsDynamicStreamID".into(), Value::Bool(false)),
        ("streamConnectionID".into(), v_int(i64::from(session_id))),
    ]);
    let root = Value::Dict(vec![("streams".into(), Value::Array(vec![stream]))]);
    plist_encode(&root).map_err(|e| anyhow::anyhow!("{e}"))
}

fn parse_stream_ports(body: &[u8]) -> Result<(u16, u16)> {
    let root = plist_decode(body).map_err(|e| anyhow::anyhow!("{e}"))?;
    let s0 = root
        .dict_get("streams")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .context("stream SETUP response missing streams[0]")?;
    let data = s0
        .dict_get("dataPort")
        .and_then(Value::as_u64)
        .context("stream SETUP missing dataPort")? as u16;
    let ctl = s0
        .dict_get("controlPort")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u16;
    if data == 0 {
        anyhow::bail!("stream SETUP dataPort is 0");
    }
    Ok((data, ctl))
}

fn volume_body(level: f64) -> String {
    if level <= 0.0 {
        "volume: -144.000000\r\n".into()
    } else {
        let db = -30.0 + 30.0 * level.clamp(0.0, 1.0);
        format!("volume: {db:.6}\r\n")
    }
}

fn log_plist(label: &str, body: &[u8]) {
    match plist_decode(body) {
        Ok(v) => {
            let mut s = String::new();
            pretty_print_value(&v, 0, &mut s);
            info!("{label}\n{s}");
        }
        Err(e) => info!(bytes = body.len(), "{label} (not plist: {e})"),
    }
}

fn random_mac() -> Result<String> {
    let mut b = [0u8; 6];
    fill_random(&mut b).map_err(|e| anyhow::anyhow!("{e}"))?;
    b[0] = (b[0] | 0x02) & 0xFE;
    Ok(format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    ))
}

fn random_uuid() -> Result<String> {
    let mut b = [0u8; 16];
    fill_random(&mut b).map_err(|e| anyhow::anyhow!("{e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
        u32::from_be_bytes(b[0..4].try_into().unwrap()),
        u16::from_be_bytes(b[4..6].try_into().unwrap()),
        u16::from_be_bytes(b[6..8].try_into().unwrap()),
        u16::from_be_bytes(b[8..10].try_into().unwrap()),
        u64::from_be_bytes({
            let mut t = [0u8; 8];
            t[2..].copy_from_slice(&b[10..16]);
            t
        }) & 0x0000_ffff_ffff_ffff
    ))
}
