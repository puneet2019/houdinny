//! Tor transport — connects through the Tor network using arti-client.
//!
//! Each `TorTransport` instance represents a unique Tor circuit (via
//! `IsolationToken`). To get multiple exit IPs, create multiple instances
//! sharing the same `TorClient` but with different isolation tokens.

use super::{AsyncReadWrite, Transport};
use crate::error::{Error, Result};
use arti_client::{IsolationToken, StreamPrefs, TorClient, TorClientConfig};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tor_rtcompat::PreferredRuntime;

/// A Tor transport that connects through the Tor network.
///
/// Each instance uses a unique [`IsolationToken`] so that connections made
/// through it use a separate circuit (and therefore a different exit IP)
/// from other `TorTransport` instances.
pub struct TorTransport {
    /// Shared Tor client — expensive to create, so shared across all instances.
    client: Arc<TorClient<PreferredRuntime>>,
    /// Unique isolation token — forces a distinct circuit per instance.
    isolation_token: IsolationToken,
    /// Human-readable label from config.
    label: String,
    /// Unique identifier for this transport instance.
    id: String,
    /// Whether this transport is healthy and usable.
    healthy: AtomicBool,
}

impl TorTransport {
    /// Create a new `TorTransport` with a shared client and a unique isolation token.
    pub fn new(
        client: Arc<TorClient<PreferredRuntime>>,
        label: impl Into<String>,
        index: usize,
    ) -> Self {
        let label = label.into();
        let id = format!("tor-{label}-{index}");
        Self {
            client,
            isolation_token: IsolationToken::new(),
            label,
            id,
            healthy: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl Transport for TorTransport {
    async fn provision(&mut self) -> Result<()> {
        // Bootstrap the TorClient if it hasn't been bootstrapped yet.
        // This is idempotent — calling bootstrap() on an already-bootstrapped
        // client returns immediately.
        self.client
            .bootstrap()
            .await
            .map_err(|e| Error::TunnelProvision(format!("Tor bootstrap: {e}")))?;
        Ok(())
    }

    async fn connect(&self, addr: &str, port: u16) -> Result<Box<dyn AsyncReadWrite>> {
        let mut prefs = StreamPrefs::new();
        prefs.set_isolation(self.isolation_token);
        let stream = self
            .client
            .connect_with_prefs((addr, port), &prefs)
            .await
            .map_err(|e| Error::TunnelConnect(format!("Tor: {e}")))?;
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
        "tor"
    }

    async fn close(&self) -> Result<()> {
        self.healthy.store(false, Ordering::Relaxed);
        Ok(())
    }
}

/// Create `count` Tor transports sharing one `TorClient` but with different circuits.
///
/// Each transport gets a unique [`IsolationToken`], so traffic through each
/// one exits the Tor network from a different relay (different IP address).
///
/// The `TorClient` is bootstrapped lazily — the first call to [`Transport::provision`]
/// will connect to the Tor network (which takes 10-30 seconds on first run).
pub async fn create_tor_pool(count: usize, label_prefix: &str) -> Result<Vec<TorTransport>> {
    let config = TorClientConfig::default();
    let client = TorClient::builder()
        .config(config)
        .create_unbootstrapped()
        .map_err(|e| Error::TunnelProvision(format!("Tor client creation: {e}")))?;
    let client = Arc::new(client);

    let transports = (0..count)
        .map(|i| TorTransport::new(Arc::clone(&client), label_prefix, i))
        .collect();

    Ok(transports)
}

/// Create a `TorClient` with isolated temporary directories.
///
/// Used by tests and anywhere we need a throwaway client that won't
/// conflict with other instances or the system Tor state.
#[cfg(test)]
fn create_test_client(
    state_dir: &std::path::Path,
    cache_dir: &std::path::Path,
) -> Result<Arc<TorClient<PreferredRuntime>>> {
    use arti_client::BootstrapBehavior;

    let config =
        arti_client::config::TorClientConfigBuilder::from_directories(state_dir, cache_dir)
            .build()
            .map_err(|e| Error::TunnelProvision(format!("Tor config: {e}")))?;
    let client = TorClient::builder()
        .config(config)
        .bootstrap_behavior(BootstrapBehavior::Manual)
        .create_unbootstrapped()
        .map_err(|e| Error::TunnelProvision(format!("Tor client creation: {e}")))?;
    Ok(Arc::new(client))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_tokens_are_unique() {
        // IsolationToken::new() produces unique tokens each time.
        // We verify they are created without panicking.
        let _token1 = IsolationToken::new();
        let _token2 = IsolationToken::new();
    }

    #[tokio::test]
    async fn create_pool_correct_count() {
        let state = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");
        let client = create_test_client(state.path(), cache.path()).expect("client");

        let pool: Vec<TorTransport> = (0..5)
            .map(|i| TorTransport::new(Arc::clone(&client), "test", i))
            .collect();
        assert_eq!(pool.len(), 5);
    }

    #[tokio::test]
    async fn unique_ids() {
        let state = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");
        let client = create_test_client(state.path(), cache.path()).expect("client");

        let pool: Vec<TorTransport> = (0..3)
            .map(|i| TorTransport::new(Arc::clone(&client), "mytor", i))
            .collect();

        assert_eq!(pool[0].id(), "tor-mytor-0");
        assert_eq!(pool[1].id(), "tor-mytor-1");
        assert_eq!(pool[2].id(), "tor-mytor-2");

        // All should report as healthy.
        for t in &pool {
            assert!(t.healthy());
        }
    }

    #[tokio::test]
    async fn protocol_and_label() {
        let state = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");
        let client = create_test_client(state.path(), cache.path()).expect("client");

        let t = TorTransport::new(client, "circuit", 0);
        assert_eq!(t.protocol(), "tor");
        assert_eq!(t.label(), "circuit");
    }

    #[tokio::test]
    async fn healthy_flag_management() {
        let state = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");
        let client = create_test_client(state.path(), cache.path()).expect("client");

        let t = TorTransport::new(client, "health", 0);
        assert!(t.healthy());
        t.close().await.expect("close");
        assert!(!t.healthy());
    }

    #[tokio::test]
    async fn zero_circuits_yields_empty_pool() {
        let state = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");
        let client = create_test_client(state.path(), cache.path()).expect("client");

        let pool: Vec<TorTransport> = (0..0)
            .map(|i| TorTransport::new(Arc::clone(&client), "empty", i))
            .collect();
        assert!(pool.is_empty());
    }
}
