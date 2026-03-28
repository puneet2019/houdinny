//! Transport trait and implementations.
//!
//! Each transport speaks a protocol (SOCKS5, WireGuard, Tor, etc.)
//! and provides tunnel connections through that protocol.

use crate::error::Result;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

/// A bidirectional async stream (read + write).
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncReadWrite for T {}

/// Transport plugin — bottom layer (tunnels).
///
/// Each implementation speaks one protocol. The pool holds
/// `Arc<dyn Transport>` and doesn't care which protocol is behind it.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Provision the tunnel (auth, payment, session setup).
    ///
    /// - SOCKS5/WireGuard: no-op (config is static)
    /// - Tor: builds circuit
    /// - Sentinel: pays on-chain, gets ephemeral credentials
    async fn provision(&mut self) -> Result<()>;

    /// Connect to a destination through this tunnel.
    async fn connect(&self, addr: &str, port: u16) -> Result<Box<dyn AsyncReadWrite>>;

    /// Is this tunnel alive and usable?
    fn healthy(&self) -> bool;

    /// Unique identifier for this tunnel instance.
    fn id(&self) -> &str;

    /// Human-readable label (from config).
    fn label(&self) -> &str;

    /// Which protocol this transport speaks.
    fn protocol(&self) -> &str;

    /// Tear down the tunnel and release resources.
    async fn close(&self) -> Result<()>;
}

pub mod http_proxy;

#[cfg(feature = "socks5")]
pub mod socks5;

pub mod sentinel;

#[cfg(feature = "tor")]
pub mod tor;

#[cfg(feature = "wireguard")]
pub mod wireguard;
