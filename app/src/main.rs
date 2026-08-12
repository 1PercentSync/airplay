//! airplay CLI — milestone 1: probe commands.
//!
//!   airplay probe devices                 Enumerate WASAPI render endpoints
//!   airplay probe airplay <ip> [--port N] Plaintext GET /info
//!   airplay probe pair    <ip> [--port N] Transient pair-setup drill (M1-M4)

use std::net::{IpAddr, SocketAddr};

use airplay_rtsp::client::PlainClient;
use airplay_rtsp::info::Info;
use airplay_rtsp::pairing;
use audio_pipe::SourceKind;

mod probe_devices;
mod run;

const DEFAULT_PORT: u16 = 7000;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbosity = args.iter().filter(|a| a.as_str() == "-v").count();
    let args: Vec<String> = args.into_iter().filter(|a| a != "-v").collect();
    init_tracing(verbosity.min(3) as u8);

    let code = match args.first().map(String::as_str) {
        Some("probe") => probe(&args[1..]).await,
        Some("run") => run_cmd(&args[1..]).await,
        _ => {
            usage();
            2
        }
    };
    std::process::exit(code);
}

fn usage() {
    eprintln!(
        "Usage:\n  \
         airplay probe devices\n  \
         airplay probe airplay <ip> [--port N] [-v]\n  \
         airplay probe pair <ip> [--port N] [-v]\n  \
         airplay run <ip> [--port N] [--source sine|wasapi] [--device NAME] [--volume 0-100]"
    );
}

async fn run_cmd(args: &[String]) -> i32 {
    let addr = match parse_target(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[STATUS] error: {e}");
            usage();
            return 2;
        }
    };
    let mut source = SourceKind::Wasapi { device: None };
    let mut volume = 50.0f32;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                match args.get(i + 1).map(String::as_str) {
                    Some("sine") => {
                        source = SourceKind::Sine {
                            freq_hz: 440.0,
                            rate: 48000,
                        }
                    }
                    Some("wasapi") => {
                        source = SourceKind::Wasapi { device: None }
                    }
                    _ => {
                        eprintln!("[STATUS] error: --source sine|wasapi");
                        return 2;
                    }
                }
                i += 1;
            }
            "--device" => {
                match args.get(i + 1) {
                    Some(name) => {
                        source = SourceKind::Wasapi {
                            device: Some(name.clone()),
                        }
                    }
                    None => {
                        eprintln!("[STATUS] error: --device NAME");
                        return 2;
                    }
                }
                i += 1;
            }
            "--volume" => {
                match args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) {
                    Some(v) => volume = v,
                    None => {
                        eprintln!("[STATUS] error: --volume 0-100");
                        return 2;
                    }
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    run::run(run::RunArgs {
        addr,
        source,
        volume_pct: volume,
    })
    .await
}

async fn probe(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("devices") => probe_devices::run(),
        Some("airplay") => probe_airplay(&args[1..], false).await,
        Some("pair") => probe_airplay(&args[1..], true).await,
        _ => {
            usage();
            2
        }
    }
}

fn parse_target(args: &[String]) -> Result<SocketAddr, String> {
    let ip: IpAddr = args
        .first()
        .ok_or("missing <ip>")?
        .parse()
        .map_err(|_| "invalid ip")?;
    let mut port = DEFAULT_PORT;
    if let Some(i) = args.iter().position(|a| a == "--port") {
        port = args
            .get(i + 1)
            .and_then(|p| p.parse().ok())
            .ok_or("invalid --port")?;
    }
    Ok(SocketAddr::new(ip, port))
}

async fn probe_airplay(args: &[String], pair: bool) -> i32 {
    let addr = match parse_target(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[STATUS] error: {e}");
            usage();
            return 2;
        }
    };
    println!("[STATUS] connecting {addr}");
    let mut client = match PlainClient::connect(addr).await {
        Ok(c) => {
            println!("[STATUS] connected");
            c
        }
        Err(e) => {
            println!("[STATUS] connect_failed: {e}");
            return 1;
        }
    };

    if pair {
        return probe_pair(&mut client).await;
    }

    println!("[STATUS] probing GET /info");
    let resp = match client.request("GET", "/info", &[], &[]).await {
        Ok(r) => r,
        Err(e) => {
            println!("[STATUS] info_failed: {e}");
            return 1;
        }
    };
    println!("[STATUS] /info http_status={} body={} bytes", resp.status, resp.body.len());
    if resp.status != 200 {
        println!("[STATUS] info_rejected (non-200)");
        return 1;
    }
    match Info::parse(&resp.body) {
        Ok(info) => {
            println!("--- capability summary ---");
            for line in info.capability_summary() {
                println!("{line}");
            }
            println!("--- full dump ---\n{}", info.dump());
            println!("[STATUS] info_ok");
            0
        }
        Err(e) => {
            println!("[STATUS] info_parse_failed: {e} (body not a bplist?)");
            1
        }
    }
}

async fn probe_pair(client: &mut PlainClient) -> i32 {
    println!("[STATUS] pairing: transient (HKP 4) M1-M4 drill");
    match pairing::transient_pair(client).await {
        Ok(outcome) => {
            for line in &outcome.transcript {
                println!("  {line}");
            }
            println!(
                "[STATUS] pair_ok session_key_fingerprint={}",
                outcome.key_fingerprint()
            );
            0
        }
        Err(e) => {
            println!("[STATUS] pair_failed: {e}");
            1
        }
    }
}

fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
