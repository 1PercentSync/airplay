//! Encrypted event-channel TCP: decrypt incoming RTSP, reply 200 with CSeq echo.
//!
//! [evidence: pyatv channels.py:60-95; owntone pair_homekit.c:2900-2910;
//! raop_sender.cpp:1571-1580]

use airplay_core::{Error, Result};
use airplay_crypto::hap::{HapCipher, FRAME_MAX};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tracing::{debug, info, warn};

const RETRIES: u32 = 5;

pub async fn connect_events(
    host: SocketAddr,
    event_port: u16,
    ikm: &[u8; 64],
) -> Result<EventConn> {
    let addr = SocketAddr::new(host.ip(), event_port);
    let mut last = Error::Rtsp("event connect not attempted".into());
    for i in 0..RETRIES {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                info!(%addr, attempt = i + 1, "event channel connected");
                return Ok(EventConn {
                    stream,
                    hap: HapCipher::events(ikm)?,
                    plain: Vec::new(),
                });
            }
            Err(e) => {
                last = Error::from(e);
                warn!(%addr, attempt = i + 1, "event connect failed, retry in 1s");
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
    Err(last)
}

pub struct EventConn {
    stream: TcpStream,
    hap: HapCipher,
    plain: Vec<u8>,
}

impl EventConn {
    pub async fn serve(mut self) -> Result<()> {
        loop {
            let mut hdr = [0u8; 2];
            match self.stream.read_exact(&mut hdr).await {
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
            let n = u16::from_le_bytes(hdr) as usize;
            if n > FRAME_MAX {
                return Err(Error::Rtsp(format!("event HAP frame {n} > {FRAME_MAX}")));
            }
            let mut rest = vec![0u8; n + 16];
            self.stream.read_exact(&mut rest).await?;
            let chunk = self.hap.decrypt_frame(hdr, &rest)?;
            self.plain.extend_from_slice(&chunk);
            while let Some((cseq, proto, used)) = try_parse_request(&self.plain) {
                self.plain.drain(..used);
                debug!(?cseq, proto, "event request");
                let mut resp = format!("{proto} 200 OK\r\n");
                if let Some(c) = cseq {
                    resp.push_str(&format!("CSeq: {c}\r\n"));
                }
                resp.push_str("Server: AirPlay/550.10\r\n");
                resp.push_str("Content-Length: 0\r\n\r\n");
                let framed = self.hap.encrypt_message(resp.as_bytes())?;
                self.stream.write_all(&framed).await?;
            }
            if self.plain.len() > 64 * 1024 {
                return Err(Error::Rtsp("event plaintext too large".into()));
            }
        }
    }
}

fn try_parse_request(buf: &[u8]) -> Option<(Option<String>, &'static str, usize)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let header_end = end + 4;
    let text = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let proto = if first.contains("HTTP/1.1") {
        "HTTP/1.1"
    } else {
        "RTSP/1.0"
    };
    let mut content_len = 0usize;
    let mut cseq = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':').unwrap_or((line, ""));
        let k = k.trim();
        let v = v.trim();
        if k.eq_ignore_ascii_case("content-length") {
            content_len = v.parse().unwrap_or(0);
        }
        if k.eq_ignore_ascii_case("cseq") {
            cseq = Some(v.to_string());
        }
    }
    if buf.len() < header_end + content_len {
        return None;
    }
    Some((cseq, proto, header_end + content_len))
}
