//! End-to-end integration: full session against a mock receiver.
//!
//! The mock plays the receiver for the WHOLE chain: transient pair-setup →
//! encrypted control channel → GET /info → session SETUP → event channel
//! (pushes POST /command, expects bare 200) → RECORD → stream SETUP →
//! receives RTP audio (decrypts with shk, asserts shk == K[..32]) → sync
//! packets → timing replies → retransmit (0x55 → 0xD6) → TEARDOWN.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use num_bigint::BigUint;
use sha2::{Digest, Sha512};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};

use airplay_crypto::hap::{derive_keys, HapChannel};
use airplay_rtsp::bplist::{self, Value};
use airplay_rtsp::client::PlainClient;
use airplay_rtsp::pairing::transient_pair;
use airplay_rtsp::session::{self, SessionConfig};
use airplay_stream::ntp;
use airplay_stream::pump::{self, AudioBlock, PumpConfig, PumpStats};

const PIN: &str = "3939";
const USERNAME: &str = "Pair-Setup";

// ---------- SRP server ----------

fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn pad_be(mut v: Vec<u8>, len: usize) -> Vec<u8> {
    if v.len() < len {
        let mut out = vec![0u8; len - v.len()];
        out.append(&mut v);
        out
    } else {
        v
    }
}

fn modulus() -> BigUint {
    const N: &str = concat!(
        "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
        "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437",
        "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
        "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05",
        "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB",
        "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
        "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718",
        "3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33",
        "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
        "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864",
        "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
        "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"
    );
    BigUint::parse_bytes(N.as_bytes(), 16).unwrap()
}

fn tlv_decode(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut pos = 0;
    while pos + 2 <= data.len() {
        let (t, n) = (data[pos], data[pos + 1] as usize);
        let v = &data[pos + 2..pos + 2 + n];
        if let Some(last) = out.last_mut().filter(|(lt, _)| *lt == t) {
            last.1.extend_from_slice(v);
        } else {
            out.push((t, v.to_vec()));
        }
        pos += 2 + n;
    }
    out
}

fn tlv_encode(entries: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, v) in entries {
        let mut rest = *v;
        loop {
            let n = rest.len().min(255);
            out.push(*t);
            out.push(n as u8);
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if rest.is_empty() {
                break;
            }
        }
    }
    out
}

fn tlv_get(map: &[(u8, Vec<u8>)], t: u8) -> Option<&[u8]> {
    map.iter().find(|(tt, _)| *tt == t).map(|(_, v)| v.as_slice())
}

struct SrpServer {
    salt: [u8; 16],
    b: BigUint,
    b_pub: Vec<u8>,
    n: BigUint,
    v: BigUint,
}

impl SrpServer {
    fn new() -> Self {
        let n = modulus();
        let g = BigUint::from(5u32);
        let salt = *b"mock-salt-16byt!";
        let userpass = sha512(&[format!("{USERNAME}:{PIN}").as_bytes()]);
        let x = BigUint::from_bytes_be(&sha512(&[&salt, &userpass]));
        let v = g.modpow(&x, &n);
        let mut b_bytes = [0u8; 32];
        getrandom::fill(&mut b_bytes).unwrap();
        let b = BigUint::from_bytes_be(&b_bytes);
        let k = BigUint::from_bytes_be(&sha512(&[
            &pad_be(n.to_bytes_be(), 384),
            &pad_be(g.to_bytes_be(), 384),
        ]));
        let b_pub_bn = (&k * &v + g.modpow(&b, &n)) % &n;
        Self {
            salt,
            b,
            b_pub: b_pub_bn.to_bytes_be(),
            n,
            v,
        }
    }

    fn m2(&self) -> Vec<u8> {
        tlv_encode(&[(0x02, &self.salt), (0x03, &self.b_pub)])
    }

    fn handle_m3(&self, body: &[u8]) -> Option<(Vec<u8>, [u8; 64])> {
        let map = tlv_decode(body);
        let a_pub = tlv_get(&map, 0x03)?;
        let m1 = tlv_get(&map, 0x04)?;
        let a = BigUint::from_bytes_be(a_pub);
        let u = BigUint::from_bytes_be(&sha512(&[
            &pad_be(a_pub.to_vec(), 384),
            &pad_be(self.b_pub.clone(), 384),
        ]));
        let s = (&a * self.v.modpow(&u, &self.n)).modpow(&self.b, &self.n);
        let k_sess = sha512(&[&s.to_bytes_be()]);
        let g = BigUint::from(5u32);
        let h_n = sha512(&[&self.n.to_bytes_be()]);
        let h_g = sha512(&[&g.to_bytes_be()]);
        let h_xor: Vec<u8> = h_n.iter().zip(h_g.iter()).map(|(x, y)| x ^ y).collect();
        let h_user = sha512(&[USERNAME.as_bytes()]);
        let m1_expect = sha512(&[&h_xor, &h_user, &self.salt, a_pub, &self.b_pub, &k_sess]);
        if m1 != m1_expect {
            return None;
        }
        let hamk = sha512(&[a_pub, &m1_expect, &k_sess]);
        Some((tlv_encode(&[(0x06, &[0x04]), (0x04, &hamk)]), k_sess))
    }
}

// ---------- plaintext helpers (pairing phase) ----------

async fn read_plain_request(s: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = vec![0u8; 65536];
    let mut total = 0;
    let mut he = None;
    let mut cl = 0usize;
    loop {
        let n = s.read(&mut buf[total..]).await.unwrap();
        assert!(n > 0, "closed mid-request");
        total += n;
        if he.is_none() {
            if let Some(p) = buf[..total].windows(4).position(|w| w == b"\r\n\r\n") {
                he = Some(p + 4);
                let head = String::from_utf8_lossy(&buf[..p]).to_string();
                cl = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .starts_with("content-length:")
                            .then(|| l.split(':').nth(1)?.trim().parse().ok())
                    })
                    .unwrap_or(Some(0))
                    .unwrap();
            }
        }
        if let Some(h) = he {
            if total >= h + cl {
                return (
                    String::from_utf8_lossy(&buf[..h]).to_string(),
                    buf[h..h + cl].to_vec(),
                );
            }
        }
    }
}

async fn write_response(s: &mut TcpStream, cseq: &str, body: &[u8], extra: &str) {
    let resp = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Length: {}\r\n{}\r\n",
        body.len(),
        extra
    );
    s.write_all(resp.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
}

fn cseq_of(head: &str) -> &str {
    head.lines()
        .find_map(|l| l.strip_prefix("CSeq: "))
        .unwrap()
        .trim()
}

// ---------- mock encrypted channel ----------

struct MockCrypto {
    hap: HapChannel,
    plain_rx: Vec<u8>,
}

impl MockCrypto {
    async fn read_request(&mut self, s: &mut TcpStream) -> (String, Vec<u8>) {
        loop {
            if let Some(req) = self.try_parse() {
                return req;
            }
            let mut lenb = [0u8; 2];
            s.read_exact(&mut lenb).await.unwrap();
            let flen = u16::from_le_bytes(lenb) as usize;
            let mut body = vec![0u8; flen + 16];
            s.read_exact(&mut body).await.unwrap();
            let pt = self.hap.decrypt(lenb, &body).unwrap();
            self.plain_rx.extend_from_slice(&pt);
        }
    }

    fn try_parse(&mut self) -> Option<(String, Vec<u8>)> {
        let he = self.plain_rx.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        let head = String::from_utf8_lossy(&self.plain_rx[..he]).to_string();
        let cl = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .starts_with("content-length:")
                    .then(|| l.split(':').nth(1)?.trim().parse().ok())
            })
            .unwrap_or(Some(0))
            .unwrap();
        if self.plain_rx.len() < he + cl {
            return None;
        }
        let body = self.plain_rx[he..he + cl].to_vec();
        self.plain_rx.drain(..he + cl);
        Some((head, body))
    }

    async fn respond(&mut self, s: &mut TcpStream, cseq: &str, body: &[u8], extra: &str) {
        // Binary-safe: headers as bytes, then the raw body.
        let head = format!(
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Length: {}\r\n{}\r\n",
            body.len(),
            extra
        );
        let mut resp = head.into_bytes();
        resp.extend_from_slice(body);
        let wire = self.hap.encrypt(&resp);
        s.write_all(&wire).await.unwrap();
    }
}

// ---------- shared verification state ----------

struct Ports {
    timing: u16,
    control: u16,
    shk: Vec<u8>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_session_end_to_end() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let data_udp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_udp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let data_port = data_udp.local_addr().unwrap().port();
    let ctrl_port = control_udp.local_addr().unwrap().port();
    let event_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let event_port = event_listener.local_addr().unwrap().port();
    let (event_keys_tx, mut event_keys_rx) = mpsc::channel::<HapChannel>(1);

    let audio_count = Arc::new(AtomicUsize::new(0));
    let (ports_tx, mut ports_rx) = mpsc::channel::<Ports>(1);
    let (event_ok_tx, mut event_ok_rx) = mpsc::channel::<bool>(1);
    let (teardown_tx, mut teardown_rx) = mpsc::channel::<bool>(1);

    // ---------- mock receiver ----------
    let audio_count_srv = audio_count.clone();
    let data_udp_srv = data_udp.clone();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let srp = SrpServer::new();

        // Pairing.
        let (head, _body) = read_plain_request(&mut s).await;
        assert!(head.contains("/pair-setup"));
        write_response(&mut s, cseq_of(&head), &srp.m2(), "").await;
        let (head, body) = read_plain_request(&mut s).await;
        let (m4, server_k) = srp.handle_m3(&body).expect("bad client proof");
        write_response(&mut s, cseq_of(&head), &m4, "").await;

        // Encrypted channel: receiver reads Control-Write, writes Control-Read.
        let ctrl_read: [u8; 32] = airplay_crypto::hkdf::hkdf_sha512(
            "Control-Salt",
            "Control-Write-Encryption-Key",
            server_k.as_slice(),
            32,
        )
        .try_into()
        .unwrap();
        let ctrl_write: [u8; 32] = airplay_crypto::hkdf::hkdf_sha512(
            "Control-Salt",
            "Control-Read-Encryption-Key",
            server_k.as_slice(),
            32,
        )
        .try_into()
        .unwrap();
        let mut crypto = MockCrypto {
            hap: HapChannel::new(ctrl_write, ctrl_read),
            plain_rx: Vec::new(),
        };

        // Event-channel keys for the mock: write = Events-Write, read = Events-Read.
        let ev_write: [u8; 32] = airplay_crypto::hkdf::hkdf_sha512(
            "Events-Salt",
            "Events-Write-Encryption-Key",
            server_k.as_slice(),
            32,
        )
        .try_into()
        .unwrap();
        let ev_read: [u8; 32] = airplay_crypto::hkdf::hkdf_sha512(
            "Events-Salt",
            "Events-Read-Encryption-Key",
            server_k.as_slice(),
            32,
        )
        .try_into()
        .unwrap();
        event_keys_tx
            .send(HapChannel::new(ev_write, ev_read))
            .await
            .ok();

        let mut ports = Ports {
            timing: 0,
            control: 0,
            shk: Vec::new(),
        };
        let mut ports_sent = false;

        loop {
            let (head, body) = crypto.read_request(&mut s).await;
            let cseq = cseq_of(&head).to_string();
            if head.starts_with("GET /info") {
                let mut m = BTreeMap::new();
                m.insert("deviceID".into(), Value::String("AA:BB:CC:DD:EE:FF".into()));
                crypto
                    .respond(&mut s, &cseq, &bplist::encode(&Value::Dict(m)), "")
                    .await;
            } else if head.starts_with("SETUP") && !body.is_empty() {
                let plist = bplist::decode(&body).unwrap();
                if let Value::Dict(d) = &plist {
                    if d.contains_key("timingProtocol") {
                        assert_eq!(d.get("timingProtocol"), Some(&Value::String("NTP".into())));
                        if let Some(Value::Int(i)) = d.get("timingPort") {
                            ports.timing = *i as u16;
                        }
                        let mut r = BTreeMap::new();
                        r.insert("eventPort".into(), Value::Int(event_port as i128));
                        crypto
                            .respond(
                                &mut s,
                                &cseq,
                                &bplist::encode(&Value::Dict(r)),
                                "Session: 99\r\n",
                            )
                            .await;
                    } else if d.contains_key("streams") {
                        if let Some(Value::Array(streams)) = d.get("streams") {
                            if let Some(Value::Dict(st)) = streams.first() {
                                assert_eq!(st.get("type"), Some(&Value::Int(0x60)));
                                assert_eq!(st.get("spf"), Some(&Value::Int(352)));
                                assert_eq!(st.get("ct"), Some(&Value::Int(2)));
                                if let Some(Value::Data(k)) = st.get("shk") {
                                    // Audio key rule: shk == first 32 bytes of K.
                                    assert_eq!(k.as_slice(), &server_k[..32]);
                                    ports.shk = k.clone();
                                }
                                if let Some(Value::Int(cp)) = st.get("controlPort") {
                                    ports.control = *cp as u16;
                                }
                            }
                        }
                        let mut st0 = BTreeMap::new();
                        st0.insert("dataPort".into(), Value::Int(data_port as i128));
                        st0.insert("controlPort".into(), Value::Int(ctrl_port as i128));
                        st0.insert("type".into(), Value::Int(0x60));
                        let mut r = BTreeMap::new();
                        r.insert("streams".into(), Value::Array(vec![Value::Dict(st0)]));
                        crypto
                            .respond(&mut s, &cseq, &bplist::encode(&Value::Dict(r)), "")
                            .await;
                        if !ports_sent {
                            ports_sent = true;
                            // Audio receive task.
                            let shk2 = ports.shk.clone();
                            let ac = audio_count_srv.clone();
                            let du = data_udp_srv.clone();
                            tokio::spawn(async move {
                                verify_audio(du, shk2, ac).await;
                            });
                            ports_tx
                                .send(Ports {
                                    timing: ports.timing,
                                    control: ports.control,
                                    shk: ports.shk.clone(),
                                })
                                .await
                                .ok();
                        }
                    }
                }
            } else if head.starts_with("RECORD") || head.starts_with("POST /feedback") || head.starts_with("SET_PARAMETER") {
                crypto.respond(&mut s, &cseq, &[], "").await;
            } else if head.starts_with("TEARDOWN") {
                crypto.respond(&mut s, &cseq, &[], "").await;
                teardown_tx.send(true).await.ok();
                break;
            } else {
                crypto.respond(&mut s, &cseq, &[], "").await;
            }
        }
    });

    // ---------- event channel mock ----------
    let event_task = tokio::spawn(async move {
        let (mut es, _) = event_listener.accept().await.unwrap();
        // Give the client responder a moment, then push POST /command.
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Keys arrive via a side channel: derive from the same SRP K… but the
        // event task does not know K. Instead it waits for the pairing phase
        // to publish keys — simplest: this task is started AFTER pairing in
        // the server flow. (Kept simple: we re-derive below using a key
        // received through a channel.)
        let mut hap: HapChannel = event_keys_rx.recv().await.unwrap();
        let req = "POST /command RTSP/1.0\r\nCSeq: 777\r\nContent-Length: 0\r\n\r\n";
        es.write_all(&hap.encrypt(req.as_bytes())).await.unwrap();
        let mut lenb = [0u8; 2];
        es.read_exact(&mut lenb).await.unwrap();
        let flen = u16::from_le_bytes(lenb) as usize;
        let mut body = vec![0u8; flen + 16];
        es.read_exact(&mut body).await.unwrap();
        let pt = hap.decrypt(lenb, &body).unwrap();
        let text = String::from_utf8_lossy(&pt).to_string();
        assert!(text.starts_with("RTSP/1.0 200 OK"), "{text}");
        assert!(text.contains("CSeq: 777"), "{text}");
        event_ok_tx.send(true).await.ok();
    });

    // ---------- client ----------
    let mut plain = PlainClient::connect(addr).await.unwrap();
    let outcome = transient_pair(&mut plain).await.expect("pairing failed");
    let keys = derive_keys(&outcome.session_key);
    let cfg = SessionConfig::default();
    let session = session::establish(plain, keys, &cfg)
        .await
        .expect("session establish failed");

    // Streaming stack (mirrors the run orchestrator). The NTP timing server
    // is started inside establish() — verify replies via session counters.
    let clock = Arc::new(ntp::StreamClock::new(44100));
    let stats = Arc::new(PumpStats::default());
    let (sd_tx, sd_rx) = watch::channel(false);
    let sync_task = {
        let s = session.control_socket.clone();
        let dest = session.sync_dest();
        let c = clock.clone();
        let sd = sd_rx.clone();
        tokio::spawn(async move { ntp::sync_sender(s, dest, c, sd).await })
    };
    let pump_task = {
        let data_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let cfgp = PumpConfig {
            dest_data: session.data_dest(),
            control_socket: session.control_socket.clone(),
            ssrc: session.session_id,
            audio_key: session.audio_key,
            spf: 352,
            rate: 44100,
        };
        let (_tx, block_rx) = mpsc::channel::<AudioBlock>(64);
        tokio::spawn(pump::run(
            cfgp,
            data_socket,
            clock.clone(),
            block_rx,
            stats.clone(),
            sd_rx.clone(),
        ))
    };

    // ---------- verifications driven by mock-observed state ----------
    let ports = tokio::time::timeout(Duration::from_secs(5), ports_rx.recv())
        .await
        .expect("ports never reported")
        .expect("ports channel closed");

    // Audio: wait for ≥10 verified packets.
    tokio::time::timeout(Duration::from_secs(5), async {
        while audio_count.load(Ordering::Relaxed) < 10 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("audio packets did not arrive");

    // Timing: send a request, expect a reply with our reftime echoed.
    let tq = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut req = [0u8; 32];
    req[0] = 0x80;
    req[1] = 0xD2;
    req[24..32].copy_from_slice(&0xAABBCCDDEEFF0011u64.to_be_bytes());
    tq.send_to(&req, ("127.0.0.1", ports.timing)).await.unwrap();
    let mut resp = [0u8; 32];
    tokio::time::timeout(Duration::from_secs(2), tq.recv_from(&mut resp))
        .await
        .expect("no timing reply")
        .unwrap();
    assert_eq!(resp[1], 0xD3);
    assert_eq!(&resp[8..16], &0xAABBCCDDEEFF0011u64.to_be_bytes());
    assert!(session.timing_replies.load(Ordering::Relaxed) > 0);

    // Retransmit: request seq 0, expect 0xD6 + the full original packet.
    // Sync packets (0xD4, 20B) may interleave on the same socket — tolerate.
    let rtx = [0x80, 0xD5, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01];
    control_udp
        .send_to(&rtx, ("127.0.0.1", ports.control))
        .await
        .unwrap();
    let mut got_rtx = false;
    let mut got_sync = false;
    for _ in 0..10 {
        let mut rb = [0u8; 2048];
        let (rn, _) = match tokio::time::timeout(Duration::from_secs(3), control_udp.recv_from(&mut rb)).await {
            Ok(v) => v.unwrap(),
            Err(_) => break,
        };
        if rb[1] == 0xD4 && rn == 20 {
            assert_eq!(&rb[2..4], &0x0007u16.to_be_bytes());
            got_sync = true;
            continue;
        }
        if rb[1] == 0xD6 {
            assert_eq!(rb[0], 0x80);
            assert_eq!(&rb[2..4], &0u16.to_be_bytes());
            assert!(rn > 100, "retransmit should carry the full packet, got {rn}");
            got_rtx = true;
            break;
        }
    }
    assert!(got_rtx, "never got the 0xD6 retransmit reply");
    assert!(got_sync, "never got a sync packet (0xD4)");

    // Event channel roundtrip.
    tokio::time::timeout(Duration::from_secs(5), event_ok_rx.recv())
        .await
        .expect("event channel no answer");

    // ---------- shutdown ----------
    sd_tx.send(true).unwrap();
    let _ = pump_task.await;
    sync_task.abort();
    session.teardown().await;

    tokio::time::timeout(Duration::from_secs(3), teardown_rx.recv())
        .await
        .expect("receiver never saw TEARDOWN");

    let _ = server.await;
    event_task.abort();
}

/// Receive audio packets and verify decryptability + ALAC silence shape.
async fn verify_audio(data_udp: Arc<UdpSocket>, shk: Vec<u8>, count: Arc<AtomicUsize>) {
    let cipher = ChaCha20Poly1305::new(shk.as_slice().try_into().unwrap());
    let mut buf = [0u8; 2048];
    let mut last_seq: Option<u16> = None;
    loop {
        let (n, _) = match data_udp.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        if n < 12 + 16 + 8 {
            continue;
        }
        let pkt = &buf[..n];
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        if let Some(l) = last_seq {
            assert_eq!(seq, l.wrapping_add(1), "seq must advance by 1");
        }
        last_seq = Some(seq);
        let nonce8: [u8; 8] = pkt[n - 8..].try_into().unwrap();
        let mut nonce12 = [0u8; 12];
        nonce12[4..].copy_from_slice(&nonce8);
        let pt = cipher
            .decrypt(
                (&nonce12).into(),
                Payload {
                    msg: &pkt[12..n - 8],
                    aad: &pkt[4..12],
                },
            )
            .expect("audio packet must decrypt");
        // Starved input → ALAC silence frame (1412 bytes, header 0x20 0x00 0x02).
        assert_eq!(pt.len(), 1412, "ALAC frame size");
        assert_eq!(&pt[..3], &[0x20, 0x00, 0x02], "ALAC silence header");
        count.fetch_add(1, Ordering::Relaxed);
    }
}
