//! HTTP proxy server — the front door for agent traffic.
//!
//! Handles both **HTTP CONNECT** (HTTPS tunneling) and **plain HTTP** forwarding.
//! Agents connect by setting `HTTP_PROXY` / `HTTPS_PROXY` to the listen address.
//!
//! # Architecture
//!
//! ```text
//! Agent ──HTTP CONNECT──→ ProxyServer ──picker()──→ Transport ──→ destination
//! Agent ──plain GET───→ ProxyServer ──picker()──→ Transport ──→ destination
//! ```
//!
//! For CONNECT: transparent TCP tunnel via `copy_bidirectional`.
//! For plain HTTP: rewrite request line to relative path, forward through tunnel.

use crate::error::{Error, Result};
use crate::transport::Transport;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info};

/// HTTP proxy server that agents connect to.
///
/// Listens on a TCP address and dispatches each incoming connection
/// to a tunnel obtained from the `picker` closure.
pub struct ProxyServer {
    listen_addr: SocketAddr,
}

impl ProxyServer {
    /// Create a new proxy server bound to the given address.
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self { listen_addr }
    }

    /// Run the proxy server, accepting connections until the process exits.
    ///
    /// The `picker` closure is called for every new connection with the
    /// destination `(host, port)` and must return a [`Transport`] to use
    /// for that connection.
    pub async fn run<F>(self, picker: F) -> Result<()>
    where
        F: Fn(&str, u16) -> Result<Arc<dyn Transport>> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(self.listen_addr).await?;
        let actual_addr = listener.local_addr()?;
        info!(addr = %actual_addr, "proxy server listening");

        let picker = Arc::new(picker);

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            info!(peer = %peer_addr, "new connection");

            let picker_ref = Arc::clone(&picker);
            tokio::spawn(async move {
                let picker: &F = &picker_ref;
                if let Err(e) = handle_connection(stream, peer_addr, picker).await {
                    error!(peer = %peer_addr, error = %e, "connection failed");
                }
            });
        }
    }
}

/// Handle a single incoming connection — determine if CONNECT or plain HTTP.
async fn handle_connection<F>(stream: TcpStream, peer_addr: SocketAddr, picker: &F) -> Result<()>
where
    F: Fn(&str, u16) -> Result<Arc<dyn Transport>> + Send + Sync,
{
    let mut reader = BufReader::new(stream);

    // Read the first line to determine the request type.
    let mut first_line = String::new();
    reader.read_line(&mut first_line).await?;
    let first_line = first_line.trim_end().to_string();

    if first_line.is_empty() {
        return Err(Error::Proxy("empty request line".into()));
    }

    debug!(peer = %peer_addr, line = %first_line, "request line");

    if first_line.to_uppercase().starts_with("CONNECT ") {
        handle_connect(reader, peer_addr, &first_line, picker).await
    } else {
        handle_plain_http(reader, peer_addr, &first_line, picker).await
    }
}

/// Handle an HTTP CONNECT request (HTTPS tunneling).
///
/// 1. Parse host + port from `CONNECT host:port HTTP/1.1`
/// 2. Read and discard remaining headers
/// 3. Pick a transport and connect through the tunnel
/// 4. Reply `200 Connection Established`
/// 5. Relay bytes bidirectionally
async fn handle_connect<F>(
    mut reader: BufReader<TcpStream>,
    peer_addr: SocketAddr,
    first_line: &str,
    picker: &F,
) -> Result<()>
where
    F: Fn(&str, u16) -> Result<Arc<dyn Transport>> + Send + Sync,
{
    let (host, port) = parse_connect(first_line)?;
    info!(peer = %peer_addr, host = %host, port = port, "CONNECT request");

    // Read and discard remaining headers until empty line.
    let mut header_line = String::new();
    loop {
        header_line.clear();
        reader.read_line(&mut header_line).await?;
        if header_line.trim().is_empty() {
            break;
        }
    }

    // Pick a transport for this destination.
    let transport = picker(&host, port)?;
    info!(
        peer = %peer_addr,
        host = %host,
        port = port,
        tunnel = %transport.id(),
        "tunnel selected for CONNECT"
    );

    // Connect through the tunnel.
    let mut tunnel_stream = transport.connect(&host, port).await?;
    info!(peer = %peer_addr, host = %host, port = port, "tunnel connected");

    // Send 200 Connection Established back to the agent.
    let response = b"HTTP/1.1 200 Connection Established\r\n\r\n";
    reader.get_mut().write_all(response).await?;

    // Relay bytes bidirectionally.
    let mut agent_stream = reader.into_inner();
    match tokio::io::copy_bidirectional(&mut agent_stream, &mut *tunnel_stream).await {
        Ok((agent_to_tunnel, tunnel_to_agent)) => {
            debug!(
                peer = %peer_addr,
                host = %host,
                port = port,
                agent_to_tunnel = agent_to_tunnel,
                tunnel_to_agent = tunnel_to_agent,
                "CONNECT relay finished"
            );
        }
        Err(e) => {
            // Connection reset / broken pipe is normal when either side closes.
            debug!(
                peer = %peer_addr,
                host = %host,
                port = port,
                error = %e,
                "CONNECT relay ended"
            );
        }
    }

    Ok(())
}

/// Handle a plain HTTP request (non-CONNECT).
///
/// 1. Parse method, host, port, path from the request line
/// 2. Read all headers
/// 3. Pick a transport and connect through the tunnel
/// 4. Rewrite the request line to a relative path
/// 5. Forward the request and relay the response
async fn handle_plain_http<F>(
    mut reader: BufReader<TcpStream>,
    peer_addr: SocketAddr,
    first_line: &str,
    picker: &F,
) -> Result<()>
where
    F: Fn(&str, u16) -> Result<Arc<dyn Transport>> + Send + Sync,
{
    let (method, host, port, path) = parse_http_request(first_line)?;
    info!(
        peer = %peer_addr,
        method = %method,
        host = %host,
        port = port,
        path = %path,
        "plain HTTP request"
    );

    // Read remaining headers.
    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
        // Track Content-Length for body forwarding.
        if let Some(val) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
            && let Ok(len) = val.trim().parse::<usize>()
        {
            content_length = len;
        }
        headers.push(line);
    }

    // Pick a transport.
    let transport = picker(&host, port)?;
    info!(
        peer = %peer_addr,
        host = %host,
        port = port,
        tunnel = %transport.id(),
        "tunnel selected for HTTP"
    );

    // Connect through the tunnel.
    let mut tunnel_stream = transport.connect(&host, port).await?;

    // Rewrite request line: GET /path HTTP/1.1
    let rewritten_line = format!("{method} {path} HTTP/1.1\r\n");
    tunnel_stream.write_all(rewritten_line.as_bytes()).await?;

    // Forward headers.
    for header in &headers {
        tunnel_stream.write_all(header.as_bytes()).await?;
    }
    // End of headers.
    tunnel_stream.write_all(b"\r\n").await?;

    // Forward body if present.
    if content_length > 0 {
        let mut body_buf = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body_buf).await?;
        tunnel_stream.write_all(&body_buf).await?;
    }

    tunnel_stream.flush().await?;

    // Relay the response back to the agent.
    // For plain HTTP we relay everything the server sends until the server closes.
    let mut agent_stream = reader.into_inner();
    match tokio::io::copy(&mut *tunnel_stream, &mut agent_stream).await {
        Ok(bytes) => {
            debug!(
                peer = %peer_addr,
                host = %host,
                port = port,
                bytes = bytes,
                "HTTP response relayed"
            );
        }
        Err(e) => {
            debug!(
                peer = %peer_addr,
                host = %host,
                port = port,
                error = %e,
                "HTTP relay ended"
            );
        }
    }

    Ok(())
}

/// Parse a CONNECT request line into `(host, port)`.
///
/// Expected format: `CONNECT host:port HTTP/1.1`
///
/// # Examples
///
/// ```
/// # use houdinny::proxy::parse_connect;
/// let (host, port) = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();
/// assert_eq!(host, "example.com");
/// assert_eq!(port, 443);
/// ```
pub fn parse_connect(line: &str) -> Result<(String, u16)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(Error::Proxy(format!("malformed CONNECT line: {line}")));
    }

    if !parts[0].eq_ignore_ascii_case("CONNECT") {
        return Err(Error::Proxy(format!(
            "expected CONNECT method, got: {}",
            parts[0]
        )));
    }

    let authority = parts[1];
    parse_host_port(authority)
}

/// Parse an absolute-form HTTP request line into `(method, host, port, path)`.
///
/// Expected format: `METHOD http://host[:port]/path HTTP/1.1`
///
/// If no port is specified, defaults to 80 for http and 443 for https.
/// If no path is specified, defaults to `/`.
///
/// # Examples
///
/// ```
/// # use houdinny::proxy::parse_http_request;
/// let (method, host, port, path) = parse_http_request("GET http://example.com/path HTTP/1.1").unwrap();
/// assert_eq!(method, "GET");
/// assert_eq!(host, "example.com");
/// assert_eq!(port, 80);
/// assert_eq!(path, "/path");
/// ```
pub fn parse_http_request(line: &str) -> Result<(String, String, u16, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(Error::Proxy(format!("malformed HTTP request line: {line}")));
    }

    let method = parts[0].to_string();
    let url = parts[1];

    // Determine scheme and default port.
    let (without_scheme, default_port) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, 443u16)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, 80u16)
    } else {
        return Err(Error::Proxy(format!(
            "expected absolute URL (http:// or https://), got: {url}"
        )));
    };

    // Split into authority and path.
    let (authority, path) = match without_scheme.find('/') {
        Some(idx) => (&without_scheme[..idx], &without_scheme[idx..]),
        None => (without_scheme, "/"),
    };

    // Parse host and port from authority.
    let (host, port) = if authority.starts_with('[') {
        // IPv6: [::1]:port or [::1]
        parse_ipv6_authority(authority, default_port)?
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port = p
                    .parse::<u16>()
                    .map_err(|_| Error::Proxy(format!("invalid port in URL: {p}")))?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), default_port),
        }
    };

    if host.is_empty() {
        return Err(Error::Proxy(format!("empty host in URL: {url}")));
    }

    Ok((method, host, port, path.to_string()))
}

/// Parse `host:port` from a CONNECT authority string.
///
/// Handles both regular hostnames (`example.com:443`) and
/// IPv6 bracket notation (`[::1]:443`).
fn parse_host_port(authority: &str) -> Result<(String, u16)> {
    if authority.starts_with('[') {
        // IPv6: [::1]:port
        parse_ipv6_authority(authority, 0).and_then(|(host, port)| {
            if port == 0 {
                Err(Error::Proxy(format!(
                    "missing port in CONNECT authority: {authority}"
                )))
            } else {
                Ok((host, port))
            }
        })
    } else {
        match authority.rsplit_once(':') {
            Some((host, port_str)) => {
                let port = port_str
                    .parse::<u16>()
                    .map_err(|_| Error::Proxy(format!("invalid port: {port_str}")))?;
                if host.is_empty() {
                    return Err(Error::Proxy(format!(
                        "empty host in authority: {authority}"
                    )));
                }
                Ok((host.to_string(), port))
            }
            None => Err(Error::Proxy(format!(
                "missing port in CONNECT authority: {authority}"
            ))),
        }
    }
}

/// Parse an IPv6 authority like `[::1]:8080` or `[::1]`.
///
/// Returns `(host_without_brackets, port)`, using `default_port` when no
/// explicit port is present.
fn parse_ipv6_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    let closing_bracket = authority
        .find(']')
        .ok_or_else(|| Error::Proxy(format!("malformed IPv6 authority: {authority}")))?;

    let host = &authority[1..closing_bracket];
    let remainder = &authority[closing_bracket + 1..];

    let port = if let Some(port_str) = remainder.strip_prefix(':') {
        port_str
            .parse::<u16>()
            .map_err(|_| Error::Proxy(format!("invalid port in IPv6 authority: {port_str}")))?
    } else {
        default_port
    };

    if host.is_empty() {
        return Err(Error::Proxy(format!(
            "empty host in IPv6 authority: {authority}"
        )));
    }

    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_connect tests ──────────────────────────────────────────

    #[test]
    fn connect_standard_https() {
        let (host, port) = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn connect_custom_port() {
        let (host, port) = parse_connect("CONNECT example.com:8080 HTTP/1.1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);
    }

    #[test]
    fn connect_subdomain() {
        let (host, port) = parse_connect("CONNECT api.openai.com:443 HTTP/1.1").unwrap();
        assert_eq!(host, "api.openai.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn connect_ipv4() {
        let (host, port) = parse_connect("CONNECT 192.168.1.1:8443 HTTP/1.1").unwrap();
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 8443);
    }

    #[test]
    fn connect_ipv6() {
        let (host, port) = parse_connect("CONNECT [::1]:443 HTTP/1.1").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 443);
    }

    #[test]
    fn connect_ipv6_full() {
        let (host, port) = parse_connect("CONNECT [2001:db8::1]:8080 HTTP/1.1").unwrap();
        assert_eq!(host, "2001:db8::1");
        assert_eq!(port, 8080);
    }

    #[test]
    fn connect_lowercase() {
        let (host, port) = parse_connect("connect example.com:443 HTTP/1.1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn connect_garbage() {
        assert!(parse_connect("garbage").is_err());
    }

    #[test]
    fn connect_empty() {
        assert!(parse_connect("").is_err());
    }

    #[test]
    fn connect_missing_port() {
        assert!(parse_connect("CONNECT example.com HTTP/1.1").is_err());
    }

    #[test]
    fn connect_invalid_port() {
        assert!(parse_connect("CONNECT example.com:notaport HTTP/1.1").is_err());
    }

    #[test]
    fn connect_wrong_method() {
        assert!(parse_connect("GET example.com:443 HTTP/1.1").is_err());
    }

    #[test]
    fn connect_ipv6_missing_port() {
        assert!(parse_connect("CONNECT [::1] HTTP/1.1").is_err());
    }

    // ── parse_http_request tests ─────────────────────────────────────

    #[test]
    fn http_get_with_path() {
        let (method, host, port, path) =
            parse_http_request("GET http://example.com/path HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
        assert_eq!(path, "/path");
    }

    #[test]
    fn http_get_custom_port() {
        let (method, host, port, path) =
            parse_http_request("GET http://example.com:8080/path HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);
        assert_eq!(path, "/path");
    }

    #[test]
    fn http_post() {
        let (method, host, port, path) =
            parse_http_request("POST http://api.example.com/v1/data HTTP/1.1").unwrap();
        assert_eq!(method, "POST");
        assert_eq!(host, "api.example.com");
        assert_eq!(port, 80);
        assert_eq!(path, "/v1/data");
    }

    #[test]
    fn http_no_path() {
        let (method, host, port, path) =
            parse_http_request("GET http://example.com HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
        assert_eq!(path, "/");
    }

    #[test]
    fn http_no_path_with_port() {
        let (method, host, port, path) =
            parse_http_request("GET http://example.com:9090 HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(host, "example.com");
        assert_eq!(port, 9090);
        assert_eq!(path, "/");
    }

    #[test]
    fn http_deep_path() {
        let (_, _, _, path) =
            parse_http_request("GET http://example.com/a/b/c?q=1&r=2 HTTP/1.1").unwrap();
        assert_eq!(path, "/a/b/c?q=1&r=2");
    }

    #[test]
    fn http_https_scheme() {
        let (_, host, port, path) =
            parse_http_request("GET https://secure.example.com/api HTTP/1.1").unwrap();
        assert_eq!(host, "secure.example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/api");
    }

    #[test]
    fn http_ipv6() {
        let (_, host, port, path) =
            parse_http_request("GET http://[::1]:8080/test HTTP/1.1").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 8080);
        assert_eq!(path, "/test");
    }

    #[test]
    fn http_ipv6_default_port() {
        let (_, host, port, path) = parse_http_request("GET http://[::1]/test HTTP/1.1").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 80);
        assert_eq!(path, "/test");
    }

    #[test]
    fn http_missing_scheme() {
        assert!(parse_http_request("GET /path HTTP/1.1").is_err());
    }

    #[test]
    fn http_garbage() {
        assert!(parse_http_request("garbage").is_err());
    }

    #[test]
    fn http_empty() {
        assert!(parse_http_request("").is_err());
    }

    #[test]
    fn http_invalid_port() {
        assert!(parse_http_request("GET http://example.com:bad/path HTTP/1.1").is_err());
    }

    #[test]
    fn http_root_path_trailing_slash() {
        let (_, _, _, path) = parse_http_request("GET http://example.com/ HTTP/1.1").unwrap();
        assert_eq!(path, "/");
    }

    #[test]
    fn http_delete_method() {
        let (method, _, _, _) =
            parse_http_request("DELETE http://example.com/resource HTTP/1.1").unwrap();
        assert_eq!(method, "DELETE");
    }

    #[test]
    fn http_put_method() {
        let (method, _, _, _) =
            parse_http_request("PUT http://example.com/resource HTTP/1.1").unwrap();
        assert_eq!(method, "PUT");
    }
}
