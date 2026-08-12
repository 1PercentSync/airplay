//! AP2 session orchestration: GET /info → session SETUP (NTP) → event
//! channel → RECORD → stream SETUP → keepalive + volume.
//!
//! Sequence and plist shapes:
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1625-1758 (SETUP bodies,
//!  order: event channel OPEN + RECORD accepted BEFORE stream SETUP),
//!  610-615+ (event responder: bare 200 OK, Server + optional CSeq only);
//!  airplay-cli/DESIGN.md §10 (keepalive cadence, miss tolerance)]

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use airplay_core::AlacFormat;
use airplay_crypto::hap::{DerivedKeys, HapChannel};

use crate::bplist::{self, Value};
use crate::client::{ClientError, CryptoClient, PlainClient, Response};

const CONTROL_DEADLINE: Duration = Duration::from_secs(10);
const FEEDBACK_DEADLINE: Duration = Duration::from_secs(2);
const FEEDBACK_INTERVAL: Duration = Duration::from_secs(2);
const MAX_FEEDBACK_MISSES: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("transport at {step}: {source}")]
    Transport {
        step: &'static str,
        #[source]
        source: ClientError,
    },
    #[error("HTTP {status} at {step}")]
    Status { step: &'static str, status: u16 },
    #[error("malformed plist at {step}: {detail}")]
    BadPlist {
        step: &'static str,
        detail: String,
    },
    #[error("missing {field} at {step}")]
    MissingField {
        step: &'static str,
        field: &'static str,
    },
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SessionConfig {
    /// Volume percent 0..100 (0 = mute sentinel −144 dB); pushed after RECORD.
    pub volume_pct: f32,
    pub latency_min: u32,
    pub latency_max: u32,
    pub format: AlacFormat,
    /// Sender display name (X-Apple-Client-Name is fixed in the client).
    pub sender_name: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            volume_pct: 50.0,
            latency_min: 11025,
            latency_max: 88200,
            format: AlacFormat::L44100_16_2,
            sender_name: "airplay".into(),
        }
    }
}

/// Task guard: aborts spawned tasks if establish() bails out early.
struct AbortOnDrop(Vec<JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// An established AP2 session, ready for the RTP pump + NTP tasks.
pub struct Session {
    pub client: Arc<Mutex<CryptoClient>>,
    pub receiver: SocketAddr,
    /// Numeric session id: SSRC + streamConnectionID + rtsp URI tail.
    pub session_id: u32,
    pub rtsp_session: String,
    /// Receiver ports for audio data / control (sync + retransmit).
    pub data_port: u16,
    pub control_port: u16,
    /// Our bound sockets: timing (NTP replies) and control (0x55/0xD6, sync).
    pub timing_socket: Arc<UdpSocket>,
    pub control_socket: Arc<UdpSocket>,
    pub audio_key: [u8; 32],
    /// Timing-request reply counter (diagnostics).
    pub timing_replies: Arc<std::sync::atomic::AtomicU64>,
    /// Receiver of the death broadcast (reason string).
    pub dead_rx: watch::Receiver<String>,
    dead_tx: watch::Sender<String>,
    tasks: Vec<JoinHandle<()>>,
}

impl Session {
    /// Destination for sync packets (receiver control port).
    pub fn sync_dest(&self) -> SocketAddr {
        SocketAddr::new(self.receiver.ip(), self.control_port)
    }

    pub fn data_dest(&self) -> SocketAddr {
        SocketAddr::new(self.receiver.ip(), self.data_port)
    }

    /// Push volume (0..100; 0 → −144 mute sentinel, else −30..0 dB linear).
    ///
    /// [evidence: raop_sender.cpp:429-442 (pyatv pct_to_dbfs, "volume: %.6f")]
    pub async fn set_volume(&self, pct: f32) -> Result<(), SessionError> {
        let pct = pct.clamp(0.0, 100.0);
        let dbfs = if pct < 0.01 { -144.0 } else { -30.0 + 0.3 * pct as f64 };
        let body = format!("volume: {dbfs:.6}");
        let resp = self
            .client
            .lock()
            .await
            .request(
                "SET_PARAMETER",
                &self.rtsp_uri(),
                &[
                    ("Content-Type".into(), "text/parameters".into()),
                    ("Session".into(), self.rtsp_session.clone()),
                ],
                body.as_bytes(),
                CONTROL_DEADLINE,
            )
            .await
            .map_err(|e| SessionError::Transport {
                step: "SET_PARAMETER volume",
                source: e,
            })?;
        if resp.status != 200 {
            return Err(SessionError::Status {
                step: "SET_PARAMETER volume",
                status: resp.status,
            });
        }
        Ok(())
    }

    fn rtsp_uri(&self) -> String {
        format!("rtsp://{}/{}", self.receiver.ip(), self.session_id)
    }

    /// Farewell TEARDOWN (best effort, 250 ms budget), then stop tasks.
    pub async fn teardown(self) {
        let mut req = self.client.lock().await;
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            req.request(
                "TEARDOWN",
                &format!("rtsp://{}/{}", self.receiver.ip(), self.session_id),
                &[],
                &[],
                CONTROL_DEADLINE,
            ),
        )
        .await;
        drop(req);
        for t in self.tasks {
            t.abort();
        }
        let _ = self.dead_tx.send("teardown".into());
    }
}

/// Establish a full session over an already-paired plaintext connection.
pub async fn establish(
    plain: PlainClient,
    keys: DerivedKeys,
    cfg: &SessionConfig,
) -> Result<Session, SessionError> {
    let receiver = plain.peer_addr()?;
    let dacp = plain.dacp_id().to_string();
    let client = Arc::new(Mutex::new(plain.into_crypto(keys.control)));
    let (dead_tx, dead_rx) = watch::channel(String::new());
    let mut tasks = AbortOnDrop(Vec::new());

    // GET /info on the encrypted channel (required before SETUP).
    let info = request(&client, "GET", "/info", &[], &[], "GET /info(c)").await?;
    tracing::debug!(body_len = info.body.len(), "encrypted /info ok");

    // Bind our timing + control sockets BEFORE session SETUP (ports are
    // advertised in the request body).
    let timing_socket = Arc::new(UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0)).await?);
    let control_socket = Arc::new(UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0)).await?);
    let timing_port = timing_socket.local_addr()?.port();
    let control_port = control_socket.local_addr()?.port();

    // The timing server MUST be live before the stream SETUP: the receiver
    // performs clock sync against our advertised timingPort around it
    // [evidence: airplay-cli ap2_client.c:1790-1811 starts the timing service
    // before session SETUP; FXChainPlayer STARTPLAYING runs firstSync before
    // sendSessionSetup]. Without it the receiver silently stalls SETUP.
    let timing_replies = Arc::new(std::sync::atomic::AtomicU64::new(0));
    tasks.0.push(tokio::spawn({
        let s = timing_socket.clone();
        let r = timing_replies.clone();
        async move { airplay_stream::ntp::timing_server(s, r).await }
    }));

    // Numeric session identity.
    let mut sid = [0u8; 4];
    let _ = getrandom::fill(&mut sid);
    let session_id = u32::from_be_bytes(sid);
    let rtsp_uri = format!("rtsp://{}/{}", receiver.ip(), session_id);

    // ---- session SETUP ----
    let device_id = dacp_id_colon(&dacp);
    let session_uuid = uuid_v4();
    let mut d = BTreeMap::new();
    d.insert("deviceID".into(), Value::String(device_id.clone()));
    d.insert("sessionUUID".into(), Value::String(session_uuid));
    d.insert("timingPort".into(), Value::Int(timing_port as i128));
    d.insert("timingProtocol".into(), Value::String("NTP".into()));
    d.insert("isMultiSelectAirPlay".into(), Value::Bool(true));
    d.insert("groupContainsGroupLeader".into(), Value::Bool(false));
    d.insert("macAddress".into(), Value::String(device_id));
    d.insert("model".into(), Value::String("iPhone14,3".into()));
    d.insert("name".into(), Value::String(cfg.sender_name.clone()));
    d.insert("osBuildVersion".into(), Value::String("20F66".into()));
    d.insert("osName".into(), Value::String("iPhone OS".into()));
    d.insert("osVersion".into(), Value::String("16.5".into()));
    d.insert("senderSupportsRelay".into(), Value::Bool(false));
    d.insert("sourceVersion".into(), Value::String("690.7.1".into()));
    d.insert("statsCollectionEnabled".into(), Value::Bool(false));
    let body = bplist::encode(&Value::Dict(d));
    let resp = request(
        &client,
        "SETUP",
        &rtsp_uri,
        &[(
            "Content-Type".into(),
            "application/x-apple-binary-plist".into(),
        )],
        &body,
        "SETUP session",
    )
    .await?;
    let rtsp_session = resp.header("Session").unwrap_or("1").to_string();
    let setup_plist = bplist::decode(&resp.body).map_err(|e| SessionError::BadPlist {
        step: "SETUP session",
        detail: e.to_string(),
    })?;
    let event_port = find_int(&setup_plist, "eventPort").ok_or(SessionError::MissingField {
        step: "SETUP session",
        field: "eventPort",
    })? as u16;
    tracing::info!(event_port, rtsp_session, "session SETUP ok");

    // ---- event channel (reverse TCP, full-time service) ----
    let event_stream = TcpStream::connect((receiver.ip(), event_port)).await?;
    event_stream.set_nodelay(true).ok();
    let event_hap = HapChannel::new(keys.events_read_key, keys.events_write_key);
    let mut event_shutdown = dead_tx.subscribe();
    tasks.0.push(tokio::spawn(async move {
        event_responder(event_stream, event_hap, &mut event_shutdown).await;
    }));

    // ---- RECORD ----
    request(&client, "RECORD", &rtsp_uri, &[], &[], "RECORD").await?;
    tracing::info!("RECORD ok");

    // ---- stream SETUP (type 0x60 realtime ALAC) ----
    let mut s = BTreeMap::new();
    s.insert("audioFormat".into(), Value::Int(cfg.format.code() as i128));
    s.insert("audioMode".into(), Value::String("default".into()));
    s.insert("controlPort".into(), Value::Int(control_port as i128));
    s.insert("ct".into(), Value::Int(2)); // ALAC
    s.insert("isMedia".into(), Value::Bool(true));
    s.insert("latencyMax".into(), Value::Int(cfg.latency_max as i128));
    s.insert("latencyMin".into(), Value::Int(cfg.latency_min as i128));
    s.insert("shk".into(), Value::Data(keys.audio_key.to_vec()));
    s.insert("spf".into(), Value::Int(352));
    s.insert("sr".into(), Value::Int(cfg.format.sample_rate() as i128));
    s.insert("type".into(), Value::Int(0x60));
    s.insert("supportsDynamicStreamID".into(), Value::Bool(false));
    s.insert("streamConnectionID".into(), Value::Int(session_id as i128));
    let mut streams_map = BTreeMap::new();
    streams_map.insert(
        "streams".into(),
        Value::Array(vec![Value::Dict(s)]),
    );
    let body = bplist::encode(&Value::Dict(streams_map));
    let resp = request(
        &client,
        "SETUP",
        &rtsp_uri,
        &[(
            "Content-Type".into(),
            "application/x-apple-binary-plist".into(),
        )],
        &body,
        "SETUP stream",
    )
    .await?;
    let stream_plist = bplist::decode(&resp.body).map_err(|e| SessionError::BadPlist {
        step: "SETUP stream",
        detail: e.to_string(),
    })?;
    let (data_port, ctrl_port) = parse_stream_ports(&stream_plist)?;
    tracing::info!(data_port, ctrl_port, "stream SETUP ok");

    // ---- keepalive: POST /feedback every ~2 s, tolerate 3 misses ----
    let client2 = client.clone();
    let dead_tx2 = dead_tx.clone();
    tasks.0.push(tokio::spawn(async move {
        keepalive(client2, dead_tx2).await;
    }));

    let tasks_vec = std::mem::take(&mut tasks.0);
    let session = Session {
        client,
        receiver,
        session_id,
        rtsp_session,
        data_port,
        control_port: ctrl_port,
        timing_socket,
        control_socket,
        audio_key: keys.audio_key,
        timing_replies,
        dead_rx,
        dead_tx,
        tasks: tasks_vec,
    };

    // ---- volume (a receiver can sit muted until told otherwise) ----
    if let Err(e) = session.set_volume(cfg.volume_pct).await {
        tracing::warn!(?e, "initial volume push failed (continuing)");
    }

    Ok(session)
}

/// One serialized encrypted RTSP exchange with status enforcement.
async fn request(
    client: &Arc<Mutex<CryptoClient>>,
    method: &str,
    uri: &str,
    extra: &[(String, String)],
    body: &[u8],
    step: &'static str,
) -> Result<Response, SessionError> {
    let resp = client
        .lock()
        .await
        .request(method, uri, extra, body, CONTROL_DEADLINE)
        .await
        .map_err(|e| SessionError::Transport { step, source: e })?;
    if resp.status != 200 {
        return Err(SessionError::Status {
            step,
            status: resp.status,
        });
    }
    Ok(resp)
}

/// /feedback loop. Consecutive misses ≤ 3 are tolerated; the third miss
/// declares the channel dead (recovery is the caller's job).
async fn keepalive(client: Arc<Mutex<CryptoClient>>, dead_tx: watch::Sender<String>) {
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(FEEDBACK_INTERVAL).await;
        let result = client
            .lock()
            .await
            .request("POST", "/feedback", &[], &[], FEEDBACK_DEADLINE)
            .await;
        match result {
            Ok(resp) if resp.status == 200 => {
                if misses > 0 {
                    tracing::info!(misses, "feedback recovered");
                }
                misses = 0;
            }
            Ok(resp) => {
                misses += 1;
                tracing::warn!(status = resp.status, misses, "feedback miss (status)");
            }
            Err(ClientError::Io(e)) => {
                // Hard error: the channel is already gone.
                let _ = dead_tx.send(format!("feedback I/O: {e}"));
                return;
            }
            Err(e) => {
                misses += 1;
                tracing::warn!(?e, misses, "feedback miss");
            }
        }
        if misses >= MAX_FEEDBACK_MISSES {
            let _ = dead_tx.send(format!("feedback {misses} consecutive misses"));
            return;
        }
    }
}

/// Event-channel responder: decrypt pushed requests, reply bare 200 OK
/// (Server + optional CSeq only — extra headers can corrupt the receiver's
/// realtime timeline).
///
/// [evidence: raop_sender.cpp:663-700]
async fn event_responder(
    mut stream: TcpStream,
    mut hap: HapChannel,
    shutdown: &mut watch::Receiver<String>,
) {
    let mut enc: Vec<u8> = Vec::new();
    let mut plain: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = stream.read(&mut buf) => {
                match n {
                    Ok(0) => {
                        tracing::warn!("event channel closed by receiver");
                        return;
                    }
                    Ok(n) => enc.extend_from_slice(&buf[..n]),
                    Err(e) => {
                        tracing::warn!(?e, "event channel read error");
                        return;
                    }
                }
            }
            _ = shutdown.changed() => return,
        }

        // Drain complete HAP frames → plaintext.
        loop {
            if enc.len() < 2 {
                break;
            }
            let flen = u16::from_le_bytes([enc[0], enc[1]]) as usize;
            if enc.len() < 2 + flen + 16 {
                break;
            }
            let lenb: [u8; 2] = [enc[0], enc[1]];
            let body = enc[2..2 + flen + 16].to_vec();
            match hap.decrypt(lenb, &body) {
                Ok(pt) => plain.extend_from_slice(&pt),
                Err(_) => {
                    tracing::error!("event channel decrypt failed, dropping connection");
                    return;
                }
            }
            enc.drain(..2 + flen + 16);
        }

        // Answer each complete RTSP request with an encrypted bare 200 OK.
        while let Some((cseq, total)) = complete_request(&plain) {
            let mut resp = String::from("RTSP/1.0 200 OK\r\nServer: AirTunes/550.10\r\n");
            if let Some(c) = cseq {
                resp.push_str(&format!("CSeq: {c}\r\n"));
            }
            resp.push_str("\r\n");
            let wire = hap.encrypt(resp.as_bytes());
            if stream.write_all(&wire).await.is_err() {
                return;
            }
            plain.drain(..total);
        }
    }
}

/// Locate one complete RTSP request in the plaintext buffer; return
/// (optional CSeq, total byte length).
fn complete_request(plain: &[u8]) -> Option<(Option<String>, usize)> {
    let he = plain
        .windows(4)
        .position(|w| w == b"\r\n\r\n")?
        + 4;
    let head = String::from_utf8_lossy(&plain[..he]);
    let mut content_len = 0usize;
    let mut cseq = None;
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            if k.trim().eq_ignore_ascii_case("Content-Length") {
                content_len = v.parse().unwrap_or(0);
            } else if k.trim().eq_ignore_ascii_case("CSeq") {
                cseq = Some(v.to_string());
            }
        }
    }
    let total = he + content_len;
    if plain.len() < total {
        return None;
    }
    Some((cseq, total))
}

fn find_int(v: &Value, key: &str) -> Option<i128> {
    match v {
        Value::Dict(m) => match m.get(key) {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        },
        _ => None,
    }
}

/// streams[0].{dataPort, controlPort?} — controlPort falls back to dataPort.
fn parse_stream_ports(v: &Value) -> Result<(u16, u16), SessionError> {
    if let Value::Dict(m) = v {
        if let Some(Value::Array(streams)) = m.get("streams") {
            if let Some(first) = streams.first() {
                let data = find_int(first, "dataPort").ok_or(SessionError::MissingField {
                    step: "SETUP stream",
                    field: "streams[0].dataPort",
                })? as u16;
                let ctrl = find_int(first, "controlPort")
                    .map(|c| c as u16)
                    .unwrap_or(data);
                return Ok((data, ctrl));
            }
        }
    }
    Err(SessionError::MissingField {
        step: "SETUP stream",
        field: "streams",
    })
}

/// "AABBCCDDEEFF1122" → "AA:BB:CC:DD:EE:FF" (first 6 bytes).
fn dacp_id_colon(dacp: &str) -> String {
    let pairs: Vec<&str> = dacp
        .as_bytes()
        .chunks(2)
        .take(6)
        .map(|c| std::str::from_utf8(c).unwrap_or("00"))
        .collect();
    pairs.join(":")
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = b[6] & 0x0F | 0x40;
    b[8] = b[8] & 0x3F | 0x80;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_ports_with_and_without_control() {
        let mut s = BTreeMap::new();
        s.insert("dataPort".into(), Value::Int(6000));
        let mut root = BTreeMap::new();
        root.insert("streams".into(), Value::Array(vec![Value::Dict(s)]));
        assert_eq!(parse_stream_ports(&Value::Dict(root)).unwrap(), (6000, 6000));

        let mut s = BTreeMap::new();
        s.insert("dataPort".into(), Value::Int(6000));
        s.insert("controlPort".into(), Value::Int(6001));
        let mut root = BTreeMap::new();
        root.insert("streams".into(), Value::Array(vec![Value::Dict(s)]));
        assert_eq!(parse_stream_ports(&Value::Dict(root)).unwrap(), (6000, 6001));
    }

    #[test]
    fn complete_request_detection() {
        assert_eq!(complete_request(b"POST /command RTSP/1.0\r\nCSeq: 7\r\n\r\n"), Some((Some("7".into()), 35)));
        let with_body = b"POST /x RTSP/1.0\r\nContent-Length: 5\r\n\r\nhelloPOST";
        assert_eq!(complete_request(with_body), Some((None, 44)));
        assert_eq!(complete_request(b"POST /x RTSP/1.0\r\n\r"), None);
    }

    #[test]
    fn dacp_colon_format() {
        assert_eq!(dacp_id_colon("A1B2C3D4E5F60708"), "A1:B2:C3:D4:E5:F6");
    }
}
