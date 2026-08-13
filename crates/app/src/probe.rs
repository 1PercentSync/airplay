use airplay_core::{AIRPLAY_PORT, MDNS_BROWSE_SECS};
use airplay_rtsp::{
    parse_host_port, plist_decode, pretty_print_value, Identity, RtspClient,
};
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tracing::info;

pub fn devices() -> Result<()> {
    let list = audio_pipe::list_render_devices()?;
    for (i, d) in list.iter().enumerate() {
        println!("===== Device {i} =====");
        println!("Name        : {}", d.friendly_name);
        println!("Device ID   : {}", d.id);
        println!(
            "Mix format  : {} Hz, {} ch, {} bit (valid_bits={}), subtype={}",
            d.mix_rate, d.mix_channels, d.mix_bits, d.mix_valid_bits, d.subtype
        );
        println!();
    }
    println!("[STATUS] devices_ok count={}", list.len());
    Ok(())
}

pub async fn discover() -> Result<()> {
    let mdns = ServiceDaemon::new().map_err(|e| anyhow::anyhow!("mdns daemon: {e}"))?;
    let rx = mdns
        .browse("_airplay._tcp.local.")
        .map_err(|e| anyhow::anyhow!("mdns browse: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(MDNS_BROWSE_SECS);
    let mut found: HashMap<String, Discovered> = HashMap::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, rx.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(svc))) => {
                if !svc.is_valid() {
                    continue;
                }
                let mut addrs: Vec<IpAddr> = svc
                    .addresses
                    .iter()
                    .filter_map(scoped_ip)
                    .collect();
                addrs.sort_by_key(|a| match a {
                    IpAddr::V4(_) => 0u8,
                    IpAddr::V6(_) => 1,
                });
                addrs.dedup();
                let txt = &svc.txt_properties;
                found.insert(
                    svc.fullname.clone(),
                    Discovered {
                        fullname: svc.fullname.clone(),
                        host: svc.host.clone(),
                        port: svc.port,
                        addrs,
                        model: txt.get_property_val_str("model").unwrap_or("").to_string(),
                        deviceid: txt.get_property_val_str("deviceid").unwrap_or("").to_string(),
                        features: txt.get_property_val_str("features").unwrap_or("").to_string(),
                        srcvers: txt.get_property_val_str("srcvers").unwrap_or("").to_string(),
                        protovers: txt.get_property_val_str("protovers").unwrap_or("").to_string(),
                        osvers: txt.get_property_val_str("osvers").unwrap_or("").to_string(),
                    },
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                info!("mdns recv ended: {e}");
                break;
            }
            Err(_) => break,
        }
    }
    let _ = mdns.shutdown();
    let mut list: Vec<_> = found.into_values().collect();
    list.sort_by(|a, b| a.fullname.cmp(&b.fullname));
    for d in &list {
        let use_addr = d
            .addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| d.addrs.first());
        let name = d
            .fullname
            .split("._airplay")
            .next()
            .unwrap_or(&d.fullname);
        println!("Name       : {name}");
        println!("Host       : {}", d.host);
        println!("Port       : {}", d.port);
        print!("Addresses  :");
        for a in &d.addrs {
            print!(" {a}");
        }
        println!();
        if let Some(ip) = use_addr {
            println!("Use        : {ip}:{}", d.port);
        }
        println!(
            "TXT        : model={} deviceid={} features={} srcvers={} protovers={} osvers={}",
            d.model, d.deviceid, d.features, d.srcvers, d.protovers, d.osvers
        );
        println!();
    }
    println!("[STATUS] discover_ok count={}", list.len());
    Ok(())
}

struct Discovered {
    fullname: String,
    host: String,
    port: u16,
    addrs: Vec<IpAddr>,
    model: String,
    deviceid: String,
    features: String,
    srcvers: String,
    protovers: String,
    osvers: String,
}

fn scoped_ip(s: &mdns_sd::ScopedIp) -> Option<IpAddr> {
    let t = s.to_string();
    let t = t.split('%').next().unwrap_or(&t);
    t.parse().ok()
}

pub async fn airplay(target: &str) -> Result<()> {
    let addr = parse_host_port(target, AIRPLAY_PORT).map_err(|e| anyhow::anyhow!("{e}"))?;
    let identity = Identity::generate().map_err(|e| anyhow::anyhow!("{e}"))?;
    info!("GET /info {addr}");
    let mut rtsp = RtspClient::connect(addr, identity)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let resp = rtsp
        .request("GET", "/info", &[], None, &[])
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "RTSP {} {}, body {} bytes",
        resp.code,
        resp.reason,
        resp.body.len()
    );
    let value = plist_decode(&resp.body).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut pretty = String::new();
    pretty_print_value(&value, 0, &mut pretty);
    println!("{pretty}");
    println!("[STATUS] info_ok");
    Ok(())
}

pub async fn pair(target: &str) -> Result<()> {
    let addr = parse_host_port(target, AIRPLAY_PORT).map_err(|e| anyhow::anyhow!("{e}"))?;
    let identity = Identity::generate().map_err(|e| anyhow::anyhow!("{e}"))?;
    info!("transient pair-setup {addr}");
    let key = airplay_rtsp::transient_pair(addr, identity)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("session_key_len={}", key.len());
    println!("[STATUS] pair_ok");
    Ok(())
}
