//! Integration test: transient pair-setup M1..M4 against a mock receiver.
//!
//! The mock implements the server side of SRP-6a independently (verifies the
//! client proof M1, replies with salt/B and HAMK), exercising:
//! TLV continuation (384-byte A/B), request framing, status parsing, and the
//! full pairing state machine.

use airplay_rtsp::client::PlainClient;
use airplay_rtsp::pairing::transient_pair;

use num_bigint::BigUint;
use sha2::{Digest, Sha512};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PIN: &str = "3939";
const USERNAME: &str = "Pair-Setup";

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

// --- tiny TLV8 helpers for the mock (decode + encode) ---
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

struct MockServer {
    salt: [u8; 16],
    b: BigUint,
    b_pub: Vec<u8>,
    n: BigUint,
    v: BigUint,
}

impl MockServer {
    fn new() -> Self {
        let n = modulus();
        let g = BigUint::from(5u32);
        let salt = *b"mock-salt-16byt!";
        let userpass = sha512(&[format!("{USERNAME}:{PIN}").as_bytes()]);
        let x = BigUint::from_bytes_be(&sha512(&[&salt, &userpass]));
        let v = g.modpow(&x, &n);
        let k = BigUint::from_bytes_be(&sha512(&[
            &pad_be(n.to_bytes_be(), 384),
            &pad_be(g.to_bytes_be(), 384),
        ]));
        let mut b_bytes = [0u8; 32];
        getrandom::fill(&mut b_bytes).unwrap();
        let b = BigUint::from_bytes_be(&b_bytes);
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

    /// Verify M3, return (m4_body, server_session_key) or None on bad proof.
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

async fn read_request(s: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut buf = vec![0u8; 65536];
    let mut total = 0usize;
    let mut header_end = None;
    let mut content_len = 0usize;
    loop {
        let n = s.read(&mut buf[total..]).await.unwrap();
        assert!(n > 0, "connection closed mid-request");
        total += n;
        if header_end.is_none() {
            if let Some(pos) = buf[..total]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                assert!(head.contains("X-Apple-HKP: 4"), "missing HKP header: {head}");
                content_len = head
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
        if let Some(he) = header_end {
            if total >= he + content_len {
                let head = String::from_utf8_lossy(&buf[..he]).to_string();
                return (head, buf[he..he + content_len].to_vec());
            }
        }
    }
}

async fn write_response(s: &mut tokio::net::TcpStream, cseq: &str, body: &[u8]) {
    let resp = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(resp.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
}

#[tokio::test]
async fn transient_pair_against_mock_receiver() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = MockServer::new();

    let server_task = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();

        // M1 → M2
        let (head, body) = read_request(&mut s).await;
        assert!(head.starts_with("POST /pair-setup RTSP/1.0"), "{head}");
        let m1 = tlv_decode(&body);
        assert_eq!(tlv_get(&m1, 0x00), Some(&[0x00][..])); // Method
        assert_eq!(tlv_get(&m1, 0x06), Some(&[0x01][..])); // State
        assert_eq!(tlv_get(&m1, 0x13), Some(&[0x10][..])); // Flags
        let cseq = head
            .lines()
            .find_map(|l| l.strip_prefix("CSeq: "))
            .unwrap()
            .to_string();
        write_response(&mut s, &cseq, &server.m2()).await;

        // M3 → M4
        let (head, body) = read_request(&mut s).await;
        let (m4, server_key) = server.handle_m3(&body).expect("client M1 proof invalid");
        let cseq = head
            .lines()
            .find_map(|l| l.strip_prefix("CSeq: "))
            .unwrap()
            .to_string();
        write_response(&mut s, &cseq, &m4).await;
        server_key
    });

    let mut client = PlainClient::connect(addr).await.unwrap();
    let outcome = transient_pair(&mut client).await.expect("pairing failed");
    let server_key = server_task.await.unwrap();

    assert_eq!(
        outcome.session_key, server_key,
        "client and server session keys must agree"
    );
    assert!(outcome
        .transcript
        .iter()
        .any(|l| l.contains("HAMK") || l.contains("proof")));
}
