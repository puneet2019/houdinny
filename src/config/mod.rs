//! Configuration loading and validation for houdinny.
//!
//! Supports loading from TOML files (`tunnels.toml`) or from CLI tunnel URLs
//! (`socks5://127.0.0.1:1080`). All structs derive [`serde::Deserialize`] so
//! the TOML crate handles the heavy lifting.
//!
//! # Examples
//!
//! ```rust
//! use houdinny::config::Config;
//! use std::path::Path;
//!
//! // From a TOML string
//! let cfg = Config::parse_toml("").unwrap();
//! assert_eq!(cfg.proxy.listen, "127.0.0.1:8080");
//!
//! // From CLI tunnel URLs
//! let urls = vec!["socks5://127.0.0.1:1080".to_string()];
//! let cfg = Config::from_tunnel_urls(&urls);
//! assert_eq!(cfg.tunnel.len(), 1);
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

use crate::error::{Error, Result};

/// Top-level configuration, typically loaded from `tunnels.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Proxy server settings (listen address, mode, strategy).
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// List of tunnel definitions.
    #[serde(default)]
    pub tunnel: Vec<TunnelConfig>,
}

/// Proxy server configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    /// Socket address to listen on (e.g. `"127.0.0.1:8080"`).
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Operating mode for the proxy.
    #[serde(default)]
    pub mode: ProxyMode,

    /// Tunnel selection strategy.
    #[serde(default)]
    pub strategy: Strategy,
}

/// Proxy operating mode.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyMode {
    /// Forward traffic without inspecting TLS (default).
    #[default]
    Transparent,
    /// Man-in-the-middle: terminate TLS to inspect / modify traffic.
    Mitm,
}

/// Tunnel selection strategy used by the router.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Pick a random tunnel for each request.
    #[default]
    Random,
    /// Cycle through tunnels in order.
    RoundRobin,
}

/// Configuration for a single tunnel.
///
/// Only `protocol` is required; other fields depend on which protocol is used.
#[derive(Debug, Clone, Deserialize)]
pub struct TunnelConfig {
    /// Tunnel protocol (`"socks5"`, `"wireguard"`, `"tor"`, `"http-proxy"`, `"sentinel"`).
    pub protocol: String,

    /// Human-readable label for logging.
    pub label: Option<String>,

    /// Address or endpoint (meaning depends on protocol).
    pub address: Option<String>,

    /// Remote endpoint (WireGuard).
    pub endpoint: Option<String>,

    /// WireGuard private key.
    pub private_key: Option<String>,

    /// WireGuard server public key.
    pub public_key: Option<String>,

    /// DNS server to use inside the tunnel.
    pub dns: Option<String>,

    /// Number of Tor circuits to maintain.
    pub circuits: Option<u32>,

    /// How often to rotate circuits (e.g. `"10m"`).
    pub rotate_interval: Option<String>,

    /// Sentinel node address.
    pub node: Option<String>,

    /// Sentinel payment denomination.
    pub denom: Option<String>,

    /// Sentinel deposit amount.
    pub deposit: Option<String>,

    /// Path to wallet key file (Sentinel).
    pub wallet_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Default listen address for the proxy.
fn default_listen() -> String {
    "127.0.0.1:8080".to_string()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            mode: ProxyMode::default(),
            strategy: Strategy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Display impls
// ---------------------------------------------------------------------------

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Random => write!(f, "random"),
            Self::RoundRobin => write!(f, "round-robin"),
        }
    }
}

impl fmt::Display for ProxyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transparent => write!(f, "transparent"),
            Self::Mitm => write!(f, "mitm"),
        }
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl std::str::FromStr for Config {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Config(format!("invalid TOML: {e}")))
    }
}

impl Config {
    /// Load configuration from a TOML file on disk.
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
        contents.parse()
    }

    /// Parse configuration from a TOML string.
    pub fn parse_toml(s: &str) -> Result<Self> {
        s.parse()
    }

    /// Build a [`Config`] from CLI tunnel URLs.
    ///
    /// Accepts URLs of the form `socks5://host:port`. Unknown schemes are
    /// stored with the scheme as the protocol and the rest as the address.
    pub fn from_tunnel_urls(urls: &[String]) -> Self {
        let tunnels: Vec<TunnelConfig> = urls
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let (protocol, address) = parse_tunnel_url(url);
                TunnelConfig {
                    protocol,
                    label: Some(format!("cli-{i}")),
                    address: Some(address),
                    endpoint: None,
                    private_key: None,
                    public_key: None,
                    dns: None,
                    circuits: None,
                    rotate_interval: None,
                    node: None,
                    denom: None,
                    deposit: None,
                    wallet_key: None,
                }
            })
            .collect();

        Self {
            proxy: ProxyConfig::default(),
            tunnel: tunnels,
        }
    }
}

/// Parse a tunnel URL like `socks5://127.0.0.1:1080` into `("socks5", "127.0.0.1:1080")`.
fn parse_tunnel_url(url: &str) -> (String, String) {
    match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_string(), rest.to_string()),
        None => ("socks5".to_string(), url.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Strategy parsing from CLI strings
// ---------------------------------------------------------------------------

impl Strategy {
    /// Parse a strategy name from a CLI flag value.
    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_lowercase().as_str() {
            "random" => Ok(Self::Random),
            "round-robin" | "roundrobin" | "rr" => Ok(Self::RoundRobin),
            other => Err(Error::Config(format!("unknown strategy: {other}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_toml() {
        let toml_str = include_str!("../../tunnels.example.toml");
        let cfg = Config::parse_toml(toml_str).expect("example toml should parse");
        assert_eq!(cfg.proxy.listen, "127.0.0.1:8080");
        assert_eq!(cfg.proxy.mode, ProxyMode::Transparent);
        assert_eq!(cfg.proxy.strategy, Strategy::Random);
        assert_eq!(cfg.tunnel.len(), 1);
        assert_eq!(cfg.tunnel[0].protocol, "tor");
        assert_eq!(cfg.tunnel[0].circuits, Some(5));
    }

    #[test]
    fn parse_empty_config_uses_defaults() {
        let cfg = Config::parse_toml("").expect("empty string should parse");
        assert_eq!(cfg.proxy.listen, "127.0.0.1:8080");
        assert_eq!(cfg.proxy.mode, ProxyMode::Transparent);
        assert_eq!(cfg.proxy.strategy, Strategy::Random);
        assert!(cfg.tunnel.is_empty());
    }

    #[test]
    fn parse_tunnel_urls() {
        let urls = vec![
            "socks5://127.0.0.1:1080".to_string(),
            "socks5://127.0.0.1:1081".to_string(),
        ];
        let cfg = Config::from_tunnel_urls(&urls);
        assert_eq!(cfg.tunnel.len(), 2);
        assert_eq!(cfg.tunnel[0].protocol, "socks5");
        assert_eq!(cfg.tunnel[0].address.as_deref(), Some("127.0.0.1:1080"));
        assert_eq!(cfg.tunnel[0].label.as_deref(), Some("cli-0"));
        assert_eq!(cfg.tunnel[1].address.as_deref(), Some("127.0.0.1:1081"));
    }

    #[test]
    fn parse_tunnel_url_without_scheme_defaults_to_socks5() {
        let urls = vec!["127.0.0.1:9050".to_string()];
        let cfg = Config::from_tunnel_urls(&urls);
        assert_eq!(cfg.tunnel[0].protocol, "socks5");
        assert_eq!(cfg.tunnel[0].address.as_deref(), Some("127.0.0.1:9050"));
    }

    #[test]
    fn default_proxy_config() {
        let pc = ProxyConfig::default();
        assert_eq!(pc.listen, "127.0.0.1:8080");
        assert_eq!(pc.mode, ProxyMode::Transparent);
        assert_eq!(pc.strategy, Strategy::Random);
    }

    #[test]
    fn proxy_mode_deserialization() {
        #[derive(Deserialize)]
        struct Wrapper {
            mode: ProxyMode,
        }
        let w: Wrapper = toml::from_str("mode = \"transparent\"").unwrap();
        assert_eq!(w.mode, ProxyMode::Transparent);

        let w: Wrapper = toml::from_str("mode = \"mitm\"").unwrap();
        assert_eq!(w.mode, ProxyMode::Mitm);
    }

    #[test]
    fn strategy_deserialization() {
        #[derive(Deserialize)]
        struct Wrapper {
            strategy: Strategy,
        }
        let w: Wrapper = toml::from_str("strategy = \"random\"").unwrap();
        assert_eq!(w.strategy, Strategy::Random);

        let w: Wrapper = toml::from_str("strategy = \"round-robin\"").unwrap();
        assert_eq!(w.strategy, Strategy::RoundRobin);
    }

    #[test]
    fn strategy_from_name() {
        assert_eq!(Strategy::from_name("random").unwrap(), Strategy::Random);
        assert_eq!(
            Strategy::from_name("round-robin").unwrap(),
            Strategy::RoundRobin
        );
        assert_eq!(
            Strategy::from_name("roundrobin").unwrap(),
            Strategy::RoundRobin
        );
        assert_eq!(Strategy::from_name("rr").unwrap(), Strategy::RoundRobin);
        assert!(Strategy::from_name("nonexistent").is_err());
    }

    #[test]
    fn strategy_display() {
        assert_eq!(Strategy::Random.to_string(), "random");
        assert_eq!(Strategy::RoundRobin.to_string(), "round-robin");
    }

    #[test]
    fn proxy_mode_display() {
        assert_eq!(ProxyMode::Transparent.to_string(), "transparent");
        assert_eq!(ProxyMode::Mitm.to_string(), "mitm");
    }

    #[test]
    fn full_toml_with_multiple_tunnels() {
        let toml_str = r#"
[proxy]
listen = "0.0.0.0:9090"
mode = "mitm"
strategy = "round-robin"

[[tunnel]]
protocol = "socks5"
address = "127.0.0.1:1080"
label = "ssh-tokyo"

[[tunnel]]
protocol = "wireguard"
endpoint = "169.150.196.30:51820"
private_key = "my-private-key"
public_key = "server-public-key"
address = "10.5.0.2/32"
dns = "103.86.96.100"
label = "nord-us-east"

[[tunnel]]
protocol = "sentinel"
node = "sentnode1qx..."
denom = "udvpn"
deposit = "1000000udvpn"
wallet_key = "~/.houdinny/sentinel_key"
label = "sentinel-singapore"
"#;
        let cfg = Config::parse_toml(toml_str).expect("multi-tunnel toml should parse");
        assert_eq!(cfg.proxy.listen, "0.0.0.0:9090");
        assert_eq!(cfg.proxy.mode, ProxyMode::Mitm);
        assert_eq!(cfg.proxy.strategy, Strategy::RoundRobin);
        assert_eq!(cfg.tunnel.len(), 3);
        assert_eq!(cfg.tunnel[0].protocol, "socks5");
        assert_eq!(cfg.tunnel[1].protocol, "wireguard");
        assert_eq!(cfg.tunnel[1].private_key.as_deref(), Some("my-private-key"));
        assert_eq!(cfg.tunnel[2].protocol, "sentinel");
        assert_eq!(cfg.tunnel[2].node.as_deref(), Some("sentnode1qx..."));
    }

    #[test]
    fn from_file_nonexistent_returns_error() {
        let result = Config::from_file(Path::new("/nonexistent/path/tunnels.toml"));
        assert!(result.is_err());
    }
}
