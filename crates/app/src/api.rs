//! Local read-only API for the browser extension (A/V sync).
//!
//! One endpoint: `GET http://127.0.0.1:<port>/status` ->
//! `{"delay":bool,"lead_ms":u32}`.
//!
//! `delay` = streaming to HomePod AND an Edge/Chrome session is active on
//! the configured capture endpoint (see `audio_pipe::browser_active_on`).
//! The extension adds its own offset on top of `lead_ms`; fetch from the
//! extension's background context, never from page scripts.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

pub const DEFAULT_PORT: u16 = 17653;

pub struct ApiState {
    streaming: AtomicBool,
    lead_frames: AtomicU32,
    capture_hint: Mutex<String>,
}

impl ApiState {
    pub fn new(capture_hint: String, lead_frames: u32) -> Self {
        Self {
            streaming: AtomicBool::new(false),
            lead_frames: AtomicU32::new(lead_frames),
            capture_hint: Mutex::new(capture_hint),
        }
    }

    /// Mirror of `[STATUS] streaming`: true only while RTP is flowing.
    pub fn set_streaming(&self, on: bool) {
        self.streaming.store(on, Ordering::SeqCst);
    }

    pub fn set_lead_frames(&self, frames: u32) {
        self.lead_frames.store(frames, Ordering::SeqCst);
    }

    pub fn set_capture_hint(&self, hint: String) {
        *self.capture_hint.lock().unwrap() = hint;
    }

    fn status_json(&self) -> String {
        let streaming = self.streaming.load(Ordering::SeqCst);
        let frames = self.lead_frames.load(Ordering::SeqCst);
        let rate = airplay_stream::SAMPLE_RATE as u64;
        let lead_ms = (frames as u64 * 1000 + rate / 2) / rate;
        let delay = streaming && self.browser_on_sink();
        format!("{{\"delay\":{delay},\"lead_ms\":{lead_ms}}}")
    }

    /// Same device resolution as capture: hint (id or name substring),
    /// then Steam Speakers, then system default.
    #[cfg(windows)]
    fn browser_on_sink(&self) -> bool {
        let hint = self.capture_hint.lock().unwrap().clone();
        let hint = if hint.is_empty() {
            None
        } else {
            Some(hint.as_str())
        };
        match audio_pipe::pick_render_device_id(hint) {
            Ok(id) => audio_pipe::browser_active_on(&id).unwrap_or(false),
            Err(e) => {
                warn!("api: resolve capture device: {e}");
                false
            }
        }
    }

    #[cfg(not(windows))]
    fn browser_on_sink(&self) -> bool {
        false
    }
}

pub fn spawn(state: Arc<ApiState>, port: u16, handle: &tokio::runtime::Handle) {
    handle.spawn(serve(state, port));
}

async fn serve(state: Arc<ApiState>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    info!("api: http://127.0.0.1:{port}/status");
    loop {
        let (mut sock, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let Ok(n) = sock.read(&mut buf).await else {
                return;
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let line = req.lines().next().unwrap_or("");
            let (code, body) = if line.starts_with("GET /status") {
                ("200 OK", state.status_json())
            } else {
                ("404 Not Found", String::from("{}"))
            };
            let resp = format!(
                "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}
