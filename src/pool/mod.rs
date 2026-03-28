//! Tunnel pool — manages multiple transports behind thread-safe interior mutability.
//!
//! The [`Pool`] holds `Arc<dyn Transport>` instances and provides methods to
//! add, remove, query, and filter transports at runtime. All operations are
//! safe to call from multiple tasks concurrently.

use crate::transport::Transport;
use std::fmt;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// A thread-safe pool of tunnel transports.
///
/// Transports are stored behind an `Arc<RwLock<...>>` so the pool can be
/// shared across tasks and mutated at runtime (adding/removing tunnels).
///
/// # Examples
///
/// ```no_run
/// # #[tokio::main]
/// # async fn main() {
/// use houdinny::pool::Pool;
///
/// // Create an empty pool and add transports later.
/// let pool = Pool::new(vec![]);
/// assert!(pool.is_empty().await);
/// # }
/// ```
pub struct Pool {
    transports: Arc<RwLock<Vec<Arc<dyn Transport>>>>,
}

impl Pool {
    /// Create a new pool from a list of transports.
    ///
    /// Pass an empty `Vec` to start with no transports and add them later
    /// via [`Pool::add`].
    pub fn new(transports: Vec<Arc<dyn Transport>>) -> Self {
        info!(count = transports.len(), "pool created");
        Self {
            transports: Arc::new(RwLock::new(transports)),
        }
    }

    /// Add a transport to the pool at runtime.
    pub async fn add(&self, transport: Arc<dyn Transport>) {
        let id = transport.id().to_owned();
        let label = transport.label().to_owned();
        let mut guard = self.transports.write().await;
        guard.push(transport);
        info!(id = %id, label = %label, total = guard.len(), "transport added to pool");
    }

    /// Remove a transport by its ID.
    ///
    /// Returns `true` if a transport with the given ID was found and removed,
    /// `false` otherwise.
    pub async fn remove(&self, id: &str) -> bool {
        let mut guard = self.transports.write().await;
        let before = guard.len();
        guard.retain(|t| t.id() != id);
        let removed = guard.len() < before;
        if removed {
            info!(id = %id, remaining = guard.len(), "transport removed from pool");
        } else {
            warn!(id = %id, "attempted to remove unknown transport");
        }
        removed
    }

    /// Return only healthy transports.
    ///
    /// Logs a warning for each unhealthy transport encountered.
    pub async fn healthy_transports(&self) -> Vec<Arc<dyn Transport>> {
        let guard = self.transports.read().await;
        Self::filter_healthy(&guard)
    }

    /// Synchronous version of [`Pool::healthy_transports`] for use inside
    /// non-async closures (e.g. the proxy picker).
    ///
    /// Uses [`RwLock::try_read`] to avoid blocking the runtime. Returns an
    /// empty list if the lock is currently held by a writer.
    pub fn healthy_transports_sync(&self) -> Vec<Arc<dyn Transport>> {
        match self.transports.try_read() {
            Ok(guard) => Self::filter_healthy(&guard),
            Err(_) => {
                warn!("pool lock contended — returning empty transport list");
                Vec::new()
            }
        }
    }

    /// Shared filter logic for healthy transport extraction.
    fn filter_healthy(transports: &[Arc<dyn Transport>]) -> Vec<Arc<dyn Transport>> {
        let mut healthy = Vec::new();
        for t in transports.iter() {
            if t.healthy() {
                healthy.push(Arc::clone(t));
            } else {
                warn!(id = %t.id(), label = %t.label(), "unhealthy transport skipped");
            }
        }
        healthy
    }

    /// Look up a transport by its ID.
    pub async fn get(&self, id: &str) -> Option<Arc<dyn Transport>> {
        let guard = self.transports.read().await;
        guard.iter().find(|t| t.id() == id).map(Arc::clone)
    }

    /// Return the number of transports in the pool (healthy or not).
    pub async fn len(&self) -> usize {
        self.transports.read().await.len()
    }

    /// Return `true` if the pool contains no transports.
    pub async fn is_empty(&self) -> bool {
        self.transports.read().await.is_empty()
    }

    /// Return all transports in the pool.
    pub async fn all(&self) -> Vec<Arc<dyn Transport>> {
        self.transports
            .read()
            .await
            .iter()
            .map(Arc::clone)
            .collect()
    }

    /// Return a display-friendly status summary of the pool.
    ///
    /// Because reading the pool is async, [`Display`] cannot be used directly.
    /// Call this method to get a pre-formatted summary string.
    pub async fn status_summary(&self) -> PoolStatus {
        let guard = self.transports.read().await;
        let total = guard.len();
        let mut healthy_count = 0usize;
        let mut labels = Vec::with_capacity(total);
        for t in guard.iter() {
            if t.healthy() {
                healthy_count += 1;
            }
            labels.push(t.label().to_owned());
        }
        PoolStatus {
            total,
            healthy: healthy_count,
            labels,
        }
    }
}

/// A snapshot of pool status, suitable for display.
///
/// Obtained via [`Pool::status_summary`].
#[derive(Debug, Clone)]
pub struct PoolStatus {
    /// Total number of transports.
    pub total: usize,
    /// Number of healthy transports.
    pub healthy: usize,
    /// Labels of all transports.
    pub labels: Vec<String>,
}

impl fmt::Display for PoolStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Pool({} total, {} healthy, labels=[{}])",
            self.total,
            self.healthy,
            self.labels.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::transport::AsyncReadWrite;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A mock transport for testing the pool.
    struct MockTransport {
        id: String,
        label: String,
        healthy: AtomicBool,
    }

    impl MockTransport {
        fn new(id: impl Into<String>, label: impl Into<String>, is_healthy: bool) -> Self {
            Self {
                id: id.into(),
                label: label.into(),
                healthy: AtomicBool::new(is_healthy),
            }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn provision(&mut self) -> Result<()> {
            Ok(())
        }

        async fn connect(&self, _addr: &str, _port: u16) -> Result<Box<dyn AsyncReadWrite>> {
            Err(crate::error::Error::TunnelConnect("mock transport".into()))
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
            "mock"
        }

        async fn close(&self) -> Result<()> {
            self.healthy.store(false, Ordering::Relaxed);
            Ok(())
        }
    }

    fn mock(id: &str, label: &str, healthy: bool) -> Arc<dyn Transport> {
        Arc::new(MockTransport::new(id, label, healthy))
    }

    #[tokio::test]
    async fn empty_pool() {
        let pool = Pool::new(vec![]);
        assert!(pool.is_empty().await);
        assert_eq!(pool.len().await, 0);
        assert!(pool.all().await.is_empty());
        assert!(pool.healthy_transports().await.is_empty());
    }

    #[tokio::test]
    async fn add_and_len() {
        let pool = Pool::new(vec![]);
        pool.add(mock("t1", "tunnel-1", true)).await;
        pool.add(mock("t2", "tunnel-2", true)).await;
        assert_eq!(pool.len().await, 2);
        assert!(!pool.is_empty().await);
    }

    #[tokio::test]
    async fn remove_existing() {
        let pool = Pool::new(vec![mock("a", "alpha", true), mock("b", "beta", true)]);
        assert!(pool.remove("a").await);
        assert_eq!(pool.len().await, 1);
        assert!(pool.get("a").await.is_none());
        assert!(pool.get("b").await.is_some());
    }

    #[tokio::test]
    async fn remove_nonexistent() {
        let pool = Pool::new(vec![mock("a", "alpha", true)]);
        assert!(!pool.remove("zzz").await);
        assert_eq!(pool.len().await, 1);
    }

    #[tokio::test]
    async fn healthy_filters_correctly() {
        let pool = Pool::new(vec![
            mock("h1", "healthy-1", true),
            mock("u1", "unhealthy-1", false),
            mock("h2", "healthy-2", true),
            mock("u2", "unhealthy-2", false),
        ]);

        let healthy = pool.healthy_transports().await;
        assert_eq!(healthy.len(), 2);

        let ids: Vec<&str> = healthy.iter().map(|t| t.id()).collect();
        assert!(ids.contains(&"h1"));
        assert!(ids.contains(&"h2"));
        assert!(!ids.contains(&"u1"));
        assert!(!ids.contains(&"u2"));
    }

    #[tokio::test]
    async fn get_by_id() {
        let pool = Pool::new(vec![
            mock("x", "x-label", true),
            mock("y", "y-label", false),
        ]);

        let x = pool.get("x").await;
        assert!(x.is_some());
        assert_eq!(x.as_ref().map(|t| t.label()), Some("x-label"));

        let y = pool.get("y").await;
        assert!(y.is_some());
        assert_eq!(y.as_ref().map(|t| t.id()), Some("y"));

        assert!(pool.get("nope").await.is_none());
    }

    #[tokio::test]
    async fn all_returns_everything() {
        let pool = Pool::new(vec![mock("a", "alpha", true), mock("b", "beta", false)]);
        let all = pool.all().await;
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn status_summary_display() {
        let pool = Pool::new(vec![
            mock("a", "alpha", true),
            mock("b", "beta", false),
            mock("c", "gamma", true),
        ]);

        let status = pool.status_summary().await;
        assert_eq!(status.total, 3);
        assert_eq!(status.healthy, 2);
        assert_eq!(status.labels, vec!["alpha", "beta", "gamma"]);

        let display = format!("{status}");
        assert!(display.contains("3 total"));
        assert!(display.contains("2 healthy"));
        assert!(display.contains("alpha"));
        assert!(display.contains("beta"));
        assert!(display.contains("gamma"));
    }

    #[tokio::test]
    async fn add_after_remove() {
        let pool = Pool::new(vec![mock("a", "alpha", true)]);
        assert!(pool.remove("a").await);
        assert!(pool.is_empty().await);

        pool.add(mock("b", "beta", true)).await;
        assert_eq!(pool.len().await, 1);
        assert_eq!(
            pool.get("b").await.map(|t| t.id().to_owned()),
            Some("b".to_owned())
        );
    }
}
