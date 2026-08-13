//! Plaintext RTSP/1.0 client for probe `/info` and `/pair-setup`.
//!
//! [evidence: pyatv support/rtsp.py:86-89,254-300; http.py:50-80,110-140;
//! owntone airplay.c:889-933; raop_sender.cpp:545-567]

use airplay_core::{Error, Result, CLIENT_NAME, USER_AGENT};
use airplay_crypto::random::{random_u32, random_u64};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct Identity {
    pub dacp_id: String,
    pub active_remote: u32,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        Ok(Self {
            dacp_id: format!("{:016X}", random_u64()?),
            active_remote: random_u32()?,
        })
    }
}

#[derive(Debug)]
pub struct Response {
    pub code: u16,
    pub reason: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct RtspClient {
    stream: TcpStream,
    cseq: u32,
    identity: Identity,
}

impl RtspClient {
    pub async fn connect(addr: SocketAddr, identity: Identity) -> Result<Self> {
        let stream = timeout(IO_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::Rtsp("connect timed out".into()))?
            .map_err(Error::from)?;
        Ok(Self {
            stream,
            cseq: 0,
            identity,
        })
    }

    pub async fn request(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, &str)],
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Response> {
        let cseq = self.cseq;
        self.cseq += 1;
        let mut msg = format!("{method} {uri} RTSP/1.0\r\n");
        msg.push_str(&format!("CSeq: {cseq}\r\n"));
        msg.push_str(&format!("User-Agent: {USER_AGENT}\r\n"));
        msg.push_str(&format!("DACP-ID: {}\r\n", self.identity.dacp_id));
        msg.push_str(&format!("Active-Remote: {}\r\n", self.identity.active_remote));
        msg.push_str(&format!("Client-Instance: {}\r\n", self.identity.dacp_id));
        msg.push_str(&format!("X-Apple-Client-Name: {CLIENT_NAME}\r\n"));
        for (k, v) in extra {
            msg.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(ct) = content_type {
            msg.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if !body.is_empty() {
            msg.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        msg.push_str("\r\n");
        let mut wire = msg.into_bytes();
        wire.extend_from_slice(body);
        tracing::debug!(method, uri, cseq, bytes = wire.len(), "rtsp send");
        timeout(IO_TIMEOUT, self.stream.write_all(&wire))
            .await
            .map_err(|_| Error::Rtsp("write timed out".into()))?
            .map_err(Error::from)?;

        let resp = timeout(IO_TIMEOUT, read_response(&mut self.stream))
            .await
            .map_err(|_| Error::Rtsp("read timed out".into()))??;
        if !(200..300).contains(&resp.code) {
            return Err(Error::Rtsp(format!(
                "{method} {uri} failed: {} {}",
                resp.code, resp.reason
            )));
        }
        Ok(resp)
    }
}

async fn read_response(stream: &mut TcpStream) -> Result<Response> {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(Error::Rtsp("eof before headers".into()));
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(Error::Rtsp("headers too large".into()));
        }
    }
    let header_text = String::from_utf8_lossy(&buf);
    let mut lines = header_text.split("\r\n");
    let status = lines.next().unwrap_or("");
    let mut parts = status.splitn(3, ' ');
    let _proto = parts.next().unwrap_or("");
    let code: u16 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| Error::Rtsp(format!("bad status line: {status}")))?;
    let reason = parts.next().unwrap_or("").trim_end().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(": ") {
            headers.insert(k.to_ascii_lowercase(), v.to_string());
        } else if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut body).await?;
    }
    Ok(Response {
        code,
        reason,
        headers,
        body,
    })
}

pub fn parse_host_port(spec: &str, default_port: u16) -> Result<SocketAddr> {
    if let Ok(addr) = spec.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if spec.starts_with('[') {
        return spec
            .parse::<SocketAddr>()
            .map_err(|e| Error::Rtsp(format!("bad address {spec}: {e}")));
    }
    if let Some((host, port)) = spec.rsplit_once(':') {
        if host.parse::<std::net::Ipv4Addr>().is_ok() || port.chars().all(|c| c.is_ascii_digit()) {
            let p: u16 = port
                .parse()
                .map_err(|_| Error::Rtsp(format!("bad port in {spec}")))?;
            let ip: std::net::IpAddr = host
                .parse()
                .map_err(|_| Error::Rtsp(format!("bad host in {spec}")))?;
            return Ok(SocketAddr::new(ip, p));
        }
    }
    let ip: std::net::IpAddr = spec
        .parse()
        .map_err(|_| Error::Rtsp(format!("bad host {spec}")))?;
    Ok(SocketAddr::new(ip, default_port))
}
