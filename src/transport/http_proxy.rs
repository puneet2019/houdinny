//! HTTP CONNECT proxy transport — connects through upstream HTTP proxies.
//!
//! Works with residential proxies (BrightData, Oxylabs), datacenter proxies,
//! corporate HTTP proxies, and any proxy that supports the `CONNECT` method.
//!
//! The protocol:
//! ```text
//! Client → CONNECT target:port HTTP/1.1\r\nHost: target:port\r\n\r\n → Proxy
//! Proxy  → HTTP/1.1 200 Connection Established\r\n\r\n             → Client
//! ```
//! After the 200, the TCP socket is a bidirectional tunnel.

use super::{AsyncReadWrite, Transport};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A transport that tunnels through an HTTP CONNECT proxy.
pub struct HttpProxyTransport {
    /// Proxy address (host:port).
    proxy_addr: String,
    /// Optional Basic auth credentials (username, password).
    auth: Option<(String, String)>,
    label: String,
    id: String,
    healthy: AtomicBool,
}

impl HttpProxyTransport {
    /// Create a new HTTP CONNECT proxy transport.
    ///
    /// `proxy_addr` is the proxy's `host:port`, e.g. `"proxy.brightdata.com:22225"`.
    /// `auth` is optional `(username, password)` for `Proxy-Authorization: Basic`.
    pub fn new(
        proxy_addr: impl Into<String>,
        auth: Option<(String, String)>,
        label: impl Into<String>,
    ) -> Self {
        let proxy_addr = proxy_addr.into();
        let label = label.into();
        let id = format!("http-proxy-{}", label);
        Self {
            proxy_addr,
            auth,
            label,
            id,
            healthy: AtomicBool::new(true),
        }
    }

    /// Create from a URL like `http://user:pass@host:port` or `http://host:port`.
    ///
    /// The scheme (`http://`) is stripped. If `user:pass@` is present, it becomes
    /// the Basic auth credentials.
    pub fn from_url(url: &str, label: impl Into<String>) -> Result<Self> {
        // Strip the scheme.
        let without_scheme = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .ok_or_else(|| {
                Error::Config(format!(
                    "http-proxy URL must start with http:// or https://: {url}"
                ))
            })?;

        // Remove any trailing slash.
        let without_scheme = without_scheme.trim_end_matches('/');

        if without_scheme.is_empty() {
            return Err(Error::Config(format!("http-proxy URL has no host: {url}")));
        }

        // Split auth from host: "user:pass@host:port" or "host:port".
        let (auth, host_port) = if let Some(at_pos) = without_scheme.rfind('@') {
            let cred = &without_scheme[..at_pos];
            let hp = &without_scheme[at_pos + 1..];
            if hp.is_empty() {
                return Err(Error::Config(format!(
                    "http-proxy URL has no host after @: {url}"
                )));
            }
            // Parse user:pass
            let (user, pass) = cred.split_once(':').ok_or_else(|| {
                Error::Config(format!(
                    "http-proxy URL auth must be user:pass, got: {cred}"
                ))
            })?;
            (Some((user.to_string(), pass.to_string())), hp.to_string())
        } else {
            (None, without_scheme.to_string())
        };

        // Validate that we have a port (or at least something parseable).
        if host_port.is_empty() {
            return Err(Error::Config(format!("http-proxy URL has no host: {url}")));
        }

        Ok(Self::new(host_port, auth, label))
    }

    /// Build the `Proxy-Authorization: Basic ...` header, or an empty string.
    fn auth_header(&self) -> String {
        match &self.auth {
            Some((user, pass)) => {
                use base64::Engine;
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
                format!("Proxy-Authorization: Basic {encoded}\r\n")
            }
            None => String::new(),
        }
    }
}

#[async_trait]
impl Transport for HttpProxyTransport {
    async fn provision(&mut self) -> Result<()> {
        // HTTP CONNECT proxies are statically configured — nothing to provision.
        Ok(())
    }

    async fn connect(&self, addr: &str, port: u16) -> Result<Box<dyn AsyncReadWrite>> {
        // 1. TCP connect to the proxy.
        let mut stream = TcpStream::connect(&self.proxy_addr).await.map_err(|e| {
            Error::TunnelConnect(format!(
                "HTTP CONNECT proxy {}: TCP connect failed: {e}",
                self.proxy_addr
            ))
        })?;

        // 2. Send the CONNECT request.
        let auth = self.auth_header();
        let connect_req =
            format!("CONNECT {addr}:{port} HTTP/1.1\r\nHost: {addr}:{port}\r\n{auth}\r\n");
        stream
            .write_all(connect_req.as_bytes())
            .await
            .map_err(|e| {
                Error::TunnelConnect(format!(
                    "HTTP CONNECT proxy {}: write CONNECT failed: {e}",
                    self.proxy_addr
                ))
            })?;

        // 3. Read the response until we see \r\n\r\n (end of HTTP headers).
        let mut buf = Vec::with_capacity(512);
        loop {
            let byte = stream.read_u8().await.map_err(|e| {
                Error::TunnelConnect(format!(
                    "HTTP CONNECT proxy {}: read response failed: {e}",
                    self.proxy_addr
                ))
            })?;
            buf.push(byte);

            // Check for end of headers.
            if buf.len() >= 4 && buf[buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }

            // Safety limit: HTTP response headers shouldn't exceed 8 KiB.
            if buf.len() > 8192 {
                return Err(Error::TunnelConnect(format!(
                    "HTTP CONNECT proxy {}: response headers too large (>8KiB)",
                    self.proxy_addr
                )));
            }
        }

        // 4. Parse the status line.
        let response = String::from_utf8_lossy(&buf);
        let status_line = response.lines().next().unwrap_or("");

        // Expect "HTTP/1.x 200 ..." or "HTTP/1.x 2xx ...".
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);

        if status_code != 200 {
            return Err(Error::TunnelConnect(format!(
                "HTTP CONNECT proxy {} returned status {status_code}: {status_line}",
                self.proxy_addr,
            )));
        }

        // 5. The stream is now a bidirectional TCP tunnel.
        Ok(Box::new(stream))
    }

    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn protocol(&self) -> &str {
        "http-proxy"
    }

    async fn close(&self) -> Result<()> {
        self.healthy.store(false, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_url_with_auth() {
        let t =
            HttpProxyTransport::from_url("http://user:pass@proxy.brightdata.com:22225", "bright")
                .unwrap();
        assert_eq!(t.proxy_addr, "proxy.brightdata.com:22225");
        assert_eq!(t.auth, Some(("user".to_string(), "pass".to_string())));
        assert_eq!(t.label, "bright");
    }

    #[test]
    fn from_url_without_auth() {
        let t = HttpProxyTransport::from_url("http://proxy.example.com:8080", "dc").unwrap();
        assert_eq!(t.proxy_addr, "proxy.example.com:8080");
        assert_eq!(t.auth, None);
        assert_eq!(t.label, "dc");
    }

    #[test]
    fn from_url_with_trailing_slash() {
        let t = HttpProxyTransport::from_url("http://user:secret@proxy.example.com:3128/", "corp")
            .unwrap();
        assert_eq!(t.proxy_addr, "proxy.example.com:3128");
        assert_eq!(t.auth, Some(("user".to_string(), "secret".to_string())));
    }

    #[test]
    fn from_url_https_scheme() {
        let t = HttpProxyTransport::from_url("https://proxy.example.com:443", "tls").unwrap();
        assert_eq!(t.proxy_addr, "proxy.example.com:443");
        assert_eq!(t.auth, None);
    }

    #[test]
    fn from_url_invalid_no_scheme() {
        let result = HttpProxyTransport::from_url("proxy.example.com:8080", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_invalid_empty_host() {
        let result = HttpProxyTransport::from_url("http://", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_invalid_empty_host_after_at() {
        let result = HttpProxyTransport::from_url("http://user:pass@", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_invalid_auth_no_colon() {
        let result = HttpProxyTransport::from_url("http://useronly@proxy.example.com:8080", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_without_port() {
        let t = HttpProxyTransport::from_url("http://proxy.example.com", "noport").unwrap();
        assert_eq!(t.proxy_addr, "proxy.example.com");
        assert_eq!(t.auth, None);
    }

    #[test]
    fn auth_header_with_credentials() {
        let t = HttpProxyTransport::new(
            "proxy:8080",
            Some(("user".to_string(), "pass".to_string())),
            "test",
        );
        let header = t.auth_header();
        // base64("user:pass") = "dXNlcjpwYXNz"
        assert_eq!(header, "Proxy-Authorization: Basic dXNlcjpwYXNz\r\n");
    }

    #[test]
    fn auth_header_without_credentials() {
        let t = HttpProxyTransport::new("proxy:8080", None, "test");
        let header = t.auth_header();
        assert_eq!(header, "");
    }

    #[test]
    fn protocol_returns_http_proxy() {
        let t = HttpProxyTransport::new("proxy:8080", None, "test");
        assert_eq!(t.protocol(), "http-proxy");
    }

    #[test]
    fn id_includes_label() {
        let t = HttpProxyTransport::new("proxy:8080", None, "my-proxy");
        assert_eq!(t.id(), "http-proxy-my-proxy");
    }

    #[test]
    fn label_accessor() {
        let t = HttpProxyTransport::new("proxy:8080", None, "lbl");
        assert_eq!(t.label(), "lbl");
    }

    #[test]
    fn healthy_default_true() {
        let t = HttpProxyTransport::new("proxy:8080", None, "test");
        assert!(t.healthy());
    }
}
