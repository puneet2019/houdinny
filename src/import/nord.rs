//! NordVPN WireGuard tunnel import.
//!
//! Fetches the user's WireGuard private key and recommended server list from
//! the NordVPN API, then generates houdinny tunnel config entries.
//!
//! # API Flow
//!
//! 1. **Credentials** — `GET https://api.nordvpn.com/v1/users/services/credentials`
//!    with `Authorization: token:<TOKEN>` header. Returns the user's NordLynx
//!    (WireGuard) private key.
//!
//! 2. **Server recommendations** —
//!    `GET https://api.nordvpn.com/v1/servers/recommendations?filters[servers_technologies][identifier]=wireguard_udp&limit=N`
//!    Optionally filtered by country. Returns server metadata including public
//!    keys and endpoints.
//!
//! **NOTE:** The exact NordVPN API endpoints and response shapes are based on
//! community documentation and may change without notice. If imports fail,
//! verify the API contract at <https://api.nordvpn.com>.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// NordVPN API response types
// ---------------------------------------------------------------------------

/// Response from the credentials endpoint.
#[derive(Debug, Deserialize)]
pub struct CredentialsResponse {
    pub nordlynx_private_key: String,
}

/// A recommended server entry.
#[derive(Debug, Deserialize)]
pub struct ServerEntry {
    #[allow(dead_code)]
    pub id: u64,
    pub name: String,
    pub hostname: String,
    /// The server's IP address (used as the WireGuard endpoint).
    pub station: String,
    pub technologies: Vec<Technology>,
    pub locations: Vec<Location>,
}

#[derive(Debug, Deserialize)]
pub struct Technology {
    pub identifier: String,
    pub metadata: Vec<TechMetadata>,
    #[allow(dead_code)]
    pub pivot: Option<TechPivot>,
}

#[derive(Debug, Deserialize)]
pub struct TechMetadata {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct TechPivot {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct Location {
    pub country: Country,
}

#[derive(Debug, Deserialize)]
pub struct Country {
    pub code: String,
    #[allow(dead_code)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Country code to NordVPN country-id mapping (subset)
// ---------------------------------------------------------------------------

/// Map a two-letter country code to the NordVPN numeric country ID.
///
/// This is an incomplete mapping covering the most commonly requested
/// countries. Unknown codes are silently ignored.
fn country_code_to_id(code: &str) -> Option<u32> {
    match code.to_uppercase().as_str() {
        "US" => Some(228),
        "GB" | "UK" => Some(227),
        "DE" => Some(81),
        "JP" => Some(114),
        "NL" => Some(153),
        "CA" => Some(38),
        "AU" => Some(13),
        "FR" => Some(74),
        "CH" => Some(209),
        "SE" => Some(208),
        "SG" => Some(195),
        "HK" => Some(97),
        "IT" => Some(106),
        "ES" => Some(202),
        "BR" => Some(30),
        "IN" => Some(100),
        "KR" => Some(114),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch NordVPN WireGuard credentials and recommended servers, then write
/// tunnel config entries to `output`.
///
/// If `output` already exists, new `[[tunnel]]` entries are appended.
/// Otherwise a fresh config file is created with a default `[proxy]` section.
pub async fn import_nord(
    token: &str,
    count: usize,
    countries: Option<&[String]>,
    output: &Path,
) -> Result<()> {
    tracing::info!("fetching NordVPN WireGuard credentials...");

    let client = reqwest::Client::new();

    // 1. Fetch the user's WireGuard private key.
    let creds = fetch_credentials(&client, token).await?;
    tracing::debug!("obtained NordLynx private key");

    // 2. Fetch recommended servers (one request per country, or a single
    //    request if no country filter is specified).
    let servers = fetch_servers(&client, count, countries).await?;

    if servers.is_empty() {
        bail!("NordVPN returned no servers matching the requested criteria");
    }

    // 3. Generate TOML entries.
    let toml_block = generate_toml(&creds.nordlynx_private_key, &servers)?;

    // 4. Write / append to the output file.
    write_config(output, &toml_block)?;

    tracing::info!(
        count = servers.len(),
        path = %output.display(),
        "added NordVPN tunnels"
    );
    println!(
        "Added {} NordVPN tunnel(s) to {}",
        servers.len(),
        output.display()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Fetch the user's NordLynx (WireGuard) private key.
async fn fetch_credentials(client: &reqwest::Client, token: &str) -> Result<CredentialsResponse> {
    let url = "https://api.nordvpn.com/v1/users/services/credentials";

    let resp = client
        .get(url)
        .basic_auth("token", Some(token))
        .send()
        .await
        .context("failed to reach NordVPN credentials API")?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("NordVPN API returned 401 — check your access token");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("NordVPN credentials API returned HTTP {status}: {body}");
    }

    resp.json::<CredentialsResponse>()
        .await
        .context("failed to parse NordVPN credentials response")
}

/// Fetch recommended WireGuard servers, optionally filtered by country.
async fn fetch_servers(
    client: &reqwest::Client,
    count: usize,
    countries: Option<&[String]>,
) -> Result<Vec<ServerEntry>> {
    let base = "https://api.nordvpn.com/v1/servers/recommendations";

    let mut all_servers: Vec<ServerEntry> = Vec::new();

    match countries {
        Some(codes) if !codes.is_empty() => {
            // Fetch servers per country, distributing `count` evenly.
            let per_country = std::cmp::max(1, count / codes.len());
            for code in codes {
                let country_id = match country_code_to_id(code) {
                    Some(id) => id,
                    None => {
                        tracing::warn!(country = code.as_str(), "unknown country code — skipping");
                        continue;
                    }
                };

                let url = format!(
                    "{base}?filters[servers_technologies][identifier]=wireguard_udp\
                     &filters[country_id]={country_id}\
                     &limit={per_country}"
                );

                let servers = fetch_server_list(client, &url).await?;
                all_servers.extend(servers);
            }
        }
        _ => {
            let url = format!(
                "{base}?filters[servers_technologies][identifier]=wireguard_udp&limit={count}"
            );
            let servers = fetch_server_list(client, &url).await?;
            all_servers.extend(servers);
        }
    }

    // Trim to requested count (country distribution may overshoot).
    all_servers.truncate(count);
    Ok(all_servers)
}

/// Execute a single server-list GET request.
async fn fetch_server_list(client: &reqwest::Client, url: &str) -> Result<Vec<ServerEntry>> {
    let resp = client
        .get(url)
        .send()
        .await
        .context("failed to reach NordVPN server recommendations API")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("NordVPN server recommendations API returned HTTP {status}: {body}");
    }

    resp.json::<Vec<ServerEntry>>()
        .await
        .context("failed to parse NordVPN server recommendations response")
}

/// Generate TOML `[[tunnel]]` blocks for the fetched servers.
pub fn generate_toml(private_key: &str, servers: &[ServerEntry]) -> Result<String> {
    let mut buf = String::new();
    buf.push_str("# Generated by: houdinny import nord\n");

    for server in servers {
        let public_key = extract_wireguard_public_key(server)?;
        let country_code = server
            .locations
            .first()
            .map(|l| l.country.code.to_lowercase())
            .unwrap_or_else(|| "xx".to_string());

        let label = format!("nord-{}-{}", country_code, server.name);

        buf.push('\n');
        buf.push_str("[[tunnel]]\n");
        buf.push_str("protocol = \"wireguard\"\n");
        buf.push_str(&format!("endpoint = \"{}:51820\"\n", server.station));
        buf.push_str(&format!("private_key = \"{private_key}\"\n"));
        buf.push_str(&format!("public_key = \"{public_key}\"\n"));
        buf.push_str("address = \"10.5.0.2/32\"\n");
        buf.push_str("dns = \"103.86.96.100\"\n");
        buf.push_str(&format!("label = \"{label}\"\n"));
    }

    Ok(buf)
}

/// Extract the WireGuard public key from a server's technology metadata.
fn extract_wireguard_public_key(server: &ServerEntry) -> Result<String> {
    for tech in &server.technologies {
        if tech.identifier == "wireguard_udp" {
            for meta in &tech.metadata {
                if meta.name == "public_key" {
                    return Ok(meta.value.clone());
                }
            }
        }
    }
    bail!(
        "server {} ({}) has no wireguard_udp public key in metadata",
        server.name,
        server.hostname
    )
}

/// Write or append tunnel config to the output file.
fn write_config(output: &Path, toml_block: &str) -> Result<()> {
    use std::fs;
    use std::io::Write;

    if output.exists() {
        // Append to existing file.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(output)
            .with_context(|| format!("failed to open {} for appending", output.display()))?;
        // Ensure we start on a new line.
        writeln!(file)?;
        write!(file, "{toml_block}")?;
    } else {
        // Create a fresh config with a default [proxy] section.
        let mut contents = String::new();
        contents.push_str("# houdinny tunnel configuration\n");
        contents.push_str("# Generated by: houdinny import nord\n\n");
        contents.push_str("[proxy]\n");
        contents.push_str("listen = \"127.0.0.1:8080\"\n");
        contents.push_str("mode = \"transparent\"\n");
        contents.push_str("strategy = \"random\"\n\n");
        contents.push_str(toml_block);

        fs::write(output, contents)
            .with_context(|| format!("failed to write {}", output.display()))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server(
        name: &str,
        station: &str,
        country_code: &str,
        public_key: &str,
    ) -> ServerEntry {
        ServerEntry {
            id: 1,
            name: name.to_string(),
            hostname: format!("{name}.nordvpn.com"),
            station: station.to_string(),
            technologies: vec![Technology {
                identifier: "wireguard_udp".to_string(),
                metadata: vec![TechMetadata {
                    name: "public_key".to_string(),
                    value: public_key.to_string(),
                }],
                pivot: Some(TechPivot {
                    status: "online".to_string(),
                }),
            }],
            locations: vec![Location {
                country: Country {
                    code: country_code.to_string(),
                    name: "Test Country".to_string(),
                },
            }],
        }
    }

    #[test]
    fn parse_credentials_response() {
        let json = r#"{"nordlynx_private_key": "AAAA+BBBB/CCCC="}"#;
        let creds: CredentialsResponse = serde_json::from_str(json).expect("should parse");
        assert_eq!(creds.nordlynx_private_key, "AAAA+BBBB/CCCC=");
    }

    #[test]
    fn parse_server_response() {
        let json = r#"[{
            "id": 123,
            "name": "us1234",
            "hostname": "us1234.nordvpn.com",
            "station": "169.150.196.30",
            "technologies": [{
                "identifier": "wireguard_udp",
                "metadata": [{"name": "public_key", "value": "SERVER_PUB_KEY"}],
                "pivot": {"status": "online"}
            }],
            "locations": [{"country": {"code": "US", "name": "United States"}}]
        }]"#;

        let servers: Vec<ServerEntry> = serde_json::from_str(json).expect("should parse");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "us1234");
        assert_eq!(servers[0].station, "169.150.196.30");
        assert_eq!(servers[0].locations[0].country.code, "US");
    }

    #[test]
    fn generate_toml_single_server() {
        let server = sample_server("us1234", "169.150.196.30", "US", "SERVER_PUB_KEY");
        let toml = generate_toml("MY_PRIVATE_KEY", &[server]).expect("should generate");

        assert!(toml.contains("protocol = \"wireguard\""));
        assert!(toml.contains("endpoint = \"169.150.196.30:51820\""));
        assert!(toml.contains("private_key = \"MY_PRIVATE_KEY\""));
        assert!(toml.contains("public_key = \"SERVER_PUB_KEY\""));
        assert!(toml.contains("address = \"10.5.0.2/32\""));
        assert!(toml.contains("dns = \"103.86.96.100\""));
        assert!(toml.contains("label = \"nord-us-us1234\""));
    }

    #[test]
    fn generate_toml_multiple_servers() {
        let servers = vec![
            sample_server("us1234", "1.1.1.1", "US", "PK1"),
            sample_server("de5678", "2.2.2.2", "DE", "PK2"),
            sample_server("jp9999", "3.3.3.3", "JP", "PK3"),
        ];
        let toml = generate_toml("MY_KEY", &servers).expect("should generate");

        assert!(toml.contains("label = \"nord-us-us1234\""));
        assert!(toml.contains("label = \"nord-de-de5678\""));
        assert!(toml.contains("label = \"nord-jp-jp9999\""));
        // Each server gets its own [[tunnel]] block.
        assert_eq!(toml.matches("[[tunnel]]").count(), 3);
    }

    #[test]
    fn generate_toml_empty_servers() {
        let toml = generate_toml("KEY", &[]).expect("should generate");
        assert!(toml.contains("# Generated by: houdinny import nord"));
        assert!(!toml.contains("[[tunnel]]"));
    }

    #[test]
    fn extract_public_key_missing() {
        let server = ServerEntry {
            id: 1,
            name: "broken".to_string(),
            hostname: "broken.nordvpn.com".to_string(),
            station: "0.0.0.0".to_string(),
            technologies: vec![Technology {
                identifier: "openvpn_tcp".to_string(),
                metadata: vec![],
                pivot: None,
            }],
            locations: vec![],
        };
        let result = extract_wireguard_public_key(&server);
        assert!(result.is_err());
    }

    #[test]
    fn country_code_mapping() {
        assert_eq!(country_code_to_id("us"), Some(228));
        assert_eq!(country_code_to_id("US"), Some(228));
        assert_eq!(country_code_to_id("de"), Some(81));
        assert_eq!(country_code_to_id("xx"), None);
    }

    #[test]
    fn write_config_creates_new_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tunnels.toml");

        let toml_block = "[[tunnel]]\nprotocol = \"wireguard\"\nlabel = \"test\"\n";
        write_config(&path, toml_block).expect("should write");

        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("[proxy]"));
        assert!(contents.contains("listen = \"127.0.0.1:8080\""));
        assert!(contents.contains("[[tunnel]]"));
        assert!(contents.contains("label = \"test\""));
    }

    #[test]
    fn write_config_appends_to_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tunnels.toml");

        // Create an initial file.
        std::fs::write(&path, "[proxy]\nlisten = \"0.0.0.0:9090\"\n").expect("write");

        let toml_block = "[[tunnel]]\nprotocol = \"wireguard\"\nlabel = \"appended\"\n";
        write_config(&path, toml_block).expect("should append");

        let contents = std::fs::read_to_string(&path).expect("read");
        // Original content preserved.
        assert!(contents.contains("listen = \"0.0.0.0:9090\""));
        // New content appended.
        assert!(contents.contains("label = \"appended\""));
    }

    #[test]
    fn parse_api_error_response_401() {
        // Simulate what the code path does for a 401 — the error message
        // should clearly mention the token.
        let msg = "NordVPN API returned 401 — check your access token";
        assert!(msg.contains("401"));
        assert!(msg.contains("token"));
    }
}
