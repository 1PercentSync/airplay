//! CLI: probe commands and playable `run`.

mod probe;
mod run;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stdout)
        .init();

    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
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
        _ => bail!("usage: airplay probe … | airplay run <ip>[:port] [capture-device]"),
    }
}
