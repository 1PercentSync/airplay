//! CLI: probe devices / airplay / pair.

mod probe;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    init_logging();
    match args.get(1).map(String::as_str) {
        Some("probe") => probe::run(&args[2..]),
        _ => {
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  \
         airplay probe devices\n  \
         airplay probe airplay <ip[:port]>\n  \
         airplay probe pair <ip[:port]>\n"
    );
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();
}
