//! Probe commands: devices, plaintext GET /info, transient pair-setup.

use airplay_core::AIRPLAY_CONTROL_PORT;
use airplay_rtsp::{pair_transient, plist, RtspClient};
use anyhow::{bail, Context, Result};
use tracing::info;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("devices") => probe_devices(),
        Some("airplay") => {
            let target = args.get(1).context("probe airplay needs <ip[:port]>")?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(probe_airplay(target))
        }
        Some("pair") => {
            let target = args.get(1).context("probe pair needs <ip[:port]>")?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(probe_pair(target))
        }
        _ => {
            bail!("unknown probe subcommand; see usage");
        }
    }
}

fn probe_devices() -> Result<()> {
    println!("[STATUS] probing");
    match audio_pipe::enumerate_render_endpoints() {
        Ok(list) => {
            println!("====== {} render endpoints ======", list.len());
            for d in &list {
                println!("===== Device =====");
                println!("Device ID           : {}", d.id);
                println!("Device name         : {}", d.friendly_name);
                println!("Adapter name        : {}", d.adapter_name);
                println!("Device description  : {}", d.description);
                println!("Mix format          : {}", d.mix_format);
                println!();
            }
            println!("[STATUS] devices_ok count={}", list.len());
            Ok(())
        }
        Err(e) => {
            println!("[STATUS] dead({e})");
            Err(e.into())
        }
    }
}

async fn probe_airplay(target: &str) -> Result<()> {
    let (host, port) = parse_host_port(target)?;
    println!("[STATUS] probing");
    info!("GET /info {host}:{port}");
    let mut rtsp = RtspClient::connect(&host, port).await?;
    let resp = rtsp.exchange("GET", "/info", &[], &[]).await?;
    if !resp.is_success() {
        println!("[STATUS] dead(info status {} {})", resp.status, resp.reason);
        bail!("GET /info returned {} {}", resp.status, resp.reason);
    }
    let ctype = resp.header("Content-Type").unwrap_or("");
    println!(
        "GET /info -> {} {}  content-type={ctype}  body={}B",
        resp.status,
        resp.reason,
        resp.body.len()
    );
    if resp.body.is_empty() {
        println!("[STATUS] dead(empty /info body)");
        bail!("empty /info body");
    }
    let value = plist::decode(&resp.body)?;
    println!("--- /info plist ---");
    println!("{value}");
    println!("--- highlights ---");
    for (k, v) in plist::info_highlights(&value) {
        println!("{k}: {v}");
    }
    println!("[STATUS] info_ok");
    Ok(())
}

async fn probe_pair(target: &str) -> Result<()> {
    let (host, port) = parse_host_port(target)?;
    println!("[STATUS] probing");
    info!("transient pair {host}:{port}");
    let mut rtsp = RtspClient::connect(&host, port).await?;
    let a = random_bytes(32)?;
    match pair_transient(&mut rtsp, &a).await {
        Ok(_) => {
            println!("[STATUS] pair_ok");
            Ok(())
        }
        Err(e) => {
            println!("[STATUS] dead({e})");
            Err(e.into())
        }
    }
}

fn parse_host_port(s: &str) -> Result<(String, u16)> {
    if let Some((h, p)) = s.rsplit_once(':') {
        if h.starts_with('[') {
            let host = h.trim_matches(|c| c == '[' || c == ']').to_string();
            let port: u16 = p.parse().context("port")?;
            return Ok((host, port));
        }
        if !h.contains(':') {
            let port: u16 = p.parse().context("port")?;
            return Ok((h.to_string(), port));
        }
    }
    Ok((s.to_string(), AIRPLAY_CONTROL_PORT))
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    fill_random(&mut buf)?;
    Ok(buf)
}

fn fill_random(buf: &mut [u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(buf)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status.0 < 0 {
            anyhow::bail!("BCryptGenRandom failed: {status:?}");
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!("no OS CSPRNG")
    }
}
