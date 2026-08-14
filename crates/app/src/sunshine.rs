//! GameStream `/serverinfo` on the Sunshine HTTP port (default 47989).
//!
//! Busy means Moonlight has not quit the launched app (`currentgame` / `state`),
//! not that a video session is in flight.
//! [evidence: Sunshine src/nvhttp.cpp serverinfo / proc_t::running]

#![cfg_attr(not(windows), allow(dead_code))]

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const HOST: &str = "127.0.0.1:47989";
const REQ: &[u8] = b"GET /serverinfo HTTP/1.1\r\nHost: 127.0.0.1:47989\r\nConnection: close\r\n\r\n";

pub async fn app_connected() -> bool {
    match query_body().await {
        Ok(body) => parse_busy(&body),
        Err(_) => false,
    }
}

async fn query_body() -> std::io::Result<String> {
    let fut = async {
        let mut stream = TcpStream::connect(HOST).await?;
        let _ = stream.set_nodelay(true);
        stream.write_all(REQ).await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    };
    tokio::time::timeout(Duration::from_millis(800), fut)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "serverinfo"))?
}

pub fn parse_busy(http: &str) -> bool {
    let body = http.rsplit("\r\n\r\n").next().unwrap_or(http);
    if body.contains("SUNSHINE_SERVER_BUSY") {
        return true;
    }
    xml_tag(body, "currentgame")
        .and_then(|s| s.parse::<i64>().ok())
        .is_some_and(|n| n > 0)
}

fn xml_tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::parse_busy;

    const FREE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 540\r\n\r\n\
<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<root status_code=\"200\"><hostname>1PercentSync</hostname><appversion>7.1.431.-1</appversion>\
<GfeVersion>3.23.0.74</GfeVersion><uniqueid>40E186A5-0262-3B01-1B91-2A551A5E6289</uniqueid>\
<HttpsPort>47984</HttpsPort><ExternalPort>47989</ExternalPort>\
<MaxLumaPixelsHEVC>1869449984</MaxLumaPixelsHEVC><mac>00:00:00:00:00:00</mac>\
<LocalIP>127.0.0.1</LocalIP><ServerCodecModeSupport>852225</ServerCodecModeSupport>\
<PairStatus>0</PairStatus><currentgame>0</currentgame><state>SUNSHINE_SERVER_FREE</state></root>";

    const BUSY: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<root><currentgame>12345</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>";

    #[test]
    fn free_xml_is_not_connected() {
        assert!(!parse_busy(FREE));
    }

    #[test]
    fn busy_xml_is_connected() {
        assert!(parse_busy(BUSY));
    }

    #[test]
    fn currentgame_alone_is_connected() {
        assert!(parse_busy("<root><currentgame>9</currentgame><state>SUNSHINE_SERVER_FREE</state></root>"));
    }

    #[test]
    fn empty_or_garbage_is_not_connected() {
        assert!(!parse_busy(""));
        assert!(!parse_busy("connection refused"));
    }
}
