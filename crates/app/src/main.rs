//! CLI: probe commands, playable `run`, and (Windows) tray GUI with no args.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod probe;
mod run;
mod sunshine;
#[cfg(windows)]
mod tray;

use anyhow::{bail, Context, Result};
use std::fs::OpenOptions;
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();

    #[cfg(windows)]
    if !cmd.is_empty() {
        attach_console();
    }

    init_tracing()?;

    if cmd.is_empty() {
        #[cfg(windows)]
        {
            return tray::run();
        }
        #[cfg(not(windows))]
        {
            bail!("tray GUI is Windows-only; use: airplay probe … | airplay run <ip>[:port]");
        }
    }

    match cmd.as_str() {
        "probe" => {
            let sub = args.next().unwrap_or_default();
            match sub.as_str() {
                "devices" => probe::devices(),
                "discover" => {
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(probe::discover())
                }
                "airplay" => {
                    let target = args
                        .next()
                        .context("usage: airplay probe airplay <ip>[:port]")?;
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(probe::airplay(&target))
                }
                "pair" => {
                    let target = args
                        .next()
                        .context("usage: airplay probe pair <ip>[:port]")?;
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(probe::pair(&target))
                }
                "channel" => {
                    let target = args
                        .next()
                        .context("usage: airplay probe channel <ip>[:port]")?;
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(probe::channel(&target))
                }
                _ => {
                    bail!(
                        "usage: airplay probe devices|discover|airplay <ip>[:port]|pair <ip>[:port]|channel <ip>[:port]"
                    )
                }
            }
        }
        "run" => {
            let target = args
                .next()
                .context("usage: airplay run <ip>[:port] [capture-device]")?;
            let device = args.next();
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(run::run(&target, device.as_deref()))
        }
        _ => bail!("usage: airplay | airplay probe … | airplay run <ip>[:port] [capture-device]"),
    }
}

fn init_tracing() -> Result<()> {
    let log_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("airplay.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("airplay.log"));
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("open log {}", log_path.display()))?;
    let writer = std::io::stdout.and(file);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Ok(())
}

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}
