//! Configuration loading and validation for houdinny.
//!
//! Supports loading from TOML files (`houdinny.toml` / `tunnels.toml`) or from
//! CLI tunnel URLs (`socks5://127.0.0.1:1080`). All structs derive
//! [`serde::Deserialize`] so the TOML crate handles the heavy lifting.
//!
//! # Environment variable resolution
//!
//! Config values may reference environment variables via `${VAR_NAME}` syntax.
//! The `.env` file (if present) is loaded before config parsing so secrets can
//! live outside the config file.
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

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Top-level configuration, typically loaded from `houdinny.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Proxy server settings (listen address, mode, strategy).
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Docker-managed VPN configuration.
    #[serde(default)]
    pub docker: DockerConfig,

    /// List of tunnel definitions.
    #[serde(default)]
    pub tunnel: Vec<TunnelConfig>,
}

// ---------------------------------------------------------------------------
// Proxy config
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Docker config
// ---------------------------------------------------------------------------

/// Docker-managed VPN configuration section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DockerConfig {
    /// Whether Docker VPN management is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// List of VPN containers to spin up.
    #[serde(default)]
    pub vpn: Vec<VpnConfig>,
}

/// A single Docker-managed VPN container configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct VpnConfig {
    /// VPN provider (`"nordvpn"` for now).
    pub provider: String,

    /// Provider token/credential. May use `${ENV_VAR}` syntax.
    pub token: Option<String>,

    /// Country code or name (e.g. `"us"`, `"de"`, `"jp"`).
    pub country: String,
}

// ---------------------------------------------------------------------------
// Tunnel config
// ---------------------------------------------------------------------------

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

    /// Network interface name (WireGuard, e.g. `"wg0"`, `"utun3"`, `"nordlynx"`).
    pub interface: Option<String>,

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
// Environment variable resolution
// ---------------------------------------------------------------------------

/// Load environment variables from a `.env` file.
///
/// Lines are parsed as `KEY=VALUE` pairs. Empty lines and lines starting with
/// `#` are skipped. This is intentionally simple — no quoting, no multi-line
/// values — to avoid adding a dependency.
pub fn load_dotenv(path: &Path) {
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                // SAFETY: we only call this at startup before spawning threads.
                #[allow(deprecated)]
                unsafe {
                    std::env::set_var(key.trim(), value.trim());
                }
            }
        }
    }
}

/// Resolve `${VAR_NAME}` references in a string from environment variables.
///
/// If a referenced variable is not set, the `${VAR_NAME}` token is left as-is
/// so the user sees what went wrong.
pub fn resolve_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            // Consume '{'
            chars.next();
            // Collect the variable name until '}'
            let mut var_name = String::new();
            let mut found_close = false;
            for c in chars.by_ref() {
                if c == '}' {
                    found_close = true;
                    break;
                }
                var_name.push(c);
            }
            if found_close && !var_name.is_empty() {
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        // Leave the original token so the user can see
                        // which variable is missing.
                        result.push_str("${");
                        result.push_str(&var_name);
                        result.push('}');
                    }
                }
            } else {
                // Malformed — push what we consumed literally.
                result.push_str("${");
                result.push_str(&var_name);
                if found_close {
                    result.push('}');
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
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
    ///
    /// The raw TOML content has `${VAR}` references resolved before parsing so
    /// that secrets can be kept in environment variables / `.env`.
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
        let resolved = resolve_env_vars(&contents);
        resolved.parse()
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
                    interface: None,
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
            docker: DockerConfig::default(),
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
    fn parse_houdinny_example_toml() {
        // Set a fake token so env var resolution works.
        #[allow(deprecated)]
        unsafe {
            std::env::set_var("NORD_TOKEN", "test-token-123");
        }
        let toml_str = include_str!("../../houdinny.toml.example");
        let resolved = resolve_env_vars(toml_str);
        let cfg = Config::parse_toml(&resolved).expect("houdinny.toml.example should parse");

        assert_eq!(cfg.proxy.listen, "127.0.0.1:8080");
        assert!(cfg.docker.enabled);
        assert_eq!(cfg.docker.vpn.len(), 3);
        assert_eq!(cfg.docker.vpn[0].provider, "nordvpn");
        assert_eq!(cfg.docker.vpn[0].token.as_deref(), Some("test-token-123"));
        assert_eq!(cfg.docker.vpn[0].country, "us");
        assert_eq!(cfg.docker.vpn[1].country, "de");
        assert_eq!(cfg.docker.vpn[2].country, "jp");
    }

    #[test]
    fn parse_empty_config_uses_defaults() {
        let cfg = Config::parse_toml("").expect("empty string should parse");
        assert_eq!(cfg.proxy.listen, "127.0.0.1:8080");
        assert_eq!(cfg.proxy.mode, ProxyMode::Transparent);
        assert_eq!(cfg.proxy.strategy, Strategy::Random);
        assert!(cfg.tunnel.is_empty());
        assert!(!cfg.docker.enabled);
        assert!(cfg.docker.vpn.is_empty());
    }

    #[test]
    fn parse_config_without_docker_section() {
        let toml_str = r#"
[proxy]
listen = "0.0.0.0:9090"

[[tunnel]]
protocol = "socks5"
address = "127.0.0.1:1080"
label = "my-socks"
"#;
        let cfg = Config::parse_toml(toml_str).expect("config without docker should parse");
        assert_eq!(cfg.proxy.listen, "0.0.0.0:9090");
        assert!(!cfg.docker.enabled);
        assert!(cfg.docker.vpn.is_empty());
        assert_eq!(cfg.tunnel.len(), 1);
    }

    #[test]
    fn parse_config_with_docker_section() {
        let toml_str = r#"
[proxy]
listen = "127.0.0.1:8080"
strategy = "round-robin"

[docker]
enabled = true

[[docker.vpn]]
provider = "nordvpn"
token = "my-token"
country = "us"

[[docker.vpn]]
provider = "nordvpn"
token = "my-token"
country = "de"
"#;
        let cfg = Config::parse_toml(toml_str).expect("config with docker should parse");
        assert!(cfg.docker.enabled);
        assert_eq!(cfg.docker.vpn.len(), 2);
        assert_eq!(cfg.docker.vpn[0].provider, "nordvpn");
        assert_eq!(cfg.docker.vpn[0].token.as_deref(), Some("my-token"));
        assert_eq!(cfg.docker.vpn[0].country, "us");
        assert_eq!(cfg.docker.vpn[1].country, "de");
    }

    #[test]
    fn resolve_env_vars_existing() {
        #[allow(deprecated)]
        unsafe {
            std::env::set_var("HOUDINNY_TEST_VAR", "hello_world");
        }
        let result = resolve_env_vars("token = \"${HOUDINNY_TEST_VAR}\"");
        assert_eq!(result, "token = \"hello_world\"");
    }

    #[test]
    fn resolve_env_vars_missing_left_as_is() {
        let result = resolve_env_vars("token = \"${HOUDINNY_NONEXISTENT_VAR_12345}\"");
        assert_eq!(result, "token = \"${HOUDINNY_NONEXISTENT_VAR_12345}\"");
    }

    #[test]
    fn resolve_env_vars_multiple() {
        #[allow(deprecated)]
        unsafe {
            std::env::set_var("HOUDINNY_A", "aaa");
            std::env::set_var("HOUDINNY_B", "bbb");
        }
        let result = resolve_env_vars("${HOUDINNY_A} and ${HOUDINNY_B}");
        assert_eq!(result, "aaa and bbb");
    }

    #[test]
    fn resolve_env_vars_no_refs() {
        let result = resolve_env_vars("just a plain string");
        assert_eq!(result, "just a plain string");
    }

    #[test]
    fn resolve_env_vars_empty_braces() {
        // ${} — empty var name — left as-is
        let result = resolve_env_vars("before ${}after");
        assert_eq!(result, "before ${}after");
    }

    #[test]
    fn dotenv_loading() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(
            &env_path,
            "# comment\nHOUDINNY_DOTENV_TEST=loaded_value\n\nANOTHER=42\n",
        )
        .unwrap();

        load_dotenv(&env_path);

        assert_eq!(
            std::env::var("HOUDINNY_DOTENV_TEST").unwrap(),
            "loaded_value"
        );
        assert_eq!(std::env::var("ANOTHER").unwrap(), "42");
    }

    #[test]
    fn config_with_env_var_references_resolves() {
        #[allow(deprecated)]
        unsafe {
            std::env::set_var("HOUDINNY_CFG_TOKEN", "secret-abc");
        }
        let toml_str = r#"
[docker]
enabled = true

[[docker.vpn]]
provider = "nordvpn"
token = "${HOUDINNY_CFG_TOKEN}"
country = "us"
"#;
        let resolved = resolve_env_vars(toml_str);
        let cfg = Config::parse_toml(&resolved).expect("resolved config should parse");
        assert_eq!(cfg.docker.vpn[0].token.as_deref(), Some("secret-abc"));
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
