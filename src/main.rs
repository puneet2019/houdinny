use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use houdinny::config::{Config, DockerConfig, Strategy, TunnelConfig, VpnConfig, load_dotenv};
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
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the TOML config file.
    #[arg(short, long, default_value = "houdinny.toml")]
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

#[derive(Subcommand, Debug)]
enum Commands {
    /// Import tunnel configurations from providers
    Import {
        #[command(subcommand)]
        provider: ImportProvider,
    },

    /// Start houdinny with Docker — reads config and runs docker compose
    #[command(name = "start-docker")]
    StartDocker {
        /// Config file path [default: houdinny.toml]
        #[arg(short, long, default_value = "houdinny.toml")]
        config: PathBuf,

        /// Override: NordVPN token (skip config)
        #[arg(long)]
        nord_token: Option<String>,

        /// Override: countries (skip config)
        #[arg(long, value_delimiter = ',')]
        countries: Option<Vec<String>>,

        /// Override: listen port
        #[arg(long)]
        port: Option<u16>,

        /// Routing strategy
        #[arg(long, default_value = "round-robin")]
        strategy: String,

        /// Don't wait for VPN health checks
        #[arg(long)]
        no_wait: bool,
    },

    /// Stop houdinny Docker stack
    #[command(name = "stop-docker")]
    StopDocker,
}

#[derive(Subcommand, Debug)]
enum ImportProvider {
    /// Import WireGuard tunnels from NordVPN
    Nord {
        /// NordVPN access token (from https://my.nordaccount.com/)
        #[arg(long)]
        token: String,
        /// Number of server configs to fetch
        #[arg(long, default_value = "3")]
        count: usize,
        /// Comma-separated country codes (e.g., us,de,jp)
        #[arg(long, value_delimiter = ',')]
        countries: Option<Vec<String>>,
        /// Output config file to write/append to
        #[arg(long, default_value = "tunnels.toml")]
        output: PathBuf,
    },
    /// Set up SSH SOCKS5 tunnels
    Ssh {
        /// SSH host (hostname or IP, e.g., tailscale hostname)
        #[arg(long)]
        host: String,
        /// SSH user
        #[arg(long, default_value = "root")]
        user: String,
        /// Local SOCKS5 ports to use (one SSH tunnel per port)
        #[arg(long, value_delimiter = ',', default_value = "1080")]
        ports: Vec<u16>,
        /// Output config file to write/append to
        #[arg(long, default_value = "tunnels.toml")]
        output: PathBuf,
        /// Actually start the SSH tunnels (ssh -D -N -f)
        #[arg(long)]
        start: bool,
    },
}

/// Build transports from tunnel configurations.
///
/// Only protocols with the corresponding feature enabled are built.
/// Unknown or feature-gated protocols are logged as warnings and skipped.
async fn build_transports(tunnels: &[TunnelConfig]) -> Vec<Arc<dyn Transport>> {
    #[allow(unused_mut)]
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
            "http-proxy" => {
                let address = match &tunnel.address {
                    Some(addr) => addr.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "http-proxy tunnel missing address — skipping"
                        );
                        continue;
                    }
                };
                let label = tunnel
                    .label
                    .as_deref()
                    .unwrap_or("(unlabelled)")
                    .to_string();
                // The address field may be a full URL (http://user:pass@host:port)
                // or a bare host:port. Try from_url first, fall back to direct.
                let transport = if address.starts_with("http://") || address.starts_with("https://")
                {
                    match houdinny::transport::http_proxy::HttpProxyTransport::from_url(
                        &address, &label,
                    ) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!(
                                label = label.as_str(),
                                error = %e,
                                "http-proxy tunnel URL parse failed — skipping"
                            );
                            continue;
                        }
                    }
                } else {
                    houdinny::transport::http_proxy::HttpProxyTransport::new(address, None, label)
                };
                transports.push(Arc::new(transport));
            }
            "sentinel" => {
                let node = match &tunnel.node {
                    Some(n) => n.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "sentinel tunnel missing 'node' field — skipping"
                        );
                        continue;
                    }
                };
                let denom = tunnel.denom.as_deref().unwrap_or("udvpn").to_string();
                let deposit = match &tunnel.deposit {
                    Some(d) => d.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "sentinel tunnel missing 'deposit' field — skipping"
                        );
                        continue;
                    }
                };
                let wallet_key = match &tunnel.wallet_key {
                    Some(k) => k.clone(),
                    None => {
                        tracing::warn!(
                            label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                            "sentinel tunnel missing 'wallet_key' field — skipping"
                        );
                        continue;
                    }
                };
                let label = tunnel
                    .label
                    .as_deref()
                    .unwrap_or("(unlabelled)")
                    .to_string();
                tracing::warn!(
                    label = label.as_str(),
                    "sentinel dVPN transport is a stub — provisioning will fail until Cosmos SDK integration is complete"
                );
                let transport = houdinny::transport::sentinel::SentinelTransport::new(
                    node, denom, deposit, wallet_key, label,
                );
                transports.push(Arc::new(transport));
            }
            #[cfg(feature = "tor")]
            "tor" => {
                let circuits = tunnel.circuits.unwrap_or(3) as usize;
                let label = tunnel
                    .label
                    .as_deref()
                    .unwrap_or("(unlabelled)")
                    .to_string();
                match houdinny::transport::tor::create_tor_pool(circuits, &label).await {
                    Ok(tor_transports) => {
                        tracing::info!(
                            label = label.as_str(),
                            circuits = tor_transports.len(),
                            "tor circuits created"
                        );
                        for t in tor_transports {
                            transports.push(Arc::new(t));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            label = label.as_str(),
                            error = %e,
                            "failed to create tor transport pool — skipping"
                        );
                    }
                }
            }
            #[cfg(not(feature = "tor"))]
            "tor" => {
                tracing::warn!(
                    label = tunnel.label.as_deref().unwrap_or("(unlabelled)"),
                    "tor tunnel configured but 'tor' feature is not enabled — skipping"
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

/// Load configuration for the `start` command, applying CLI overrides.
fn load_start_config(
    config_path: &std::path::Path,
    nord_token: &Option<String>,
    countries: &Option<Vec<String>>,
    port: &Option<u16>,
) -> Result<(Config, DockerConfig, u16)> {
    // Try to load config file. If it doesn't exist and the user supplied
    // --nord-token + --countries flags, build a synthetic config instead.
    let mut config = if config_path.exists() {
        Config::from_file(config_path)
            .with_context(|| format!("failed to load config from {}", config_path.display()))?
    } else if nord_token.is_some() && countries.is_some() {
        // Fully flag-driven, no config file needed.
        Config {
            proxy: houdinny::config::ProxyConfig::default(),
            docker: DockerConfig::default(),
            tunnel: Vec::new(),
        }
    } else {
        anyhow::bail!(
            "config file '{}' not found.\n\
             Either create it (see houdinny.toml.example) or pass --nord-token + --countries flags.",
            config_path.display()
        );
    };

    // Build the effective DockerConfig, applying CLI overrides.
    let mut docker_cfg = config.docker.clone();

    // If --countries is passed, rebuild the VPN list from scratch.
    if let Some(cli_countries) = countries {
        let token = nord_token
            .clone()
            .or_else(|| docker_cfg.vpn.first().and_then(|v| v.token.clone()));
        docker_cfg.enabled = true;
        docker_cfg.vpn = cli_countries
            .iter()
            .map(|c| VpnConfig {
                provider: "nordvpn".to_string(),
                token: token.clone(),
                country: c.clone(),
            })
            .collect();
    } else if let Some(tok) = nord_token {
        // Override token in all existing VPN entries.
        for vpn in &mut docker_cfg.vpn {
            vpn.token = Some(tok.clone());
        }
    }

    // Resolve effective port.
    let effective_port = port.unwrap_or_else(|| {
        config
            .proxy
            .listen
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(8080)
    });

    // Apply port override to config too.
    if port.is_some() {
        config.proxy.listen = format!("127.0.0.1:{effective_port}");
    }

    Ok((config, docker_cfg, effective_port))
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

    // ── load .env ────────────────────────────────────────────────────────
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    load_dotenv(&cwd.join(".env"));

    // ── dispatch subcommands ─────────────────────────────────────────────
    match cli.command {
        Some(Commands::Import { provider }) => {
            match provider {
                ImportProvider::Nord {
                    token,
                    count,
                    countries,
                    output,
                } => {
                    houdinny::import::nord::import_nord(
                        &token,
                        count,
                        countries.as_deref(),
                        &output,
                    )
                    .await?;
                }
                ImportProvider::Ssh {
                    host,
                    user,
                    ports,
                    output,
                    start,
                } => {
                    houdinny::import::ssh::import_ssh(&host, &user, &ports, &output, start).await?;
                }
            }
            return Ok(());
        }
        Some(Commands::StartDocker {
            config: config_path,
            nord_token,
            countries,
            port,
            strategy,
            no_wait,
        }) => {
            let (_config, docker_cfg, effective_port) =
                load_start_config(&config_path, &nord_token, &countries, &port)?;

            houdinny::docker::start(
                &docker_cfg,
                nord_token.as_deref(),
                effective_port,
                &strategy,
                no_wait,
            )
            .await?;
            return Ok(());
        }
        Some(Commands::StopDocker) => {
            houdinny::docker::stop().await?;
            return Ok(());
        }
        None => {
            // Fall through to existing proxy run logic.
        }
    }

    // ── config ───────────────────────────────────────────────────────────
    let mut config = if let Some(ref urls) = cli.tunnels {
        tracing::debug!(urls = ?urls, "building config from CLI tunnel URLs");
        Config::from_tunnel_urls(urls)
    } else {
        let config_path = &cli.config;
        // Fall back to tunnels.toml if houdinny.toml doesn't exist.
        let effective_path = if !config_path.exists() {
            let fallback = PathBuf::from("tunnels.toml");
            if fallback.exists() {
                tracing::info!("houdinny.toml not found, falling back to tunnels.toml");
                fallback
            } else {
                config_path.clone()
            }
        } else {
            config_path.clone()
        };
        tracing::debug!(path = %effective_path.display(), "loading config file");
        Config::from_file(&effective_path)
            .with_context(|| format!("failed to load config from {}", effective_path.display()))?
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
    let transports = build_transports(&config.tunnel).await;

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
