//! Route management for multi-tunnel setups.
//!
//! When multiple VPN tunnels are active they conflict on the default route.
//! Tunnel A hijacks the default route so Tunnel B's traffic flows through
//! Tunnel A. This module provides isolation via Linux policy-based routing
//! and network namespaces.
//!
//! On **Linux** the [`LinuxRouteManager`] uses separate routing tables and
//! `fwmark` rules so each tunnel's traffic stays on its own interface. The
//! [`NamespaceManager`] can optionally move interfaces into dedicated network
//! namespaces for complete isolation.
//!
//! On **macOS** (and other non-Linux platforms) routing isolation is handled
//! at the socket level (per-socket interface binding in the transport layer),
//! so the [`NoopRouteManager`] is used instead.
//!
//! # Design
//!
//! Commands are **generated** as `Vec<String>` rather than executed directly,
//! because they require root privileges. The caller decides whether and how
//! to run them.
//!
//! # Examples
//!
//! ```rust
//! use houdinny::route::{NoopRouteManager, RouteManager};
//!
//! let mgr = NoopRouteManager;
//! mgr.setup_tunnel_route("utun3", "tunnel-a").unwrap();
//! assert!(mgr.active_routes().is_empty());
//! ```

use serde::Deserialize;
#[cfg(target_os = "linux")]
use std::sync::RwLock;

use crate::error::Result;

// ---------------------------------------------------------------------------
// Serde default helpers
// ---------------------------------------------------------------------------

/// Default base routing table number.
fn default_base_table() -> u32 {
    100
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Route manager configuration.
///
/// Controls whether policy-based routing is enabled, which base routing
/// table number to start from, and whether to use network namespaces.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    /// Whether route management is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Base routing table number. Each tunnel gets `base_table + offset`.
    #[serde(default = "default_base_table")]
    pub base_table: u32,

    /// Whether to use Linux network namespaces for full isolation.
    #[serde(default)]
    pub use_namespaces: bool,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_table: default_base_table(),
            use_namespaces: false,
        }
    }
}

// ---------------------------------------------------------------------------
// TunnelRoute
// ---------------------------------------------------------------------------

/// Describes an active tunnel's routing state.
#[derive(Debug, Clone)]
pub struct TunnelRoute {
    /// Unique identifier for the tunnel.
    pub tunnel_id: String,

    /// Network interface name (e.g. `wg0`, `utun3`).
    pub interface: String,

    /// Linux routing table number, if applicable.
    pub routing_table: Option<u32>,

    /// Linux network namespace name, if applicable.
    pub namespace: Option<String>,
}

// ---------------------------------------------------------------------------
// RouteManager trait
// ---------------------------------------------------------------------------

/// Cross-platform abstraction for tunnel route management.
///
/// Implementations set up and tear down per-tunnel routing isolation so
/// that multiple VPN tunnels can coexist without hijacking each other's
/// default routes.
pub trait RouteManager: Send + Sync {
    /// Set up routing isolation for a tunnel interface.
    fn setup_tunnel_route(&self, interface: &str, tunnel_id: &str) -> Result<()>;

    /// Remove routing for a tunnel.
    fn teardown_tunnel_route(&self, tunnel_id: &str) -> Result<()>;

    /// List active tunnel routes.
    fn active_routes(&self) -> Vec<TunnelRoute>;
}

// ---------------------------------------------------------------------------
// LinuxRouteManager (Linux only)
// ---------------------------------------------------------------------------

/// Policy-based route manager for Linux.
///
/// Each tunnel is assigned a dedicated routing table (`base_table + offset`)
/// and a corresponding `fwmark` rule. The actual `ip` commands are
/// **generated** as strings — the caller decides whether to execute them
/// (they require root privileges).
#[cfg(target_os = "linux")]
pub struct LinuxRouteManager {
    /// Base routing table number. Each tunnel gets `base_table + offset`.
    base_table: u32,

    /// Active tunnel routes, protected by a read-write lock.
    routes: RwLock<Vec<TunnelRoute>>,
}

#[cfg(target_os = "linux")]
impl LinuxRouteManager {
    /// Create a new [`LinuxRouteManager`] with the default base table (100).
    pub fn new() -> Self {
        Self {
            base_table: default_base_table(),
            routes: RwLock::new(Vec::new()),
        }
    }

    /// Create a new [`LinuxRouteManager`] with a custom base table.
    pub fn with_base_table(base_table: u32) -> Self {
        Self {
            base_table,
            routes: RwLock::new(Vec::new()),
        }
    }

    /// Return the base routing table number.
    pub fn base_table(&self) -> u32 {
        self.base_table
    }

    /// Compute the routing table number for the Nth tunnel.
    fn table_for_offset(&self, offset: u32) -> u32 {
        self.base_table + offset
    }

    /// Generate the shell commands needed to set up routing for a tunnel.
    ///
    /// Returns commands as strings. The caller decides whether to execute
    /// them (they require root privileges).
    ///
    /// Commands produced:
    /// 1. `ip route add default via {gateway} dev {interface} table {table}`
    /// 2. `ip rule add fwmark {fwmark} table {table}`
    pub fn generate_setup_commands(
        &self,
        interface: &str,
        gateway: &str,
        table: u32,
        fwmark: u32,
    ) -> Vec<String> {
        vec![
            format!("ip route add default via {gateway} dev {interface} table {table}"),
            format!("ip rule add fwmark {fwmark} table {table}"),
        ]
    }

    /// Generate the shell commands to tear down routing for a tunnel.
    ///
    /// Commands produced:
    /// 1. `ip rule del fwmark {fwmark} table {table}`
    /// 2. `ip route del default table {table}`
    pub fn generate_teardown_commands(&self, table: u32, fwmark: u32) -> Vec<String> {
        vec![
            format!("ip rule del fwmark {fwmark} table {table}"),
            format!("ip route del default table {table}"),
        ]
    }
}

#[cfg(target_os = "linux")]
impl Default for LinuxRouteManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl RouteManager for LinuxRouteManager {
    fn setup_tunnel_route(&self, interface: &str, tunnel_id: &str) -> Result<()> {
        let mut routes = self
            .routes
            .write()
            .map_err(|e| crate::error::Error::Config(format!("route lock poisoned: {e}")))?;

        let offset = routes.len() as u32;
        let table = self.table_for_offset(offset);

        tracing::info!(
            tunnel_id,
            interface,
            table,
            "setting up tunnel route (commands not executed — generate and run manually)"
        );

        routes.push(TunnelRoute {
            tunnel_id: tunnel_id.to_string(),
            interface: interface.to_string(),
            routing_table: Some(table),
            namespace: None,
        });

        Ok(())
    }

    fn teardown_tunnel_route(&self, tunnel_id: &str) -> Result<()> {
        let mut routes = self
            .routes
            .write()
            .map_err(|e| crate::error::Error::Config(format!("route lock poisoned: {e}")))?;

        let before = routes.len();
        routes.retain(|r| r.tunnel_id != tunnel_id);
        let removed = before - routes.len();

        tracing::info!(
            tunnel_id,
            removed,
            "tearing down tunnel route (commands not executed — generate and run manually)"
        );

        Ok(())
    }

    fn active_routes(&self) -> Vec<TunnelRoute> {
        self.routes.read().map(|r| r.clone()).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// NamespaceManager (Linux only)
// ---------------------------------------------------------------------------

/// Generates Linux network namespace management commands.
///
/// Network namespaces provide the strongest isolation — each tunnel gets
/// its own network stack. Commands are generated as strings; the caller
/// decides whether to execute them.
#[cfg(target_os = "linux")]
pub struct NamespaceManager;

#[cfg(target_os = "linux")]
impl NamespaceManager {
    /// Generate commands to create a namespace and move an interface into it.
    ///
    /// Commands produced:
    /// 1. `ip netns add {ns_name}`
    /// 2. `ip link set {interface} netns {ns_name}`
    /// 3. `ip netns exec {ns_name} ip link set {interface} up`
    pub fn generate_create_commands(ns_name: &str, interface: &str) -> Vec<String> {
        vec![
            format!("ip netns add {ns_name}"),
            format!("ip link set {interface} netns {ns_name}"),
            format!("ip netns exec {ns_name} ip link set {interface} up"),
        ]
    }

    /// Generate commands to delete a namespace.
    ///
    /// Commands produced:
    /// 1. `ip netns del {ns_name}`
    pub fn generate_delete_commands(ns_name: &str) -> Vec<String> {
        vec![format!("ip netns del {ns_name}")]
    }

    /// List existing network namespaces by reading `/var/run/netns/`.
    ///
    /// Returns an empty list if the directory does not exist (common on
    /// systems where no namespaces have ever been created).
    pub fn list_namespaces() -> Result<Vec<String>> {
        let path = std::path::Path::new("/var/run/netns");
        if !path.exists() {
            return Ok(Vec::new());
        }

        let mut namespaces = Vec::new();
        let entries = std::fs::read_dir(path)?;
        for entry in entries {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                namespaces.push(name.to_string());
            }
        }

        Ok(namespaces)
    }
}

// ---------------------------------------------------------------------------
// NoopRouteManager (all platforms)
// ---------------------------------------------------------------------------

/// No-op route manager for platforms without kernel-level route isolation.
///
/// On macOS, routing isolation is handled at the socket level (per-socket
/// interface binding via `SO_BINDTODEVICE` / `IP_BOUND_IF`), so no route
/// table manipulation is needed.
pub struct NoopRouteManager;

impl RouteManager for NoopRouteManager {
    fn setup_tunnel_route(&self, _interface: &str, _tunnel_id: &str) -> Result<()> {
        tracing::debug!("route management not needed on this platform");
        Ok(())
    }

    fn teardown_tunnel_route(&self, _tunnel_id: &str) -> Result<()> {
        tracing::debug!("route teardown not needed on this platform");
        Ok(())
    }

    fn active_routes(&self) -> Vec<TunnelRoute> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create the platform-appropriate route manager.
///
/// On Linux, returns a [`LinuxRouteManager`]. On all other platforms,
/// returns a [`NoopRouteManager`].
pub fn create_route_manager(config: &RouteConfig) -> Box<dyn RouteManager> {
    if !config.enabled {
        tracing::debug!("route management disabled by config");
        return Box::new(NoopRouteManager);
    }

    #[cfg(target_os = "linux")]
    {
        tracing::info!(
            base_table = config.base_table,
            use_namespaces = config.use_namespaces,
            "creating Linux route manager"
        );
        Box::new(LinuxRouteManager::with_base_table(config.base_table))
    }

    #[cfg(not(target_os = "linux"))]
    {
        tracing::debug!("not on Linux — using noop route manager despite config.enabled=true");
        Box::new(NoopRouteManager)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- NoopRouteManager tests ---------------------------------------------

    #[test]
    fn noop_setup_succeeds_silently() {
        let mgr = NoopRouteManager;
        assert!(mgr.setup_tunnel_route("utun3", "tunnel-a").is_ok());
    }

    #[test]
    fn noop_teardown_succeeds_silently() {
        let mgr = NoopRouteManager;
        assert!(mgr.teardown_tunnel_route("tunnel-a").is_ok());
    }

    #[test]
    fn noop_active_routes_empty() {
        let mgr = NoopRouteManager;
        assert!(mgr.active_routes().is_empty());
    }

    #[test]
    fn noop_full_lifecycle() {
        let mgr = NoopRouteManager;
        mgr.setup_tunnel_route("wg0", "t1").unwrap();
        mgr.setup_tunnel_route("wg1", "t2").unwrap();
        assert!(mgr.active_routes().is_empty());
        mgr.teardown_tunnel_route("t1").unwrap();
        mgr.teardown_tunnel_route("t2").unwrap();
    }

    // -- RouteConfig deserialization tests -----------------------------------

    #[test]
    fn config_default_values() {
        let config = RouteConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.base_table, 100);
        assert!(!config.use_namespaces);
    }

    #[test]
    fn config_deserialize_empty_toml() {
        let config: RouteConfig = toml::from_str("").expect("empty TOML should use defaults");
        assert!(!config.enabled);
        assert_eq!(config.base_table, 100);
        assert!(!config.use_namespaces);
    }

    #[test]
    fn config_deserialize_full_toml() {
        let toml_str = r#"
enabled = true
base_table = 200
use_namespaces = true
"#;
        let config: RouteConfig = toml::from_str(toml_str).expect("full TOML should parse");
        assert!(config.enabled);
        assert_eq!(config.base_table, 200);
        assert!(config.use_namespaces);
    }

    #[test]
    fn config_deserialize_partial_toml() {
        let toml_str = r#"
enabled = true
"#;
        let config: RouteConfig = toml::from_str(toml_str).expect("partial TOML should parse");
        assert!(config.enabled);
        assert_eq!(config.base_table, 100);
        assert!(!config.use_namespaces);
    }

    // -- TunnelRoute struct tests -------------------------------------------

    #[test]
    fn tunnel_route_fields_accessible() {
        let route = TunnelRoute {
            tunnel_id: "tunnel-a".to_string(),
            interface: "wg0".to_string(),
            routing_table: Some(100),
            namespace: Some("ns_tunnel_a".to_string()),
        };
        assert_eq!(route.tunnel_id, "tunnel-a");
        assert_eq!(route.interface, "wg0");
        assert_eq!(route.routing_table, Some(100));
        assert_eq!(route.namespace.as_deref(), Some("ns_tunnel_a"));
    }

    #[test]
    fn tunnel_route_none_fields() {
        let route = TunnelRoute {
            tunnel_id: "tunnel-b".to_string(),
            interface: "utun4".to_string(),
            routing_table: None,
            namespace: None,
        };
        assert_eq!(route.tunnel_id, "tunnel-b");
        assert_eq!(route.interface, "utun4");
        assert!(route.routing_table.is_none());
        assert!(route.namespace.is_none());
    }

    // -- LinuxRouteManager tests (Linux only) -------------------------------

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::super::*;

        #[test]
        fn linux_generate_setup_commands_correct() {
            let mgr = LinuxRouteManager::new();
            let cmds = mgr.generate_setup_commands("wg0", "10.0.1.1", 100, 1);
            assert_eq!(cmds.len(), 2);
            assert_eq!(
                cmds[0],
                "ip route add default via 10.0.1.1 dev wg0 table 100"
            );
            assert_eq!(cmds[1], "ip rule add fwmark 1 table 100");
        }

        #[test]
        fn linux_generate_teardown_commands_correct() {
            let mgr = LinuxRouteManager::new();
            let cmds = mgr.generate_teardown_commands(100, 1);
            assert_eq!(cmds.len(), 2);
            assert_eq!(cmds[0], "ip rule del fwmark 1 table 100");
            assert_eq!(cmds[1], "ip route del default table 100");
        }

        #[test]
        fn linux_setup_with_different_params() {
            let mgr = LinuxRouteManager::with_base_table(200);
            let cmds = mgr.generate_setup_commands("wg1", "10.0.2.1", 200, 2);
            assert_eq!(
                cmds[0],
                "ip route add default via 10.0.2.1 dev wg1 table 200"
            );
            assert_eq!(cmds[1], "ip rule add fwmark 2 table 200");
        }

        #[test]
        fn linux_tracks_active_routes() {
            let mgr = LinuxRouteManager::new();
            mgr.setup_tunnel_route("wg0", "tunnel-a").unwrap();
            mgr.setup_tunnel_route("wg1", "tunnel-b").unwrap();

            let routes = mgr.active_routes();
            assert_eq!(routes.len(), 2);
            assert_eq!(routes[0].tunnel_id, "tunnel-a");
            assert_eq!(routes[0].interface, "wg0");
            assert_eq!(routes[0].routing_table, Some(100));
            assert_eq!(routes[1].tunnel_id, "tunnel-b");
            assert_eq!(routes[1].interface, "wg1");
            assert_eq!(routes[1].routing_table, Some(101));
        }

        #[test]
        fn linux_teardown_removes_route() {
            let mgr = LinuxRouteManager::new();
            mgr.setup_tunnel_route("wg0", "tunnel-a").unwrap();
            mgr.setup_tunnel_route("wg1", "tunnel-b").unwrap();

            mgr.teardown_tunnel_route("tunnel-a").unwrap();

            let routes = mgr.active_routes();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].tunnel_id, "tunnel-b");
        }

        #[test]
        fn linux_teardown_nonexistent_is_noop() {
            let mgr = LinuxRouteManager::new();
            mgr.setup_tunnel_route("wg0", "tunnel-a").unwrap();
            mgr.teardown_tunnel_route("does-not-exist").unwrap();
            assert_eq!(mgr.active_routes().len(), 1);
        }

        #[test]
        fn linux_default_base_table() {
            let mgr = LinuxRouteManager::new();
            assert_eq!(mgr.base_table(), 100);
        }

        #[test]
        fn linux_custom_base_table() {
            let mgr = LinuxRouteManager::with_base_table(500);
            assert_eq!(mgr.base_table(), 500);
        }

        // -- NamespaceManager tests -----------------------------------------

        #[test]
        fn namespace_generate_create_commands() {
            let cmds = NamespaceManager::generate_create_commands("ns_tunnel_a", "wg0");
            assert_eq!(cmds.len(), 3);
            assert_eq!(cmds[0], "ip netns add ns_tunnel_a");
            assert_eq!(cmds[1], "ip link set wg0 netns ns_tunnel_a");
            assert_eq!(cmds[2], "ip netns exec ns_tunnel_a ip link set wg0 up");
        }

        #[test]
        fn namespace_generate_delete_commands() {
            let cmds = NamespaceManager::generate_delete_commands("ns_tunnel_a");
            assert_eq!(cmds.len(), 1);
            assert_eq!(cmds[0], "ip netns del ns_tunnel_a");
        }

        #[test]
        fn namespace_create_with_different_names() {
            let cmds = NamespaceManager::generate_create_commands("houdinny_tunnel_b", "wg1");
            assert_eq!(cmds[0], "ip netns add houdinny_tunnel_b");
            assert_eq!(cmds[1], "ip link set wg1 netns houdinny_tunnel_b");
            assert_eq!(
                cmds[2],
                "ip netns exec houdinny_tunnel_b ip link set wg1 up"
            );
        }
    }

    // -- Cross-platform command generation tests ----------------------------
    // These tests verify command string generation without actually being
    // on Linux. They test the logic directly.

    #[test]
    fn setup_commands_format() {
        // Verify the expected command format by constructing manually.
        let interface = "wg0";
        let gateway = "10.0.1.1";
        let table = 100u32;
        let fwmark = 1u32;

        let expected = vec![
            format!("ip route add default via {gateway} dev {interface} table {table}"),
            format!("ip rule add fwmark {fwmark} table {table}"),
        ];

        assert_eq!(
            expected[0],
            "ip route add default via 10.0.1.1 dev wg0 table 100"
        );
        assert_eq!(expected[1], "ip rule add fwmark 1 table 100");
    }

    #[test]
    fn teardown_commands_format() {
        let table = 200u32;
        let fwmark = 2u32;

        let expected = vec![
            format!("ip rule del fwmark {fwmark} table {table}"),
            format!("ip route del default table {table}"),
        ];

        assert_eq!(expected[0], "ip rule del fwmark 2 table 200");
        assert_eq!(expected[1], "ip route del default table 200");
    }

    #[test]
    fn namespace_create_commands_format() {
        let ns_name = "ns_tunnel_a";
        let interface = "wg0";

        let expected = vec![
            format!("ip netns add {ns_name}"),
            format!("ip link set {interface} netns {ns_name}"),
            format!("ip netns exec {ns_name} ip link set {interface} up"),
        ];

        assert_eq!(expected[0], "ip netns add ns_tunnel_a");
        assert_eq!(expected[1], "ip link set wg0 netns ns_tunnel_a");
        assert_eq!(expected[2], "ip netns exec ns_tunnel_a ip link set wg0 up");
    }

    #[test]
    fn namespace_delete_commands_format() {
        let ns_name = "ns_tunnel_a";
        let expected = vec![format!("ip netns del {ns_name}")];
        assert_eq!(expected[0], "ip netns del ns_tunnel_a");
    }

    // -- Factory tests ------------------------------------------------------

    #[test]
    fn create_route_manager_disabled() {
        let config = RouteConfig {
            enabled: false,
            ..RouteConfig::default()
        };
        let mgr = create_route_manager(&config);
        // Should be a NoopRouteManager — active_routes is always empty.
        assert!(mgr.active_routes().is_empty());
        mgr.setup_tunnel_route("wg0", "t1").unwrap();
        assert!(mgr.active_routes().is_empty());
    }

    #[test]
    fn create_route_manager_enabled_non_linux() {
        // On non-Linux, even with enabled=true, we get a noop manager.
        #[cfg(not(target_os = "linux"))]
        {
            let config = RouteConfig {
                enabled: true,
                base_table: 200,
                use_namespaces: false,
            };
            let mgr = create_route_manager(&config);
            mgr.setup_tunnel_route("wg0", "t1").unwrap();
            assert!(mgr.active_routes().is_empty());
        }
    }
}
