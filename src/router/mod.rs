//! Router module — picks which tunnel to use per request.
//!
//! The [`Router`] trait defines the selection interface, and the
//! [`Strategy`] enum lets callers choose between implementations
//! via configuration.

use std::sync::{Arc, atomic::AtomicUsize};

use rand::seq::IndexedRandom;

pub use crate::config::Strategy;
use crate::transport::Transport;

/// Selects a transport from the available pool for a given destination.
///
/// Implementations must be thread-safe (`Send + Sync`) because the proxy
/// server dispatches requests concurrently.
pub trait Router: Send + Sync {
    /// Pick a healthy transport for the given destination.
    ///
    /// Returns [`crate::error::Error::NoHealthyTunnels`] when no transport
    /// in `transports` reports itself as healthy.
    fn pick(
        &self,
        dest_host: &str,
        dest_port: u16,
        transports: &[Arc<dyn Transport>],
    ) -> crate::error::Result<Arc<dyn Transport>>;
}

impl Strategy {
    /// Construct a boxed [`Router`] matching this strategy.
    pub fn build_router(&self) -> Box<dyn Router> {
        match self {
            Strategy::Random => Box::new(RandomRouter),
            Strategy::RoundRobin => Box::new(RoundRobinRouter::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Filter the transport slice down to only healthy entries.
fn healthy_transports(transports: &[Arc<dyn Transport>]) -> Vec<Arc<dyn Transport>> {
    transports.iter().filter(|t| t.healthy()).cloned().collect()
}

// ---------------------------------------------------------------------------
// RandomRouter
// ---------------------------------------------------------------------------

/// Picks a random healthy transport for each request.
pub struct RandomRouter;

impl Router for RandomRouter {
    fn pick(
        &self,
        dest_host: &str,
        dest_port: u16,
        transports: &[Arc<dyn Transport>],
    ) -> crate::error::Result<Arc<dyn Transport>> {
        let healthy = healthy_transports(transports);
        let chosen = healthy
            .choose(&mut rand::rng())
            .ok_or(crate::error::Error::NoHealthyTunnels)?;

        tracing::debug!(
            transport_id = chosen.id(),
            dest_host,
            dest_port,
            strategy = "random",
            "picked transport"
        );

        Ok(Arc::clone(chosen))
    }
}

// ---------------------------------------------------------------------------
// RoundRobinRouter
// ---------------------------------------------------------------------------

/// Cycles through healthy transports in sequential order.
///
/// Uses an [`AtomicUsize`] counter so that concurrent callers advance the
/// position without needing a mutex.
pub struct RoundRobinRouter {
    counter: AtomicUsize,
}

impl RoundRobinRouter {
    /// Create a new round-robin router starting at index 0.
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
        }
    }
}

impl Default for RoundRobinRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl Router for RoundRobinRouter {
    fn pick(
        &self,
        dest_host: &str,
        dest_port: u16,
        transports: &[Arc<dyn Transport>],
    ) -> crate::error::Result<Arc<dyn Transport>> {
        let healthy = healthy_transports(transports);
        if healthy.is_empty() {
            return Err(crate::error::Error::NoHealthyTunnels);
        }

        let idx = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % healthy.len();
        let chosen = &healthy[idx];

        tracing::debug!(
            transport_id = chosen.id(),
            dest_host,
            dest_port,
            strategy = "round-robin",
            index = idx,
            "picked transport"
        );

        Ok(Arc::clone(chosen))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashSet;

    /// A mock transport with configurable health status.
    struct MockTransport {
        id: String,
        is_healthy: bool,
    }

    impl MockTransport {
        fn new(id: &str, is_healthy: bool) -> Self {
            Self {
                id: id.to_string(),
                is_healthy,
            }
        }

        fn arc(id: &str, is_healthy: bool) -> Arc<dyn Transport> {
            Arc::new(Self::new(id, is_healthy))
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn provision(&mut self) -> crate::error::Result<()> {
            Ok(())
        }

        async fn connect(
            &self,
            _addr: &str,
            _port: u16,
        ) -> crate::error::Result<Box<dyn crate::transport::AsyncReadWrite>> {
            Err(crate::error::Error::TunnelConnect(
                "mock transport".to_string(),
            ))
        }

        fn healthy(&self) -> bool {
            self.is_healthy
        }

        fn id(&self) -> &str {
            &self.id
        }

        fn label(&self) -> &str {
            &self.id
        }

        fn protocol(&self) -> &str {
            "mock"
        }

        async fn close(&self) -> crate::error::Result<()> {
            Ok(())
        }
    }

    // -- RandomRouter tests --

    #[test]
    fn random_router_returns_no_healthy_tunnels_on_empty_slice() {
        let router = RandomRouter;
        let result = router.pick("example.com", 443, &[]);
        assert!(matches!(result, Err(crate::error::Error::NoHealthyTunnels)));
    }

    #[test]
    fn random_router_picks_from_available_transports() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("a", true),
            MockTransport::arc("b", true),
            MockTransport::arc("c", true),
        ];

        let router = RandomRouter;
        let mut seen = HashSet::new();

        for _ in 0..100 {
            let t = router.pick("example.com", 443, &transports).unwrap();
            seen.insert(t.id().to_string());
        }

        // With 3 transports and 100 trials the probability of always
        // picking the same one is (1/3)^99 — effectively zero.
        assert!(
            seen.len() > 1,
            "expected more than one distinct transport over 100 picks, got: {seen:?}"
        );
    }

    #[test]
    fn random_router_skips_unhealthy_transports() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("dead1", false),
            MockTransport::arc("alive", true),
            MockTransport::arc("dead2", false),
        ];

        let router = RandomRouter;

        for _ in 0..50 {
            let t = router.pick("example.com", 443, &transports).unwrap();
            assert_eq!(t.id(), "alive");
        }
    }

    #[test]
    fn random_router_returns_error_when_all_unhealthy() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("dead1", false),
            MockTransport::arc("dead2", false),
        ];

        let router = RandomRouter;
        let result = router.pick("example.com", 443, &transports);
        assert!(matches!(result, Err(crate::error::Error::NoHealthyTunnels)));
    }

    // -- RoundRobinRouter tests --

    #[test]
    fn round_robin_returns_no_healthy_tunnels_on_empty_slice() {
        let router = RoundRobinRouter::new();
        let result = router.pick("example.com", 443, &[]);
        assert!(matches!(result, Err(crate::error::Error::NoHealthyTunnels)));
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("a", true),
            MockTransport::arc("b", true),
            MockTransport::arc("c", true),
        ];

        let router = RoundRobinRouter::new();

        // Two full cycles to prove wrapping works.
        let expected = ["a", "b", "c", "a", "b", "c"];
        for exp in &expected {
            let t = router.pick("example.com", 443, &transports).unwrap();
            assert_eq!(t.id(), *exp);
        }
    }

    #[test]
    fn round_robin_skips_unhealthy_transports() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("dead", false),
            MockTransport::arc("alive1", true),
            MockTransport::arc("alive2", true),
        ];

        let router = RoundRobinRouter::new();

        // Only the two healthy ones should appear, in order.
        let t1 = router.pick("example.com", 443, &transports).unwrap();
        let t2 = router.pick("example.com", 443, &transports).unwrap();
        let t3 = router.pick("example.com", 443, &transports).unwrap();

        assert_eq!(t1.id(), "alive1");
        assert_eq!(t2.id(), "alive2");
        assert_eq!(t3.id(), "alive1"); // wraps around
    }

    #[test]
    fn round_robin_returns_error_when_all_unhealthy() {
        let transports: Vec<Arc<dyn Transport>> = vec![
            MockTransport::arc("dead1", false),
            MockTransport::arc("dead2", false),
        ];

        let router = RoundRobinRouter::new();
        let result = router.pick("example.com", 443, &transports);
        assert!(matches!(result, Err(crate::error::Error::NoHealthyTunnels)));
    }

    // -- Strategy tests --

    #[test]
    fn strategy_default_is_random() {
        let s = Strategy::default();
        assert!(matches!(s, Strategy::Random));
    }

    #[test]
    fn strategy_build_router_returns_working_router() {
        let transports: Vec<Arc<dyn Transport>> = vec![MockTransport::arc("x", true)];

        let random = Strategy::Random.build_router();
        assert_eq!(random.pick("h", 80, &transports).unwrap().id(), "x");

        let rr = Strategy::RoundRobin.build_router();
        assert_eq!(rr.pick("h", 80, &transports).unwrap().id(), "x");
    }

    #[test]
    fn strategy_serde_round_trip() {
        // TOML requires a table at the top level, so wrap in a struct.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrapper {
            strategy: Strategy,
        }

        let w = Wrapper {
            strategy: Strategy::RoundRobin,
        };
        let serialized = toml::to_string(&w).expect("serialize");
        assert!(serialized.contains("round-robin"));

        let deserialized: Wrapper = toml::from_str(&serialized).expect("deserialize");
        assert!(matches!(deserialized.strategy, Strategy::RoundRobin));
    }
}
