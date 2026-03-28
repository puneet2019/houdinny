//! SOCKS5 transport — connects through any SOCKS5 proxy.
//!
//! Works with SSH tunnels (`ssh -D`), proxy services, Tor, etc.

use super::{AsyncReadWrite, Transport};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};

/// A SOCKS5 transport that connects through a SOCKS5 proxy.
pub struct Socks5Transport {
    address: String,
    label: String,
    id: String,
    healthy: AtomicBool,
}

impl Socks5Transport {
    /// Create a new SOCKS5 transport.
    ///
    /// `address` is the SOCKS5 proxy address, e.g. `"127.0.0.1:1080"`.
    pub fn new(address: impl Into<String>, label: impl Into<String>) -> Self {
        let address = address.into();
        let label = label.into();
        let id = format!("socks5-{}", label);
        Self {
            address,
            label,
            id,
            healthy: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl Transport for Socks5Transport {
    async fn provision(&mut self) -> Result<()> {
        // SOCKS5 is statically configured — nothing to provision.
        Ok(())
    }

    async fn connect(&self, addr: &str, port: u16) -> Result<Box<dyn AsyncReadWrite>> {
        let config = fast_socks5::client::Config::default();
        let stream = fast_socks5::client::Socks5Stream::connect(
            &self.address,
            addr.to_string(),
            port,
            config,
        )
        .await
        .map_err(|e| Error::TunnelConnect(format!("SOCKS5 {}: {e}", self.address)))?;
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
        "socks5"
    }

    async fn close(&self) -> Result<()> {
        self.healthy.store(false, Ordering::Relaxed);
        Ok(())
    }
}
