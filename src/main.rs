use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use houdinny::config::{Config, Strategy, TunnelConfig};
use houdinny::pool::Pool;
use houdinny::proxy::ProxyServer;
use houdinny::router::Router;
use houdinny::transport::Transport;

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

/// Build transports from tunnel configurations.
///
/// Only protocols with the corresponding feature enabled are built.
/// Unknown or feature-gated protocols are logged as warnings and skipped.
fn build_transports(tunnels: &[TunnelConfig]) -> Vec<Arc<dyn Transport>> {
    let mut transports: Vec<Arc<dyn Transport>> = Vec::new();

    for tunnel in tunnels {
        match tunnel.protocol.as_str() {
            #[cfg(feature = "socks5")]
            "socks5" => {
                let address = match &tunnel.address {
                    Some(addr) => addr.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "socks5 tunnel missing address — skipping"
                        );
                        continue;
                    }
                };
                let label = tunnel
                    .label
                    .as_deref()
                    .unwrap_or("(unlabelled)")
                    .to_string();
                let transport = houdinny::transport::socks5::Socks5Transport::new(address, label);
                transports.push(Arc::new(transport));
            }
            #[cfg(not(feature = "socks5"))]
            "socks5" => {
                tracing::warn!(
                    label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                    "socks5 tunnel configured but 'socks5' feature is not enabled — skipping"
                );
            }
            #[cfg(feature = "wireguard")]
            "wireguard" => {
                let interface = match &tunnel.interface {
                    Some(iface) => iface.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "wireguard tunnel missing 'interface' field — skipping"
                        );
                        continue;
                    }
                };
                let label = tunnel
                    .label
                    .as_deref()
                    .unwrap_or("(unlabelled)")
                    .to_string();
                let transport =
                    houdinny::transport::wireguard::WireGuardTransport::new(interface, label);
                transports.push(Arc::new(transport));
            }
            #[cfg(not(feature = "wireguard"))]
            "wireguard" => {
                tracing::warn!(
                    label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                    "wireguard tunnel configured but 'wireguard' feature is not enabled — skipping"
                );
            }
            other => {
                tracing::warn!(
                    protocol = other,
                    label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                    "unsupported tunnel protocol — skipping (not yet implemented)"
                );
            }
        }
    }

    transports
}

#[tokio::main]
async fn main() -> Result<()> {
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

    // ── build transports ─────────────────────────────────────────────────
    let transports = build_transports(&config.tunnel);

    if transports.is_empty() {
        if config.tunnel.is_empty() {
            tracing::warn!("no tunnels configured — nothing to route through");
        } else {
            tracing::warn!(
                "no usable transports built from {} tunnel config(s) — \
                 check protocol support and feature flags",
                config.tunnel.len()
            );
        }
        anyhow::bail!(
            "cannot start proxy with no usable transports. \
             Add tunnels via -t/--tunnels or a config file."
        );
    }

    tracing::info!(count = transports.len(), "transports ready");

    // ── pool ─────────────────────────────────────────────────────────────
    let pool = Arc::new(Pool::new(transports));

    // ── router ───────────────────────────────────────────────────────────
    let router: Arc<Box<dyn Router>> = Arc::new(config.proxy.strategy.build_router());

    // ── picker closure ───────────────────────────────────────────────────
    let picker = {
        let pool = Arc::clone(&pool);
        let router = Arc::clone(&router);
        move |host: &str, port: u16| -> houdinny::error::Result<Arc<dyn Transport>> {
            let healthy = pool.healthy_transports_sync();
            router.pick(host, port, &healthy)
        }
    };

    // ── start proxy server ───────────────────────────────────────────────
    let addr: std::net::SocketAddr = config
        .proxy
        .listen
        .parse()
        .with_context(|| format!("invalid listen address: {}", config.proxy.listen))?;

    let server = ProxyServer::new(addr);
    println!("houdinny proxy listening on {}", addr);

    // Run the server with graceful Ctrl+C shutdown.
    tokio::select! {
        result = server.run(picker) => {
            result.with_context(|| "proxy server error")?;
        }
        _ = tokio::signal::ctrl_c() => {
            println!();
            tracing::info!("received Ctrl+C — shutting down");
        }
    }

    Ok(())
}
