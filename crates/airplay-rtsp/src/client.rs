//! RTSP/1.0 client (plaintext). Serialised request/response on one TCP connection.
//!
//! `[evidence: pyatv support/rtsp.py:254-330 exchange; pyatv support/http.py:50-80 _format_message; owntone airplay.c:888-925 request_headers_add]`

use std::collections::BTreeMap;
use std::time::Duration;

use airplay_core::{Error, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, info};

pub const USER_AGENT: &str = "AirPlay/550.10";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct RtspResponse {
    pub protocol: String,
    pub status: u16,
    pub reason: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl RtspResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == want)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct RtspClient {
    stream: TcpStream,
    cseq: u32,
    dacp_id: String,
    active_remote: String,
    deadline: Duration,
}

impl RtspClient {
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let addr = format!("{host}:{port}");
        info!("rtsp connect {addr}");
        let stream = timeout(DEFAULT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| Error::Protocol(format!("connect timeout to {addr}")))?
            .map_err(|e| Error::Protocol(format!("connect {addr}: {e}")))?;
        stream.set_nodelay(true).ok();
        Ok(Self {
            stream,
            cseq: 0,
            dacp_id: format!("{:016X}", random_u64()),
            active_remote: format!("{}", (random_u64() as u32)),
            deadline: DEFAULT_TIMEOUT,
        })
    }

    pub async fn exchange(
        &mut self,
        method: &str,
        uri: &str,
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<RtspResponse> {
        let cseq = self.cseq;
        self.cseq = self.cseq.wrapping_add(1);

        // `[evidence: pyatv support/rtsp.py:268-273; owntone airplay.c:896-925]`
        let mut msg = format!("{method} {uri} RTSP/1.0\r\n");
        msg.push_str(&format!("CSeq: {cseq}\r\n"));
        msg.push_str(&format!("User-Agent: {USER_AGENT}\r\n"));
        msg.push_str(&format!("DACP-ID: {}\r\n", self.dacp_id));
        msg.push_str(&format!("Active-Remote: {}\r\n", self.active_remote));
        msg.push_str(&format!("Client-Instance: {}\r\n", self.dacp_id));
        for (k, v) in extra_headers {
            msg.push_str(&format!("{k}: {v}\r\n"));
        }
        if !body.is_empty() {
            msg.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        msg.push_str("\r\n");

        let mut wire = msg.into_bytes();
        wire.extend_from_slice(body);
        debug!(
            "rtsp send {} {} cseq={} body={}B",
            method,
            uri,
            cseq,
            body.len()
        );
        timeout(self.deadline, self.stream.write_all(&wire))
            .await
            .map_err(|_| Error::Protocol("write timeout".into()))?
            .map_err(|e| Error::Protocol(format!("write: {e}")))?;

        let resp = timeout(self.deadline, read_response(&mut self.stream))
            .await
            .map_err(|_| Error::Protocol(format!("no response to {method} {uri}")))??;
        debug!(
            "rtsp recv {} {} body={}B",
            resp.status,
            resp.reason,
            resp.body.len()
        );
        Ok(resp)
    }
}

async fn read_response(stream: &mut TcpStream) -> Result<RtspResponse> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| Error::Protocol(format!("read: {e}")))?;
        if n == 0 {
            return Err(Error::Protocol("early eof".into()));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(header_end) = find_header_end(&buf) {
            let header_bytes = &buf[..header_end];
            let rest = &buf[header_end..];
            let header_str = std::str::from_utf8(header_bytes)
                .map_err(|_| Error::Protocol("headers not utf-8".into()))?;
            let mut lines = header_str.split("\r\n");
            let first = lines
                .next()
                .ok_or_else(|| Error::Protocol("empty response".into()))?;
            let (protocol, status, reason) = parse_status_line(first)?;
            let mut headers = BTreeMap::new();
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            let content_len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = rest.to_vec();
            while body.len() < content_len {
                let n = stream
                    .read(&mut tmp)
                    .await
                    .map_err(|e| Error::Protocol(format!("read body: {e}")))?;
                if n == 0 {
                    return Err(Error::Protocol("eof mid-body".into()));
                }
                body.extend_from_slice(&tmp[..n]);
            }
            body.truncate(content_len);
            return Ok(RtspResponse {
                protocol,
                status,
                reason,
                headers,
                body,
            });
        }
        if buf.len() > 64 * 1024 {
            return Err(Error::Protocol("headers too large".into()));
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn parse_status_line(line: &str) -> Result<(String, u16, String)> {
    // RTSP/1.0 200 OK  or HTTP/1.1 200 OK
    let mut parts = line.splitn(3, ' ');
    let proto = parts
        .next()
        .ok_or_else(|| Error::Protocol(format!("bad status: {line}")))?;
    let code = parts
        .next()
        .ok_or_else(|| Error::Protocol(format!("bad status: {line}")))?
        .parse::<u16>()
        .map_err(|_| Error::Protocol(format!("bad status code: {line}")))?;
    let reason = parts.next().unwrap_or("").to_string();
    Ok((proto.to_string(), code, reason))
}

fn random_u64() -> u64 {
    // Identifiers only (DACP-ID / Active-Remote), not a session key.
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    t.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ std::process::id() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line() {
        let (p, c, r) = parse_status_line("RTSP/1.0 200 OK").unwrap();
        assert_eq!(p, "RTSP/1.0");
        assert_eq!(c, 200);
        assert_eq!(r, "OK");
    }
}
