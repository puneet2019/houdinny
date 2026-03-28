//! WireGuard transport — binds TCP connections to an existing WireGuard interface.
//!
//! This is the **Option B (MVP)** approach: the user sets up a WireGuard
//! interface externally (via `wg-quick up`, NordVPN, Mullvad, etc.), and
//! houdinny binds outgoing sockets to that interface so traffic exits through
//! the WireGuard tunnel.
//!
//! # Platform support
//!
//! - **Linux**: uses `SO_BINDTODEVICE` via [`socket2::Socket::bind_device`].
//! - **macOS**: uses `IP_BOUND_IF` via `setsockopt` with the interface index,
//!   falling back to binding to the interface's IP address.

use super::{AsyncReadWrite, Transport};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};

/// A WireGuard transport that binds outgoing sockets to an existing WireGuard
/// network interface.
///
/// The WireGuard tunnel must already be running and the interface must exist
/// before [`Transport::provision`] is called.
pub struct WireGuardTransport {
    /// The WireGuard interface name (e.g. `"wg0"`, `"utun3"`, `"nordlynx"`).
    interface: String,
    /// Human-readable label for logging.
    label: String,
    /// Unique identifier for this transport instance.
    id: String,
    /// Whether the interface is up and the transport is healthy.
    healthy: AtomicBool,
}

impl WireGuardTransport {
    /// Create a new WireGuard transport bound to the given network interface.
    ///
    /// The interface should already exist and be managed externally (e.g. via
    /// `wg-quick up wg0`).
    pub fn new(interface: impl Into<String>, label: impl Into<String>) -> Self {
        let interface = interface.into();
        let label = label.into();
        let id = format!("wireguard-{}", label);
        Self {
            interface,
            label,
            id,
            healthy: AtomicBool::new(false), // starts unhealthy until provisioned
        }
    }
}

#[async_trait]
impl Transport for WireGuardTransport {
    async fn provision(&mut self) -> Result<()> {
        let exists = interface_exists(&self.interface);
        self.healthy.store(exists, Ordering::Relaxed);
        if exists {
            tracing::info!(
                interface = %self.interface,
                label = %self.label,
                "wireguard interface detected"
            );
            Ok(())
        } else {
            tracing::warn!(
                interface = %self.interface,
                label = %self.label,
                "wireguard interface not found — transport marked unhealthy"
            );
            // Return Ok — the transport is simply unhealthy, not a fatal error.
            // The pool will skip it when picking a transport.
            Ok(())
        }
    }

    async fn connect(&self, addr: &str, port: u16) -> Result<Box<dyn AsyncReadWrite>> {
        if !self.healthy.load(Ordering::Relaxed) {
            return Err(Error::TunnelConnect(format!(
                "wireguard interface '{}' is not healthy",
                self.interface
            )));
        }

        // Resolve the destination address.
        let dest: SocketAddr = format!("{addr}:{port}")
            .to_socket_addrs()
            .map_err(|e| {
                Error::TunnelConnect(format!("DNS resolution failed for {addr}:{port}: {e}"))
            })?
            .next()
            .ok_or_else(|| Error::TunnelConnect(format!("no addresses found for {addr}:{port}")))?;

        // Create a socket via socket2 so we can set options before connecting.
        let domain = if dest.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        };
        let socket =
            socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
                .map_err(|e| Error::TunnelConnect(format!("socket creation failed: {e}")))?;

        // Bind to the WireGuard interface.
        bind_to_interface(&socket, &self.interface)?;

        // Set non-blocking before converting to tokio.
        socket
            .set_nonblocking(true)
            .map_err(|e| Error::TunnelConnect(format!("failed to set non-blocking: {e}")))?;

        // Connect to the destination.
        let dest_addr: socket2::SockAddr = dest.into();
        match socket.connect(&dest_addr) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(nix::libc::EINPROGRESS) => {
                // Non-blocking connect in progress — tokio will handle it.
            }
            Err(e) => {
                return Err(Error::TunnelConnect(format!(
                    "connect to {dest} via interface '{}' failed: {e}",
                    self.interface
                )));
            }
        }

        // Convert to a tokio TcpStream.
        let std_stream: std::net::TcpStream = socket.into();
        let stream = tokio::net::TcpStream::from_std(std_stream)
            .map_err(|e| Error::TunnelConnect(format!("tokio conversion failed: {e}")))?;

        // Wait for the connect to actually complete.
        stream.writable().await.map_err(|e| {
            Error::TunnelConnect(format!(
                "connect to {dest} via interface '{}' failed: {e}",
                self.interface
            ))
        })?;

        // Check for connection errors.
        if let Some(err) = stream
            .take_error()
            .map_err(|e| Error::TunnelConnect(format!("failed to check socket error: {e}")))?
        {
            return Err(Error::TunnelConnect(format!(
                "connect to {dest} via interface '{}' failed: {err}",
                self.interface
            )));
        }

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
        "wireguard"
    }

    async fn close(&self) -> Result<()> {
        self.healthy.store(false, Ordering::Relaxed);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Platform: interface detection
// ---------------------------------------------------------------------------

/// Check whether a network interface with the given name exists.
#[cfg(target_os = "linux")]
fn interface_exists(name: &str) -> bool {
    let path = format!("/sys/class/net/{name}");
    std::path::Path::new(&path).exists()
}

/// Check whether a network interface with the given name exists (macOS).
///
/// Uses `nix::net::if_::if_nametoindex` — returns `Err` when the interface
/// does not exist.
#[cfg(target_os = "macos")]
fn interface_exists(name: &str) -> bool {
    nix::net::if_::if_nametoindex(name).is_ok()
}

/// Fallback for unsupported platforms — always returns false.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn interface_exists(_name: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Platform: bind socket to interface
// ---------------------------------------------------------------------------

/// Bind a socket to a specific network interface (Linux).
///
/// Uses `SO_BINDTODEVICE` via [`socket2::Socket::bind_device`].
#[cfg(target_os = "linux")]
fn bind_to_interface(socket: &socket2::Socket, interface: &str) -> Result<()> {
    socket.bind_device(Some(interface.as_bytes())).map_err(|e| {
        Error::TunnelConnect(format!(
            "SO_BINDTODEVICE to interface '{interface}' failed: {e}"
        ))
    })
}

/// Bind a socket to a specific network interface (macOS).
///
/// macOS does not support `SO_BINDTODEVICE`. Instead, we use `IP_BOUND_IF`
/// (`setsockopt` with the interface index) for IPv4, or `IPV6_BOUND_IF` for
/// IPv6 sockets.
#[cfg(target_os = "macos")]
fn bind_to_interface(socket: &socket2::Socket, interface: &str) -> Result<()> {
    let index = nix::net::if_::if_nametoindex(interface)
        .map_err(|e| Error::TunnelConnect(format!("interface '{interface}' not found: {e}")))?;

    // IP_BOUND_IF = 25 on macOS, IPV6_BOUND_IF = 125
    // These constants are not exposed by libc crate on all versions,
    // so we define them directly.
    const IP_BOUND_IF: nix::libc::c_int = 25;
    const IPV6_BOUND_IF: nix::libc::c_int = 125;

    let index_val: nix::libc::c_uint = index;

    // Try IPv4 first (most common), then IPv6.
    // We attempt both; one will fail depending on socket domain — that's fine.
    let ipv4_result = unsafe {
        nix::libc::setsockopt(
            std::os::unix::io::AsRawFd::as_raw_fd(socket),
            nix::libc::IPPROTO_IP,
            IP_BOUND_IF,
            &index_val as *const nix::libc::c_uint as *const nix::libc::c_void,
            std::mem::size_of::<nix::libc::c_uint>() as nix::libc::socklen_t,
        )
    };

    if ipv4_result == 0 {
        return Ok(());
    }

    let ipv6_result = unsafe {
        nix::libc::setsockopt(
            std::os::unix::io::AsRawFd::as_raw_fd(socket),
            nix::libc::IPPROTO_IPV6,
            IPV6_BOUND_IF,
            &index_val as *const nix::libc::c_uint as *const nix::libc::c_void,
            std::mem::size_of::<nix::libc::c_uint>() as nix::libc::socklen_t,
        )
    };

    if ipv6_result == 0 {
        Ok(())
    } else {
        Err(Error::TunnelConnect(format!(
            "failed to bind socket to interface '{interface}' (index {index}): {}",
            std::io::Error::last_os_error()
        )))
    }
}

/// Bind to interface — unsupported platform stub.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn bind_to_interface(_socket: &socket2::Socket, interface: &str) -> Result<()> {
    Err(Error::TunnelConnect(format!(
        "binding to interface '{interface}' is not supported on this platform"
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_valid_instance() {
        let t = WireGuardTransport::new("wg0", "my-vpn");
        assert_eq!(t.interface, "wg0");
        assert_eq!(t.label, "my-vpn");
        assert_eq!(t.id, "wireguard-my-vpn");
        // Starts unhealthy until provisioned.
        assert!(!t.healthy.load(Ordering::Relaxed));
    }

    #[test]
    fn protocol_returns_wireguard() {
        let t = WireGuardTransport::new("wg0", "test");
        assert_eq!(t.protocol(), "wireguard");
    }

    #[test]
    fn id_and_label_accessors() {
        let t = WireGuardTransport::new("utun3", "nord-us");
        assert_eq!(t.id(), "wireguard-nord-us");
        assert_eq!(t.label(), "nord-us");
    }

    #[tokio::test]
    async fn provision_detects_missing_interface() {
        let mut t = WireGuardTransport::new("houdinny_nonexistent_iface_12345", "missing");
        let result = t.provision().await;
        // provision succeeds but marks transport unhealthy.
        assert!(result.is_ok());
        assert!(!t.healthy());
    }

    #[tokio::test]
    async fn provision_detects_loopback() {
        // The loopback interface should always exist.
        let iface = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        let mut t = WireGuardTransport::new(iface, "loopback");
        let result = t.provision().await;
        assert!(result.is_ok());
        assert!(t.healthy());
    }

    #[tokio::test]
    async fn connect_fails_when_unhealthy() {
        let t = WireGuardTransport::new("nonexistent", "dead");
        // healthy is false by default (not provisioned).
        let result = t.connect("example.com", 443).await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error, got Ok"),
        };
        let err_msg = format!("{err}");
        assert!(err_msg.contains("not healthy"));
    }

    #[tokio::test]
    async fn close_marks_unhealthy() {
        let t = WireGuardTransport::new("lo0", "test");
        // Force healthy.
        t.healthy.store(true, Ordering::Relaxed);
        assert!(t.healthy());
        t.close().await.unwrap();
        assert!(!t.healthy());
    }

    #[test]
    fn bind_to_interface_with_invalid_interface_returns_error() {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .expect("socket creation should succeed");

        let result = bind_to_interface(&socket, "houdinny_nonexistent_iface_99999");
        assert!(result.is_err());
    }
}
