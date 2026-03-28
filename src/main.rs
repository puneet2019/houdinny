use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use houdinny::config::{Config, Strategy};

/// houdinny — privacy proxy for AI agents.
///
/// Rotates requests across tunnel pools so no single observer sees the full
/// picture. Any agent in any language just sets `HTTP_PROXY` and goes.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "tunnels.toml")]
    config: PathBuf,

    /// Listen address (overrides the value in the config file).
    #[arg(short, long)]
    listen: Option<String>,

    /// Comma-separated tunnel URLs (e.g. socks5://127.0.0.1:1080,socks5://127.0.0.1:1081).
    ///
    /// When provided, the config file is ignored and tunnels are built from
    /// these URLs instead.
    #[arg(short, long, value_delimiter = ',')]
    tunnels: Option<Vec<String>>,

    /// Routing strategy: random, round-robin.
    #[arg(short, long, default_value = "random")]
    strategy: String,

    /// Enable debug logging (RUST_LOG=debug).
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // ── logging ──────────────────────────────────────────────────────────
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // ── config ───────────────────────────────────────────────────────────
    let mut config = if let Some(ref urls) = cli.tunnels {
        tracing::debug!(urls = ?urls, "building config from CLI tunnel URLs");
        Config::from_tunnel_urls(urls)
    } else {
        tracing::debug!(path = %cli.config.display(), "loading config file");
        Config::from_file(&cli.config)
            .with_context(|| format!("failed to load config from {}", cli.config.display()))?
    };

    // CLI overrides
    if let Some(ref addr) = cli.listen {
        config.proxy.listen = addr.clone();
    }

    let strategy = Strategy::from_name(&cli.strategy)
        .with_context(|| format!("invalid strategy: {}", cli.strategy))?;
    config.proxy.strategy = strategy;

    // ── startup banner ───────────────────────────────────────────────────
    println!("houdinny v{}", env!("CARGO_PKG_VERSION"));
    println!("  listen:   {}", config.proxy.listen);
    println!("  tunnels:  {}", config.tunnel.len());
    println!("  strategy: {}", config.proxy.strategy);
    println!("  mode:     {}", config.proxy.mode);

    if config.tunnel.is_empty() {
        tracing::warn!("no tunnels configured — proxy will have nothing to route through");
    }

    for (i, t) in config.tunnel.iter().enumerate() {
        let label = t.label.as_deref().unwrap_or("(unlabelled)");
        let addr = t.address.as_deref().unwrap_or("-");
        tracing::info!(
            index = i,
            protocol = t.protocol,
            label,
            address = addr,
            "tunnel registered"
        );
    }

    // Proxy server wiring will be added in a later phase.
    tracing::info!("config loaded successfully — proxy server not yet wired");

    Ok(())
}
