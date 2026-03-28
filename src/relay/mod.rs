//! Buffering stream relay / multiplexer.
//!
//! Sits between the agent and the destination, holding the agent side
//! stable while the server side can be swapped underneath.
//!
//! # Architecture
//!
//! ```text
//! Agent <-- stable connection --> Relay <-- rotatable --> Tunnel -> Destination
//!                                   |
//!                           buffers data, can reconnect
//!                           server side on a new tunnel
//! ```
//!
//! Two relay types are provided:
//!
//! - [`RelayStream`] — simple bidirectional copy with instrumentation.
//! - [`BufferedRelay`] — advanced relay that keeps the agent side open
//!   across multiple server connections.

use crate::error::{Error, Result};
use crate::transport::AsyncReadWrite;
use bytes::BytesMut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info};

/// Default read buffer size (8 KB).
const BUF_SIZE: usize = 8 * 1024;

/// Statistics gathered during a relay session.
#[derive(Debug, Clone)]
pub struct RelayStats {
    /// Bytes sent from agent to server (upstream).
    pub bytes_up: u64,
    /// Bytes sent from server to agent (downstream).
    pub bytes_down: u64,
    /// How long the relay was active.
    pub duration: Duration,
}

/// Simple bidirectional relay between an agent and a server connection.
///
/// Uses `tokio::io::copy_bidirectional` under the hood. Once the relay
/// finishes (either side closes or errors), both connections are done.
pub struct RelayStream {
    /// Agent-side reader — held open for the lifetime of the relay.
    agent_reader: Box<dyn AsyncRead + Send + Unpin>,
    /// Agent-side writer — held open for the lifetime of the relay.
    agent_writer: Box<dyn AsyncWrite + Send + Unpin>,
    /// Server-side connection — can be swapped before starting the relay.
    server: Box<dyn AsyncReadWrite>,
    /// Bytes relayed from agent to server.
    bytes_relayed_up: AtomicU64,
    /// Bytes relayed from server to agent.
    bytes_relayed_down: AtomicU64,
}

impl RelayStream {
    /// Create a new `RelayStream`.
    ///
    /// # Arguments
    ///
    /// * `agent_reader` — the read half of the agent connection.
    /// * `agent_writer` — the write half of the agent connection.
    /// * `server` — the full-duplex server connection.
    pub fn new(
        agent_reader: impl AsyncRead + Send + Unpin + 'static,
        agent_writer: impl AsyncWrite + Send + Unpin + 'static,
        server: Box<dyn AsyncReadWrite>,
    ) -> Self {
        Self {
            agent_reader: Box::new(agent_reader),
            agent_writer: Box::new(agent_writer),
            server,
            bytes_relayed_up: AtomicU64::new(0),
            bytes_relayed_down: AtomicU64::new(0),
        }
    }

    /// Relay bytes bidirectionally between agent and server.
    ///
    /// Returns [`RelayStats`] when either side closes or an error occurs.
    pub async fn relay(&mut self) -> Result<RelayStats> {
        info!("relay stream starting");
        let start = Instant::now();

        // We need a combined read+write type for each side for copy_bidirectional.
        // Build manual relay loops using tokio::select! since agent_reader/writer
        // are split and copy_bidirectional needs a single AsyncRead+AsyncWrite.
        let mut agent_buf = [0u8; BUF_SIZE];
        let mut server_buf = [0u8; BUF_SIZE];
        let mut up: u64 = 0;
        let mut down: u64 = 0;

        loop {
            tokio::select! {
                result = self.agent_reader.read(&mut agent_buf) => {
                    match result {
                        Ok(0) => {
                            debug!(bytes_up = up, bytes_down = down, "agent closed connection");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = self.server.write_all(&agent_buf[..n]).await {
                                debug!(error = %e, "failed writing to server");
                                break;
                            }
                            up += n as u64;
                            self.bytes_relayed_up.store(up, Ordering::Relaxed);
                            debug!(bytes = n, total_up = up, "agent -> server");
                        }
                        Err(e) => {
                            debug!(error = %e, "agent read error");
                            break;
                        }
                    }
                }
                result = self.server.read(&mut server_buf) => {
                    match result {
                        Ok(0) => {
                            debug!(bytes_up = up, bytes_down = down, "server closed connection");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = self.agent_writer.write_all(&server_buf[..n]).await {
                                debug!(error = %e, "failed writing to agent");
                                break;
                            }
                            down += n as u64;
                            self.bytes_relayed_down.store(down, Ordering::Relaxed);
                            debug!(bytes = n, total_down = down, "server -> agent");
                        }
                        Err(e) => {
                            debug!(error = %e, "server read error");
                            break;
                        }
                    }
                }
            }
        }

        let duration = start.elapsed();
        info!(
            bytes_up = up,
            bytes_down = down,
            duration_ms = duration.as_millis() as u64,
            "relay stream finished"
        );

        Ok(RelayStats {
            bytes_up: up,
            bytes_down: down,
            duration,
        })
    }
}

/// Advanced buffering relay that can rotate server connections while
/// keeping the agent side open.
///
/// # Usage
///
/// ```rust,no_run
/// # use houdinny::relay::BufferedRelay;
/// # async fn example() -> houdinny::error::Result<()> {
/// let (agent_read, agent_write) = tokio::io::duplex(8192);
/// let mut relay = BufferedRelay::new(agent_read, agent_write);
///
/// // First tunnel
/// # let server1: Box<dyn houdinny::transport::AsyncReadWrite> = todo!();
/// relay.relay_through(server1).await?;
///
/// // Server disconnected or we decided to rotate -- agent side still open
/// if relay.agent_alive() {
///     # let server2: Box<dyn houdinny::transport::AsyncReadWrite> = todo!();
///     relay.relay_through(server2).await?;
/// }
/// # Ok(())
/// # }
/// ```
pub struct BufferedRelay {
    /// Agent-side reader — kept open across server rotations.
    agent_reader: Box<dyn AsyncRead + Send + Unpin>,
    /// Agent-side writer — kept open across server rotations.
    agent_writer: Box<dyn AsyncWrite + Send + Unpin>,
    /// Buffer for data received from the agent during reconnection windows.
    buffer: BytesMut,
    /// Whether the agent side is still connected.
    agent_connected: bool,
    /// Cumulative bytes sent from agent to server across all sessions.
    total_bytes_up: u64,
    /// Cumulative bytes sent from server to agent across all sessions.
    total_bytes_down: u64,
    /// When the first relay_through call started.
    started_at: Option<Instant>,
}

impl BufferedRelay {
    /// Create a new `BufferedRelay` holding the agent side of the connection.
    ///
    /// The agent side is kept open across multiple server connections. Call
    /// [`relay_through`](Self::relay_through) with successive server connections
    /// to relay traffic.
    pub fn new(
        agent_read: impl AsyncRead + Send + Unpin + 'static,
        agent_write: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self {
            agent_reader: Box::new(agent_read),
            agent_writer: Box::new(agent_write),
            buffer: BytesMut::with_capacity(BUF_SIZE),
            agent_connected: true,
            total_bytes_up: 0,
            total_bytes_down: 0,
            started_at: None,
        }
    }

    /// Start relaying through the given server connection.
    ///
    /// Bidirectionally copies data between the agent and the server.
    /// Returns when the server side disconnects or an error occurs on
    /// the server side. If the agent disconnects, this also returns but
    /// marks the relay as no longer alive.
    ///
    /// After this method returns, the agent side may still be open. Check
    /// [`agent_alive`](Self::agent_alive) and call `relay_through` again
    /// with a new server connection to continue relaying.
    pub async fn relay_through(
        &mut self,
        mut server: Box<dyn AsyncReadWrite>,
    ) -> Result<RelayStats> {
        if !self.agent_connected {
            return Err(Error::Proxy("agent connection is closed".into()));
        }

        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }

        info!("buffered relay: starting relay through new server connection");
        let start = Instant::now();
        let mut up: u64 = 0;
        let mut down: u64 = 0;

        // Flush any buffered agent data from a previous session to the new server.
        if !self.buffer.is_empty() {
            let buffered = self.buffer.split();
            debug!(
                bytes = buffered.len(),
                "flushing buffered agent data to new server"
            );
            if let Err(e) = server.write_all(&buffered).await {
                debug!(error = %e, "failed flushing buffer to server");
                return Err(Error::Io(e));
            }
            up += buffered.len() as u64;
        }

        let mut agent_buf = [0u8; BUF_SIZE];
        let mut server_buf = [0u8; BUF_SIZE];

        loop {
            tokio::select! {
                result = self.agent_reader.read(&mut agent_buf) => {
                    match result {
                        Ok(0) => {
                            debug!(bytes_up = up, bytes_down = down, "agent closed connection");
                            self.agent_connected = false;
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = server.write_all(&agent_buf[..n]).await {
                                debug!(error = %e, "failed writing to server, buffering agent data");
                                // Server write failed -- buffer the data for the next server.
                                self.buffer.extend_from_slice(&agent_buf[..n]);
                                break;
                            }
                            up += n as u64;
                            debug!(bytes = n, total_up = up, "agent -> server");
                        }
                        Err(e) => {
                            debug!(error = %e, "agent read error");
                            self.agent_connected = false;
                            break;
                        }
                    }
                }
                result = server.read(&mut server_buf) => {
                    match result {
                        Ok(0) => {
                            debug!(bytes_up = up, bytes_down = down, "server closed connection");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = self.agent_writer.write_all(&server_buf[..n]).await {
                                debug!(error = %e, "failed writing to agent");
                                self.agent_connected = false;
                                break;
                            }
                            down += n as u64;
                            debug!(bytes = n, total_down = down, "server -> agent");
                        }
                        Err(e) => {
                            debug!(error = %e, "server read error");
                            // Server error -- agent may still be alive.
                            break;
                        }
                    }
                }
            }
        }

        self.total_bytes_up += up;
        self.total_bytes_down += down;
        let duration = start.elapsed();

        info!(
            bytes_up = up,
            bytes_down = down,
            duration_ms = duration.as_millis() as u64,
            agent_alive = self.agent_connected,
            "buffered relay: session finished"
        );

        Ok(RelayStats {
            bytes_up: up,
            bytes_down: down,
            duration,
        })
    }

    /// Check if the agent side is still connected.
    ///
    /// Returns `true` if the agent has not closed its connection or
    /// produced an error. When this returns `false`, there is no point
    /// calling [`relay_through`](Self::relay_through) again.
    pub fn agent_alive(&self) -> bool {
        self.agent_connected
    }

    /// Get cumulative statistics across all [`relay_through`](Self::relay_through) calls.
    pub fn total_stats(&self) -> RelayStats {
        let duration = self
            .started_at
            .map(|s| s.elapsed())
            .unwrap_or(Duration::ZERO);
        RelayStats {
            bytes_up: self.total_bytes_up,
            bytes_down: self.total_bytes_down,
            duration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    // ── RelayStream tests ──────────────────────────────────────────

    #[tokio::test]
    async fn relay_stream_bidirectional() {
        // Agent side: duplex pair
        let (agent_relay_side, mut agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        // Server side: duplex pair
        let (server_relay_side, mut server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        let mut relay = RelayStream::new(agent_read, agent_write, server);

        // Spawn the relay in the background.
        let relay_handle = tokio::spawn(async move { relay.relay().await });

        // Agent sends data -> should arrive at server test side.
        agent_test_side
            .write_all(b"hello from agent")
            .await
            .unwrap();

        let mut buf = [0u8; 64];
        let n = server_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from agent");

        // Server sends data -> should arrive at agent test side.
        server_test_side
            .write_all(b"hello from server")
            .await
            .unwrap();

        let n = agent_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello from server");

        // Close the agent side to terminate the relay.
        drop(agent_test_side);
        // Also need to drop server test side so server read returns 0.
        drop(server_test_side);

        let stats = relay_handle.await.unwrap().unwrap();
        assert_eq!(stats.bytes_up, 16); // "hello from agent" = 16 bytes
        assert_eq!(stats.bytes_down, 17); // "hello from server" = 17 bytes
    }

    #[tokio::test]
    async fn relay_stream_server_disconnect() {
        let (agent_relay_side, _agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let (server_relay_side, server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        let mut relay = RelayStream::new(agent_read, agent_write, server);

        // Drop the server test side immediately to simulate server disconnect.
        drop(server_test_side);

        let stats = relay.relay().await.unwrap();
        assert_eq!(stats.bytes_up, 0);
        assert_eq!(stats.bytes_down, 0);
    }

    #[tokio::test]
    async fn relay_stream_agent_disconnect() {
        let (agent_relay_side, agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let (server_relay_side, _server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        let mut relay = RelayStream::new(agent_read, agent_write, server);

        // Drop the agent test side immediately to simulate agent disconnect.
        drop(agent_test_side);

        let stats = relay.relay().await.unwrap();
        assert_eq!(stats.bytes_up, 0);
        assert_eq!(stats.bytes_down, 0);
    }

    // ── BufferedRelay tests ────────────────────────────────────────

    #[tokio::test]
    async fn buffered_relay_basic_bidirectional() {
        let (agent_relay_side, mut agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let mut relay = BufferedRelay::new(agent_read, agent_write);
        assert!(relay.agent_alive());

        let (server_relay_side, mut server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        let relay_handle = tokio::spawn(async move {
            let stats = relay.relay_through(server).await;
            (relay, stats)
        });

        // Agent -> Server
        agent_test_side.write_all(b"request").await.unwrap();
        let mut buf = [0u8; 64];
        let n = server_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"request");

        // Server -> Agent
        server_test_side.write_all(b"response").await.unwrap();
        let n = agent_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"response");

        // Server disconnects.
        drop(server_test_side);

        let (relay, stats) = relay_handle.await.unwrap();
        let stats = stats.unwrap();
        assert_eq!(stats.bytes_up, 7); // "request"
        assert_eq!(stats.bytes_down, 8); // "response"
        assert!(relay.agent_alive());
    }

    #[tokio::test]
    async fn buffered_relay_multiple_servers() {
        let (agent_relay_side, mut agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let mut relay = BufferedRelay::new(agent_read, agent_write);

        // --- First server session ---
        let (server1_relay_side, mut server1_test_side) = duplex(BUF_SIZE);
        let server1: Box<dyn AsyncReadWrite> = Box::new(server1_relay_side);

        let relay_handle = tokio::spawn(async move {
            let stats = relay.relay_through(server1).await;
            (relay, stats)
        });

        agent_test_side.write_all(b"first").await.unwrap();
        let mut buf = [0u8; 64];
        let n = server1_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"first");

        server1_test_side.write_all(b"reply1").await.unwrap();
        let n = agent_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"reply1");

        // Server 1 disconnects.
        drop(server1_test_side);

        let (mut relay, stats1) = relay_handle.await.unwrap();
        let stats1 = stats1.unwrap();
        assert_eq!(stats1.bytes_up, 5);
        assert_eq!(stats1.bytes_down, 6);
        assert!(relay.agent_alive());

        // --- Second server session ---
        let (server2_relay_side, mut server2_test_side) = duplex(BUF_SIZE);
        let server2: Box<dyn AsyncReadWrite> = Box::new(server2_relay_side);

        let relay_handle = tokio::spawn(async move {
            let stats = relay.relay_through(server2).await;
            (relay, stats)
        });

        agent_test_side.write_all(b"second").await.unwrap();
        let n = server2_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"second");

        server2_test_side.write_all(b"reply2!").await.unwrap();
        let n = agent_test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"reply2!");

        // Server 2 disconnects.
        drop(server2_test_side);

        let (relay, stats2) = relay_handle.await.unwrap();
        let stats2 = stats2.unwrap();
        assert_eq!(stats2.bytes_up, 6);
        assert_eq!(stats2.bytes_down, 7);
        assert!(relay.agent_alive());

        // Total stats
        let total = relay.total_stats();
        assert_eq!(total.bytes_up, 11); // 5 + 6
        assert_eq!(total.bytes_down, 13); // 6 + 7
    }

    #[tokio::test]
    async fn buffered_relay_agent_disconnect() {
        let (agent_relay_side, agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let mut relay = BufferedRelay::new(agent_read, agent_write);

        let (server_relay_side, _server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        // Drop agent test side to simulate agent disconnect.
        drop(agent_test_side);

        let stats = relay.relay_through(server).await.unwrap();
        assert_eq!(stats.bytes_up, 0);
        assert_eq!(stats.bytes_down, 0);
        assert!(!relay.agent_alive());
    }

    #[tokio::test]
    async fn buffered_relay_agent_dead_rejects_new_server() {
        let (agent_relay_side, agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let mut relay = BufferedRelay::new(agent_read, agent_write);

        // Kill the agent.
        drop(agent_test_side);
        let (server1_relay_side, _s1) = duplex(BUF_SIZE);
        let server1: Box<dyn AsyncReadWrite> = Box::new(server1_relay_side);
        let _ = relay.relay_through(server1).await;
        assert!(!relay.agent_alive());

        // Trying to relay through another server should fail immediately.
        let (server2_relay_side, _s2) = duplex(BUF_SIZE);
        let server2: Box<dyn AsyncReadWrite> = Box::new(server2_relay_side);
        let result = relay.relay_through(server2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn buffered_relay_server_disconnect_agent_stays_open() {
        let (agent_relay_side, _agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let mut relay = BufferedRelay::new(agent_read, agent_write);

        // Server disconnects immediately.
        let (server_relay_side, server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);
        drop(server_test_side);

        let stats = relay.relay_through(server).await.unwrap();
        assert_eq!(stats.bytes_up, 0);
        assert_eq!(stats.bytes_down, 0);

        // Agent should still be alive.
        assert!(relay.agent_alive());
    }

    #[tokio::test]
    async fn relay_stats_reports_correct_counts() {
        let (agent_relay_side, mut agent_test_side) = duplex(BUF_SIZE);
        let (agent_read, agent_write) = tokio::io::split(agent_relay_side);

        let (server_relay_side, mut server_test_side) = duplex(BUF_SIZE);
        let server: Box<dyn AsyncReadWrite> = Box::new(server_relay_side);

        let mut relay = RelayStream::new(agent_read, agent_write, server);

        let relay_handle = tokio::spawn(async move { relay.relay().await });

        // Send known amounts of data.
        let up_data = b"exactly twenty!!!"; // 17 bytes
        agent_test_side.write_all(up_data).await.unwrap();
        let mut buf = [0u8; 64];
        let _ = server_test_side.read(&mut buf).await.unwrap();

        let down_data = b"twelve bytes"; // 12 bytes
        server_test_side.write_all(down_data).await.unwrap();
        let _ = agent_test_side.read(&mut buf).await.unwrap();

        // Close both sides.
        drop(agent_test_side);
        drop(server_test_side);

        let stats = relay_handle.await.unwrap().unwrap();
        assert_eq!(stats.bytes_up, 17);
        assert_eq!(stats.bytes_down, 12);
        assert!(stats.duration.as_nanos() > 0);
    }
}
