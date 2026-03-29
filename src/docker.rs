//! Docker stack orchestration for `houdinny start` / `houdinny stop`.
//!
//! Generates docker-compose.yml, .env, and tunnels.docker.toml, then
//! shells out to `docker compose` to bring the stack up or down.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::process::Command;

use crate::config::DockerConfig;

// ---------------------------------------------------------------------------
// Docker Compose structs (serialized via serde_yaml)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ComposeFile {
    services: BTreeMap<String, Service>,
    networks: BTreeMap<String, Network>,
}

#[derive(Default, Serialize)]
struct Service {
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<Build>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cap_add: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sysctls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthcheck: Option<Healthcheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    networks: Option<BTreeMap<String, NetworkConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ports: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volumes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depends_on: Option<BTreeMap<String, DependsOn>>,
}

#[derive(Serialize)]
struct Build {
    context: String,
    dockerfile: String,
}

#[derive(Serialize)]
struct Healthcheck {
    test: Vec<String>,
    interval: String,
    timeout: String,
    retries: u32,
    start_period: String,
}

#[derive(Serialize)]
struct NetworkConfig {
    ipv4_address: String,
}

#[derive(Serialize)]
struct DependsOn {
    condition: String,
}

#[derive(Serialize)]
struct Network {
    driver: String,
    ipam: Ipam,
}

#[derive(Serialize)]
struct Ipam {
    config: Vec<IpamConfig>,
}

#[derive(Serialize)]
struct IpamConfig {
    subnet: String,
}

// ---------------------------------------------------------------------------
// Country code mapping
// ---------------------------------------------------------------------------

/// Map short country codes to the full names NordVPN expects in CONNECT=.
fn country_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("us", "United_States");
    m.insert("de", "Germany");
    m.insert("jp", "Japan");
    m.insert("kr", "South_Korea");
    m.insert("gb", "United_Kingdom");
    m.insert("uk", "United_Kingdom");
    m.insert("fr", "France");
    m.insert("nl", "Netherlands");
    m.insert("ca", "Canada");
    m.insert("au", "Australia");
    m.insert("ch", "Switzerland");
    m.insert("se", "Sweden");
    m.insert("sg", "Singapore");
    m.insert("hk", "Hong_Kong");
    m.insert("in", "India");
    m.insert("br", "Brazil");
    m.insert("it", "Italy");
    m.insert("es", "Spain");
    m.insert("pl", "Poland");
    m.insert("at", "Austria");
    m.insert("no", "Norway");
    m.insert("dk", "Denmark");
    m.insert("fi", "Finland");
    m.insert("ie", "Ireland");
    m.insert("nz", "New_Zealand");
    m.insert("mx", "Mexico");
    m.insert("za", "South_Africa");
    m.insert("ro", "Romania");
    m.insert("cz", "Czech_Republic");
    m.insert("tw", "Taiwan");
    m
}

/// Resolve a user-supplied country string to the NordVPN CONNECT value.
///
/// - Known two/three-letter codes are mapped (e.g. "us" -> "United_States").
/// - If the input contains an underscore or is longer than 3 characters it is
///   assumed to be a full name and passed through unchanged.
/// - Unknown short codes are passed through as-is.
pub fn resolve_country(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    if lower.contains('_') || lower.len() > 3 {
        // Treat as a full name — capitalise each word for consistency.
        return input
            .trim()
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().to_string() + &c.as_str().to_lowercase(),
                }
            })
            .collect::<Vec<_>>()
            .join("_");
    }
    let map = country_map();
    match map.get(lower.as_str()) {
        Some(full) => (*full).to_string(),
        None => input.trim().to_string(), // unknown code — pass through
    }
}

// ---------------------------------------------------------------------------
// Config generation
// ---------------------------------------------------------------------------

/// Generate docker-compose.yml content for N countries.
pub fn generate_docker_compose(countries: &[String], port: u16) -> String {
    let mut services = BTreeMap::new();

    for (i, country) in countries.iter().enumerate() {
        let n = i + 1;
        let ip = format!("172.20.0.{}", 10 + n);
        let name = resolve_country(country);

        services.insert(
            format!("vpn-{n}"),
            Service {
                build: Some(Build {
                    context: "./docker".into(),
                    dockerfile: "Dockerfile.vpn".into(),
                }),
                container_name: Some(format!("houdinny-vpn-{n}")),
                cap_add: Some(vec!["NET_ADMIN".into(), "NET_RAW".into()]),
                sysctls: Some(vec!["net.ipv6.conf.all.disable_ipv6=1".into()]),
                environment: Some(vec![
                    "TOKEN=${NORD_TOKEN}".into(),
                    format!("CONNECT={name}"),
                    "TECHNOLOGY=NordLynx".into(),
                    "WHITELIST_SUBNET=172.20.0.0/16".into(),
                    "NETWORK=172.20.0.0/16".into(),
                ]),
                healthcheck: Some(Healthcheck {
                    test: vec![
                        "CMD-SHELL".into(),
                        "nordvpn status | grep -qi connected && nc -z 127.0.0.1 1080".into(),
                    ],
                    interval: "10s".into(),
                    timeout: "5s".into(),
                    retries: 18,
                    start_period: "60s".into(),
                }),
                networks: Some(BTreeMap::from([(
                    "houdinny-net".into(),
                    NetworkConfig { ipv4_address: ip },
                )])),
                ..Default::default()
            },
        );
    }

    // houdinny service — no depends_on health checks; the `start` command handles waiting
    services.insert(
        "houdinny".into(),
        Service {
            build: Some(Build {
                context: ".".into(),
                dockerfile: "Dockerfile".into(),
            }),
            container_name: Some("houdinny".into()),
            ports: Some(vec![format!("{port}:{port}")]),
            volumes: Some(vec![
                "./tunnels.docker.toml:/etc/houdinny/tunnels.toml:ro".into(),
            ]),
            command: Some(vec!["-c".into(), "/etc/houdinny/tunnels.toml".into()]),
            networks: Some(BTreeMap::from([(
                "houdinny-net".into(),
                NetworkConfig {
                    ipv4_address: "172.20.0.20".into(),
                },
            )])),
            ..Default::default()
        },
    );

    let compose = ComposeFile {
        services,
        networks: BTreeMap::from([(
            "houdinny-net".into(),
            Network {
                driver: "bridge".into(),
                ipam: Ipam {
                    config: vec![IpamConfig {
                        subnet: "172.20.0.0/16".into(),
                    }],
                },
            },
        )]),
    };

    let mut yaml = "# Auto-generated by `houdinny start`\n\n".to_string();
    yaml.push_str(&serde_yaml::to_string(&compose).expect("valid compose config"));
    yaml
}

/// Generate .env file content.
pub fn generate_env(nord_token: &str) -> String {
    format!("NORD_TOKEN={nord_token}\n")
}

/// Generate tunnels.docker.toml content for N countries.
pub fn generate_tunnels_config(countries: &[String], strategy: &str, port: u16) -> String {
    let mut toml = format!(
        "\
# Auto-generated by `houdinny start` — do not edit manually.

[proxy]
listen = \"0.0.0.0:{port}\"
mode = \"transparent\"
strategy = \"{strategy}\"
"
    );

    for (i, country) in countries.iter().enumerate() {
        let n = i + 1;
        let ip_last = 10 + n;
        let country_name = resolve_country(country);
        toml.push_str(&format!(
            "\n\
[[tunnel]]
protocol = \"socks5\"
address = \"172.20.0.{ip_last}:1080\"
label = \"vpn-{n}-{country_name}\"
"
        ));
    }

    toml
}

// ---------------------------------------------------------------------------
// Docker binary detection
// ---------------------------------------------------------------------------

async fn find_docker() -> Result<String> {
    // Check common paths first, then fall back to PATH.
    for candidate in &[
        "/usr/local/bin/docker",
        "/usr/bin/docker",
        "/opt/homebrew/bin/docker",
    ] {
        if tokio::fs::metadata(candidate).await.is_ok() {
            return Ok(candidate.to_string());
        }
    }
    // Try via PATH
    let output = Command::new("which")
        .arg("docker")
        .output()
        .await
        .context("failed to search PATH for docker")?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }
    bail!(
        "docker not found. Please install Docker Desktop or Docker Engine.\n\
         https://docs.docker.com/get-docker/"
    );
}

// ---------------------------------------------------------------------------
// Start command
// ---------------------------------------------------------------------------

/// Generate all config files and bring the Docker stack up.
///
/// Reads VPN entries from `DockerConfig`. A `token_override` (from `--nord-token`)
/// takes precedence over per-VPN tokens in the config.
pub async fn start(
    docker_cfg: &DockerConfig,
    token_override: Option<&str>,
    port: u16,
    strategy: &str,
    no_wait: bool,
) -> Result<()> {
    if docker_cfg.vpn.is_empty() {
        bail!(
            "no VPN entries configured. Add [[docker.vpn]] sections to houdinny.toml \
             or pass --nord-token + --countries."
        );
    }

    // Resolve the token — CLI flag wins, then per-VPN config, then error.
    let first_token = token_override
        .map(|s| s.to_string())
        .or_else(|| docker_cfg.vpn.iter().find_map(|v| v.token.clone()));
    let nord_token = first_token.as_deref().unwrap_or("");
    if nord_token.is_empty() || nord_token.contains("${") {
        bail!(
            "NordVPN token not set. Either:\n  \
             1. Put NORD_TOKEN=... in .env and use ${{NORD_TOKEN}} in houdinny.toml\n  \
             2. Pass --nord-token=... on the command line"
        );
    }

    // Build the country list from config.
    let countries: Vec<String> = docker_cfg.vpn.iter().map(|v| v.country.clone()).collect();

    let docker = find_docker().await?;

    // Resolve the project directory (where Cargo.toml lives).
    let project_dir = std::env::current_dir().context("failed to determine current directory")?;

    tracing::info!(
        countries = ?countries,
        port = port,
        strategy = strategy,
        "generating configuration files"
    );

    // 1. Write docker-compose.yml
    let compose = generate_docker_compose(&countries, port);
    let compose_path = project_dir.join("docker-compose.yml");
    tokio::fs::write(&compose_path, &compose)
        .await
        .context("failed to write docker-compose.yml")?;
    tracing::info!(path = %compose_path.display(), "wrote docker-compose.yml");

    // 2. Write .env
    let env_content = generate_env(nord_token);
    let env_path = project_dir.join(".env");
    tokio::fs::write(&env_path, &env_content)
        .await
        .context("failed to write .env")?;
    tracing::info!(path = %env_path.display(), "wrote .env");

    // 3. Write tunnels.docker.toml
    let tunnels = generate_tunnels_config(&countries, strategy, port);
    let tunnels_path = project_dir.join("tunnels.docker.toml");
    tokio::fs::write(&tunnels_path, &tunnels)
        .await
        .context("failed to write tunnels.docker.toml")?;
    tracing::info!(path = %tunnels_path.display(), "wrote tunnels.docker.toml");

    // 4. docker compose up -d --build
    println!("Starting VPN containers...");
    let status = Command::new(&docker)
        .args(["compose", "up", "-d", "--build"])
        .current_dir(&project_dir)
        .status()
        .await
        .context("failed to run `docker compose up`")?;

    if !status.success() {
        bail!(
            "`docker compose up` exited with status {}",
            status.code().unwrap_or(-1)
        );
    }

    // 5. Wait for health checks (unless --no-wait)
    if !no_wait {
        wait_for_healthy(&docker, &project_dir, &countries).await?;
    }

    // 6. Success banner
    println!();
    println!("Ready! All VPN containers are healthy.");
    println!();
    println!("  export HTTP_PROXY=http://localhost:{port}");
    println!("  export HTTPS_PROXY=http://localhost:{port}");
    println!();
    println!("  curl https://httpbin.org/ip   # different IP each time");
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Health check polling
// ---------------------------------------------------------------------------

/// Per-VPN health state tracked during polling.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum VpnState {
    Waiting,
    Connected,
    Failed(String),
}

impl VpnState {
    fn is_resolved(&self) -> bool {
        matches!(self, VpnState::Connected | VpnState::Failed(_))
    }

    fn is_connected(&self) -> bool {
        matches!(self, VpnState::Connected)
    }
}

/// Inspect the last few log lines of a stopped container and return a
/// human-readable reason for the failure.
async fn diagnose_container(docker: &str, container: &str) -> String {
    let logs = Command::new(docker)
        .args(["logs", "--tail", "20", container])
        .output()
        .await;

    let log_text = match &logs {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            format!("{stdout}{stderr}")
        }
        Err(_) => String::new(),
    };

    let lower = log_text.to_lowercase();

    if lower.contains("token") {
        return "invalid or expired NordVPN token".to_string();
    }
    if lower.contains("no such device") {
        return "WireGuard/NordLynx not available — restart Docker Desktop".to_string();
    }

    // Fall back to last non-empty log line.
    let last_line = log_text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
        .trim();
    last_line.to_string()
}

async fn wait_for_healthy(docker: &str, _project_dir: &Path, countries: &[String]) -> Result<()> {
    let timeout = Duration::from_secs(180);
    let poll_interval = Duration::from_secs(5);
    let start = Instant::now();
    let expected_vpns = countries.len();

    println!("Waiting for {expected_vpns} VPN container(s) to connect...");

    let mut states: Vec<VpnState> = vec![VpnState::Waiting; expected_vpns];

    loop {
        // ---- poll each unresolved VPN ----
        for (i, state) in states.iter_mut().enumerate() {
            if state.is_resolved() {
                continue;
            }
            let n = i + 1;
            let container = format!("houdinny-vpn-{n}");
            let country_name = resolve_country(&countries[i]);

            // 1. Check if the container is still running.
            let inspect = Command::new(docker)
                .args(["inspect", "-f", "{{.State.Running}}", &container])
                .output()
                .await;

            let running = match &inspect {
                Ok(out) => String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .eq_ignore_ascii_case("true"),
                Err(_) => false,
            };

            if !running {
                // Container exited — diagnose and mark as failed.
                let reason = diagnose_container(docker, &container).await;
                println!("  VPN-{n} ({country_name}) FAILED: container exited — {reason}");
                *state = VpnState::Failed(reason);
                continue;
            }

            // 2. Container is running — check NordVPN connection status.
            let output = Command::new(docker)
                .args(["exec", &container, "nordvpn", "status"])
                .output()
                .await;

            if let Ok(out) = output {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.to_lowercase().contains("status: connected") {
                    println!("  VPN-{n} ({country_name}) connected!");

                    // Whitelist subnet + start microsocks
                    let _ = Command::new(docker)
                        .args([
                            "exec",
                            &container,
                            "nordvpn",
                            "whitelist",
                            "add",
                            "subnet",
                            "172.20.0.0/16",
                        ])
                        .output()
                        .await;

                    let _ = Command::new(docker)
                        .args([
                            "exec",
                            "-d",
                            &container,
                            "microsocks",
                            "-p",
                            "1080",
                            "-b",
                            "0.0.0.0",
                        ])
                        .output()
                        .await;

                    *state = VpnState::Connected;
                }
            }
        }

        // ---- evaluate overall progress ----
        let connected_count = states.iter().filter(|s| s.is_connected()).count();
        let all_resolved = states.iter().all(|s| s.is_resolved());

        if all_resolved || start.elapsed() > timeout {
            // Mark any still-waiting VPNs as timed-out failures.
            for (i, state) in states.iter_mut().enumerate() {
                if matches!(state, VpnState::Waiting) {
                    let n = i + 1;
                    let container = format!("houdinny-vpn-{n}");
                    let country_name = resolve_country(&countries[i]);
                    println!(
                        "  VPN-{n} ({country_name}) FAILED: timed out after {}s — \
                         check `docker logs {container}`",
                        timeout.as_secs(),
                    );
                    *state = VpnState::Failed("timed out".to_string());
                }
            }

            if connected_count == 0 {
                bail!(
                    "No VPNs connected. Troubleshooting:\n  \
                     1. Check your NordVPN token in .env — is it valid?\n  \
                     2. Run: docker logs houdinny-vpn-1\n  \
                     3. Try restarting Docker Desktop\n  \
                     4. Try a different country"
                );
            }

            println!("\nProceeding with {connected_count}/{expected_vpns} tunnel(s).");

            // Restart houdinny so it can connect to now-available SOCKS5 proxies
            if connected_count == expected_vpns {
                println!("  All VPNs connected. Restarting houdinny proxy...");
            } else {
                println!("  Restarting houdinny proxy with available tunnels...");
            }
            let _ = Command::new(docker)
                .args(["restart", "houdinny"])
                .output()
                .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }

        tracing::debug!(
            connected = connected_count,
            expected = expected_vpns,
            "waiting..."
        );
        tokio::time::sleep(poll_interval).await;
    }
}

// ---------------------------------------------------------------------------
// Stop command
// ---------------------------------------------------------------------------

/// Bring the Docker stack down.
pub async fn stop() -> Result<()> {
    let docker = find_docker().await?;
    let project_dir = std::env::current_dir().context("failed to determine current directory")?;

    println!("Stopping houdinny Docker stack...");

    let status = Command::new(&docker)
        .args(["compose", "down"])
        .current_dir(&project_dir)
        .status()
        .await
        .context("failed to run `docker compose down`")?;

    if !status.success() {
        bail!(
            "`docker compose down` exited with status {}",
            status.code().unwrap_or(-1)
        );
    }

    println!("houdinny Docker stack stopped.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_country_code_mapping() {
        assert_eq!(resolve_country("us"), "United_States");
        assert_eq!(resolve_country("de"), "Germany");
        assert_eq!(resolve_country("jp"), "Japan");
        assert_eq!(resolve_country("kr"), "South_Korea");
        assert_eq!(resolve_country("gb"), "United_Kingdom");
        assert_eq!(resolve_country("uk"), "United_Kingdom");
        assert_eq!(resolve_country("fr"), "France");
        assert_eq!(resolve_country("sg"), "Singapore");
    }

    #[test]
    fn test_full_name_passthrough() {
        // Full names (contain underscore or > 3 chars) should pass through.
        assert_eq!(resolve_country("United_States"), "United_States");
        assert_eq!(resolve_country("Germany"), "Germany");
        assert_eq!(resolve_country("South_Korea"), "South_Korea");
    }

    #[test]
    fn test_unknown_code_passthrough() {
        // Unknown short codes pass through as-is.
        assert_eq!(resolve_country("zz"), "zz");
        assert_eq!(resolve_country("xx"), "xx");
    }

    #[test]
    fn test_generate_docker_compose_valid_yaml() {
        let countries = vec!["us".to_string(), "de".to_string()];
        let yaml_str = generate_docker_compose(&countries, 8080);

        // Strip the leading comment before parsing
        let yaml_body: String = yaml_str
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        // Should parse as valid YAML
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&yaml_body).expect("generated YAML should be valid");

        // Top-level keys must exist
        assert!(parsed.get("services").is_some(), "missing 'services' key");
        assert!(parsed.get("networks").is_some(), "missing 'networks' key");
    }

    #[test]
    fn test_generate_docker_compose_one_country() {
        let countries = vec!["us".to_string()];
        let yaml = generate_docker_compose(&countries, 8080);

        // Parse back to verify structure
        let yaml_body: String = yaml
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_body).expect("valid YAML");

        let services = parsed.get("services").unwrap().as_mapping().unwrap();

        // Should have vpn-1 and houdinny
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-1".into())));
        assert!(services.contains_key(&serde_yaml::Value::String("houdinny".into())));
        // Should NOT have vpn-2
        assert!(!services.contains_key(&serde_yaml::Value::String("vpn-2".into())));

        // Check content via string assertions too
        assert!(yaml.contains("CONNECT=United_States"));
        assert!(yaml.contains("172.20.0.11"));
        assert!(yaml.contains("8080:8080"));
    }

    #[test]
    fn test_generate_docker_compose_three_countries() {
        let countries = vec!["us".to_string(), "de".to_string(), "jp".to_string()];
        let yaml = generate_docker_compose(&countries, 9090);

        let yaml_body: String = yaml
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_body).expect("valid YAML");

        let services = parsed.get("services").unwrap().as_mapping().unwrap();

        assert!(services.contains_key(&serde_yaml::Value::String("vpn-1".into())));
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-2".into())));
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-3".into())));
        assert!(!services.contains_key(&serde_yaml::Value::String("vpn-4".into())));

        assert!(yaml.contains("CONNECT=United_States"));
        assert!(yaml.contains("CONNECT=Germany"));
        assert!(yaml.contains("CONNECT=Japan"));
        assert!(yaml.contains("172.20.0.11"));
        assert!(yaml.contains("172.20.0.12"));
        assert!(yaml.contains("172.20.0.13"));
        assert!(yaml.contains("9090:9090"));
    }

    #[test]
    fn test_generate_docker_compose_five_countries() {
        let countries = vec![
            "us".to_string(),
            "de".to_string(),
            "jp".to_string(),
            "kr".to_string(),
            "gb".to_string(),
        ];
        let yaml = generate_docker_compose(&countries, 8080);

        let yaml_body: String = yaml
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_body).expect("valid YAML");

        let services = parsed.get("services").unwrap().as_mapping().unwrap();
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-5".into())));

        assert!(yaml.contains("CONNECT=United_Kingdom"));
        assert!(yaml.contains("172.20.0.15"));

        // All 5 VPN services should exist
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-1".into())));
        assert!(services.contains_key(&serde_yaml::Value::String("vpn-5".into())));
    }

    #[test]
    fn test_generate_docker_compose_healthcheck_values() {
        let countries = vec!["us".to_string()];
        let yaml = generate_docker_compose(&countries, 8080);

        // Verify the updated healthcheck timing values
        assert!(yaml.contains("60s"), "start_period should be 60s");
        assert!(yaml.contains("retries: 18"), "retries should be 18");
    }

    #[test]
    fn test_generate_tunnels_config() {
        let countries = vec!["us".to_string(), "de".to_string(), "jp".to_string()];
        let toml = generate_tunnels_config(&countries, "round-robin", 8080);

        assert!(toml.contains("listen = \"0.0.0.0:8080\""));
        assert!(toml.contains("strategy = \"round-robin\""));
        assert!(toml.contains("address = \"172.20.0.11:1080\""));
        assert!(toml.contains("address = \"172.20.0.12:1080\""));
        assert!(toml.contains("address = \"172.20.0.13:1080\""));
        assert!(toml.contains("label = \"vpn-1-United_States\""));
        assert!(toml.contains("label = \"vpn-2-Germany\""));
        assert!(toml.contains("label = \"vpn-3-Japan\""));
    }

    #[test]
    fn test_generate_env() {
        let env = generate_env("my-secret-token");
        assert_eq!(env, "NORD_TOKEN=my-secret-token\n");
    }

    #[test]
    fn test_country_code_case_insensitive() {
        assert_eq!(resolve_country("US"), "United_States");
        assert_eq!(resolve_country("De"), "Germany");
        assert_eq!(resolve_country("JP"), "Japan");
    }
}
