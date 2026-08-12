//! RTSP client: plaintext phase (pairing/probe) + encrypted control channel
//! (post-pairing), sharing one request builder and response parser.
//!
//! Wire facts: whole RTSP exchanges ride inside HAP frames after pairing;
//! late responses are skipped by CSeq to keep the TCP byte stream and the
//! HAP read-nonce sequence intact.
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:695-740, 1596-1618;
//!  airplay-cli/DESIGN.md §10 (CSeq skip, nonce preservation)]

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use airplay_crypto::hap::HapChannel;

const USER_AGENT: &str = "AirPlay/550.10";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE: usize = 1 << 20; // 1 MiB cap

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout waiting for response")]
    Timeout,
    #[error("connect timeout")]
    ConnectTimeout,
    #[error("malformed response (status line)")]
    MalformedStatus,
    #[error("response exceeded size cap")]
    TooLarge,
    #[error("HAP decrypt failed — key/nonce desync")]
    Decrypt,
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One plaintext RTSP connection with auto-incremented CSeq.
pub struct PlainClient {
    stream: TcpStream,
    cseq: u32,
    dacp_id: String,
    active_remote: u32,
}

impl PlainClient {
    pub async fn connect(addr: SocketAddr) -> Result<Self, ClientError> {
        let stream = tokio::time::timeout(TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| ClientError::ConnectTimeout)??;
        stream.set_nodelay(true).ok();
        // Fixed per-connection AirPlay identity (random 64-bit DACP ID).
        let mut id = [0u8; 8];
        let _ = getrandom::fill(&mut id);
        let dacp_id = id.iter().map(|b| format!("{b:02X}")).collect::<String>();
        let active_remote = u32::from_be_bytes(id[..4].try_into().unwrap());
        Ok(Self {
            stream,
            cseq: 0,
            dacp_id,
            active_remote,
        })
    }

    pub fn dacp_id(&self) -> &str {
        &self.dacp_id
    }

    pub fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.peer_addr()
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    pub fn next_cseq(&mut self) -> u32 {
        self.cseq += 1;
        self.cseq
    }

    /// Upgrade this plaintext connection into the encrypted control channel
    /// (HAP nonce counters start at 0 on both sides from here).
    pub fn into_crypto(self, hap: HapChannel) -> CryptoClient {
        CryptoClient {
            stream: self.stream,
            hap,
            cseq: self.cseq,
            dacp_id: self.dacp_id,
            active_remote: self.active_remote,
            plain_rx: Vec::new(),
        }
    }

    /// Send one request and read the complete response.
    pub async fn request(
        &mut self,
        method: &str,
        path: &str,
        extra_headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, ClientError> {
        let cseq = self.next_cseq();
        let req = build_request(
            method,
            path,
            cseq,
            &self.dacp_id,
            self.active_remote,
            extra_headers,
            body,
        );
        tracing::debug!(%method, %path, cseq, body_len = body.len(), "rtsp request");
        tracing::trace!(">> request:\n{}", String::from_utf8_lossy(&req));

        tokio::time::timeout(TIMEOUT, self.stream.write_all(&req))
            .await
            .map_err(|_| ClientError::Timeout)??;

        let mut buf = Vec::with_capacity(4096);
        let resp = tokio::time::timeout(TIMEOUT, read_response_from(&mut self.stream, &mut buf))
            .await
            .map_err(|_| ClientError::Timeout)??;
        tracing::debug!(status = resp.status, cseq, body_len = resp.body.len(), "rtsp response");
        Ok(resp)
    }
}

/// Build a complete RTSP request (shared by plain + encrypted channels).
fn build_request(
    method: &str,
    path: &str,
    cseq: u32,
    dacp_id: &str,
    active_remote: u32,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut req = format!(
        "{method} {path} RTSP/1.0\r\n\
         CSeq: {cseq}\r\n\
         User-Agent: {USER_AGENT}\r\n\
         DACP-ID: {dacp_id}\r\n\
         Active-Remote: {active_remote}\r\n\
         Client-Instance: {dacp_id}\r\n\
         X-Apple-Client-Name: airplay\r\n"
    );
    if method == "SETUP" {
        // owntone/pyatv parity.
        req.push_str("X-Apple-StreamID: 1\r\n");
    }
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    let mut out = req.into_bytes();
    out.extend_from_slice(body);
    out
}

/// Parse one complete response from the front of `buf`, if present.
fn try_parse_response(buf: &mut Vec<u8>) -> Option<Response> {
    let he = find_subslice(buf, b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buf[..he]).to_string();
    let content_len = head
        .lines()
        .find_map(|l| {
            l.split_once(':').and_then(|(k, v)| {
                k.trim()
                    .eq_ignore_ascii_case("Content-Length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    if buf.len() < he + content_len {
        return None;
    }
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let mut it = status_line.splitn(3, ' ');
    let _version = it.next();
    let status: u16 = it.next()?.parse().ok()?;
    let reason = it.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let body = buf[he..he + content_len].to_vec();
    buf.drain(..he + content_len);
    Some(Response {
        status,
        reason,
        headers,
        body,
    })
}

/// Read from the stream until one complete response can be parsed.
async fn read_response_from(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
) -> Result<Response, ClientError> {
    loop {
        if let Some(resp) = try_parse_response(buf) {
            return Ok(resp);
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed by peer",
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_RESPONSE {
            return Err(ClientError::TooLarge);
        }
    }
}

/// Encrypted RTSP control channel (post-pairing).
pub struct CryptoClient {
    stream: TcpStream,
    hap: HapChannel,
    cseq: u32,
    dacp_id: String,
    active_remote: u32,
    /// Decrypted-but-not-yet-parsed plaintext. A timed-out exchange leaves
    /// its response here; the next exchange skips it by CSeq.
    plain_rx: Vec<u8>,
}

impl CryptoClient {
    pub async fn request(
        &mut self,
        method: &str,
        path: &str,
        extra_headers: &[(String, String)],
        body: &[u8],
        deadline: Duration,
    ) -> Result<Response, ClientError> {
        tokio::time::timeout(deadline, self.request_inner(method, path, extra_headers, body))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn request_inner(
        &mut self,
        method: &str,
        path: &str,
        extra_headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Response, ClientError> {
        self.cseq += 1;
        let cseq = self.cseq;
        let req = build_request(
            method,
            path,
            cseq,
            &self.dacp_id,
            self.active_remote,
            extra_headers,
            body,
        );
        tracing::debug!(%method, %path, cseq, body_len = body.len(), "rtsp(c) request");
        tracing::trace!(">> request (plaintext):\n{}", String::from_utf8_lossy(&req));

        let wire = self.hap.encrypt(&req);
        self.stream.write_all(&wire).await?;

        // Read HAP frames until a response with OUR CSeq is fully parsed.
        loop {
            if let Some(resp) = try_parse_response(&mut self.plain_rx) {
                match resp.header("CSeq").and_then(|c| c.parse::<u32>().ok()) {
                    Some(rc) if rc == cseq => {
                        tracing::debug!(status = resp.status, cseq, "rtsp(c) response");
                        return Ok(resp);
                    }
                    other => {
                        tracing::warn!(?other, want = cseq, "skipping late/foreign response by CSeq");
                        continue;
                    }
                }
            }
            // One HAP frame: [2B LE len][ct+16B tag].
            let mut lenb = [0u8; 2];
            self.stream.read_exact(&mut lenb).await?;
            let flen = u16::from_le_bytes(lenb) as usize;
            if flen > 1024 {
                return Err(ClientError::MalformedStatus);
            }
            let mut body = vec![0u8; flen + 16];
            self.stream.read_exact(&mut body).await?;
            let pt = self
                .hap
                .decrypt(lenb, &body)
                .map_err(|_| ClientError::Decrypt)?;
            self.plain_rx.extend_from_slice(&pt);
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// Mock receiver: reads one request, replies with a fixed response.
    #[tokio::test]
    async fn request_response_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("GET /info RTSP/1.0\r\n"));
            assert!(req.contains("CSeq: 1\r\n"));
            assert!(req.contains("User-Agent: AirPlay/550.10\r\n"));
            let body = b"hello";
            let resp = format!(
                "RTSP/1.0 200 OK\r\nContent-Length: {}\r\nCSeq: 1\r\n\r\n",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            s.write_all(body).await.unwrap();
        });

        let mut c = PlainClient::connect(addr).await.unwrap();
        let resp = c.request("GET", "/info", &[], &[]).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello");
        assert_eq!(resp.header("cseq"), Some("1"));
        server.await.unwrap();
    }
}
