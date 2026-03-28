//! Admin REST API — JSON control plane for inspecting and modifying the proxy at runtime.
//!
//! Runs on a separate port (default 8081) from the proxy itself.
//! All endpoints return JSON with `Content-Type: application/json`.
//!
//! # Endpoints
//!
//! | Method | Path     | Description                     |
//! |--------|----------|---------------------------------|
//! | GET    | /health  | Basic health check              |
//! | GET    | /pool    | Pool status with tunnel details |
//! | GET    | /stats   | Relay statistics (placeholder)  |

use crate::pool::Pool;
use bytes::Bytes;
use http::Method;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Admin API server that exposes JSON endpoints for runtime inspection.
///
/// # Example
///
/// ```no_run
/// # #[tokio::main]
/// # async fn main() -> houdinny::error::Result<()> {
/// use houdinny::admin::AdminServer;
/// use houdinny::pool::Pool;
/// use std::net::SocketAddr;
/// use std::sync::Arc;
///
/// let pool = Arc::new(Pool::new(vec![]));
/// let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
/// let server = AdminServer::new(addr, pool);
/// // tokio::spawn(server.run());
/// # Ok(())
/// # }
/// ```
pub struct AdminServer {
    listen_addr: SocketAddr,
    pool: Arc<Pool>,
}

impl AdminServer {
    /// Create a new admin server bound to the given address.
    pub fn new(listen_addr: SocketAddr, pool: Arc<Pool>) -> Self {
        Self { listen_addr, pool }
    }

    /// Run the admin API server. Call this in a `tokio::spawn`.
    ///
    /// Listens for HTTP/1.1 connections and dispatches each request
    /// through [`handle_request`].
    pub async fn run(self) -> crate::error::Result<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        info!(addr = %self.listen_addr, "admin API listening");

        loop {
            let (stream, peer) = listener.accept().await?;
            let pool = Arc::clone(&self.pool);
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let pool = Arc::clone(&pool);
                    async move { Ok::<_, std::convert::Infallible>(handle_request(&req, &pool).await) }
                });
                if let Err(e) = http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
                    .await
                {
                    error!(peer = %peer, error = %e, "admin connection error");
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

/// Route an incoming request and produce a JSON response.
///
/// This function is the core dispatch logic, kept separate from the server
/// so it can be tested without starting a TCP listener.
pub async fn handle_request(req: &Request<Incoming>, pool: &Pool) -> Response<Full<Bytes>> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/health") => json_response(StatusCode::OK, &health_body()),
        (&Method::GET, "/pool") => json_response(StatusCode::OK, &pool_body(pool).await),
        (&Method::GET, "/stats") => json_response(StatusCode::OK, &stats_body()),

        // Correct path but wrong method.
        (_, "/health" | "/pool" | "/stats") => json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &error_body("method not allowed"),
        ),

        // Unknown route.
        _ => json_response(StatusCode::NOT_FOUND, &error_body("not found")),
    }
}

// ---------------------------------------------------------------------------
// Response bodies (serializable structs)
// ---------------------------------------------------------------------------

/// Health check response.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Always `"ok"`.
    pub status: &'static str,
    /// Crate version from `Cargo.toml`.
    pub version: &'static str,
}

/// A single tunnel entry in the pool status response.
#[derive(Debug, Clone, Serialize)]
pub struct TunnelEntry {
    /// Unique tunnel identifier.
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Protocol (e.g. `socks5`, `wireguard`).
    pub protocol: String,
    /// Whether the tunnel is currently healthy.
    pub healthy: bool,
}

/// Pool status response.
#[derive(Debug, Clone, Serialize)]
pub struct PoolResponse {
    /// Total number of transports in the pool.
    pub total: usize,
    /// Number of healthy transports.
    pub healthy: usize,
    /// Per-tunnel details.
    pub tunnels: Vec<TunnelEntry>,
}

/// Relay statistics response (placeholder).
#[derive(Debug, Clone, Serialize)]
pub struct StatsResponse {
    /// Total connections handled since start.
    pub connections_total: u64,
    /// Currently active connections.
    pub connections_active: u64,
    /// Total bytes relayed.
    pub bytes_relayed: u64,
}

/// Generic error body.
#[derive(Debug, Clone, Serialize)]
struct ErrorBody {
    error: String,
}

// ---------------------------------------------------------------------------
// Body builders
// ---------------------------------------------------------------------------

fn health_body() -> HealthResponse {
    HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    }
}

async fn pool_body(pool: &Pool) -> PoolResponse {
    let all = pool.all().await;
    let healthy_count = all.iter().filter(|t| t.healthy()).count();
    let tunnels = all
        .iter()
        .map(|t| TunnelEntry {
            id: t.id().to_owned(),
            label: t.label().to_owned(),
            protocol: t.protocol().to_owned(),
            healthy: t.healthy(),
        })
        .collect();
    PoolResponse {
        total: all.len(),
        healthy: healthy_count,
        tunnels,
    }
}

fn stats_body() -> StatsResponse {
    StatsResponse {
        connections_total: 0,
        connections_active: 0,
        bytes_relayed: 0,
    }
}

fn error_body(msg: &str) -> ErrorBody {
    ErrorBody {
        error: msg.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialize a value to JSON and wrap it in an HTTP response with the
/// appropriate content type and status code.
fn json_response<T: Serialize>(status: StatusCode, body: &T) -> Response<Full<Bytes>> {
    match serde_json::to_vec(body) {
        Ok(json) => Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(json)))
            .expect("response builder with valid parts should not fail"),
        Err(e) => {
            error!(error = %e, "failed to serialize admin response");
            let fallback = r#"{"error":"internal serialization error"}"#;
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(fallback)))
                .expect("static response builder should not fail")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::transport::{AsyncReadWrite, Transport};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Minimal mock transport for admin API tests.
    struct MockTransport {
        id: String,
        label: String,
        proto: String,
        healthy: AtomicBool,
    }

    impl MockTransport {
        fn new(
            id: impl Into<String>,
            label: impl Into<String>,
            proto: impl Into<String>,
            is_healthy: bool,
        ) -> Self {
            Self {
                id: id.into(),
                label: label.into(),
                proto: proto.into(),
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
            Err(crate::error::Error::TunnelConnect("mock".into()))
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
            &self.proto
        }

        async fn close(&self) -> Result<()> {
            self.healthy.store(false, Ordering::Relaxed);
            Ok(())
        }
    }

    fn mock(id: &str, label: &str, proto: &str, healthy: bool) -> Arc<dyn Transport> {
        Arc::new(MockTransport::new(id, label, proto, healthy))
    }

    fn make_pool() -> Arc<Pool> {
        Arc::new(Pool::new(vec![
            mock("socks5-tokyo", "tokyo", "socks5", true),
            mock("socks5-london", "london", "socks5", true),
            mock("wg-nord-us", "nord-us", "wireguard", false),
        ]))
    }

    // -- JSON serialization tests -------------------------------------------

    #[test]
    fn health_response_serializes() {
        let body = health_body();
        let json: serde_json::Value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn stats_response_serializes() {
        let body = stats_body();
        let json: serde_json::Value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(json["connections_total"], 0);
        assert_eq!(json["connections_active"], 0);
        assert_eq!(json["bytes_relayed"], 0);
    }

    #[tokio::test]
    async fn pool_response_serializes() {
        let pool = make_pool();
        let body = pool_body(&pool).await;
        let json: serde_json::Value = serde_json::to_value(&body).expect("serialize");

        assert_eq!(json["total"], 3);
        assert_eq!(json["healthy"], 2);

        let tunnels = json["tunnels"].as_array().expect("tunnels array");
        assert_eq!(tunnels.len(), 3);

        assert_eq!(tunnels[0]["id"], "socks5-tokyo");
        assert_eq!(tunnels[0]["label"], "tokyo");
        assert_eq!(tunnels[0]["protocol"], "socks5");
        assert_eq!(tunnels[0]["healthy"], true);

        assert_eq!(tunnels[2]["id"], "wg-nord-us");
        assert_eq!(tunnels[2]["healthy"], false);
    }

    // -- Route matching / dispatch tests ------------------------------------

    /// Helper: build a [`Request<Incoming>`] for testing via [`handle_request`].
    ///
    /// We cannot construct `Incoming` directly, so instead we test the
    /// same dispatch logic through a small wrapper that mirrors
    /// `handle_request` but accepts simpler types.
    async fn dispatch(method: &Method, path: &str, pool: &Pool) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(http_body_util::Empty::<Bytes>::new())
            .expect("build request");

        // Re-implement the dispatch matching from handle_request so we can
        // test it without needing a real hyper::body::Incoming.
        let (status, body_bytes) = match (req.method(), req.uri().path()) {
            (&Method::GET, "/health") => {
                let b = health_body();
                (StatusCode::OK, serde_json::to_vec(&b).expect("ser"))
            }
            (&Method::GET, "/pool") => {
                let b = pool_body(pool).await;
                (StatusCode::OK, serde_json::to_vec(&b).expect("ser"))
            }
            (&Method::GET, "/stats") => {
                let b = stats_body();
                (StatusCode::OK, serde_json::to_vec(&b).expect("ser"))
            }
            (_, "/health" | "/pool" | "/stats") => {
                let b = error_body("method not allowed");
                (
                    StatusCode::METHOD_NOT_ALLOWED,
                    serde_json::to_vec(&b).expect("ser"),
                )
            }
            _ => {
                let b = error_body("not found");
                (StatusCode::NOT_FOUND, serde_json::to_vec(&b).expect("ser"))
            }
        };

        let json: serde_json::Value = serde_json::from_slice(&body_bytes).expect("deser");
        (status, json)
    }

    #[tokio::test]
    async fn route_get_health() {
        let pool = make_pool();
        let (status, json) = dispatch(&Method::GET, "/health", &pool).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn route_get_pool() {
        let pool = make_pool();
        let (status, json) = dispatch(&Method::GET, "/pool", &pool).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["total"], 3);
        assert_eq!(json["healthy"], 2);
    }

    #[tokio::test]
    async fn route_get_stats() {
        let pool = make_pool();
        let (status, json) = dispatch(&Method::GET, "/stats", &pool).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["connections_total"], 0);
    }

    #[tokio::test]
    async fn route_post_health_returns_405() {
        let pool = make_pool();
        let (status, json) = dispatch(&Method::POST, "/health", &pool).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(json["error"], "method not allowed");
    }

    #[tokio::test]
    async fn route_unknown_returns_404() {
        let pool = make_pool();
        let (status, json) = dispatch(&Method::GET, "/nonexistent", &pool).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"], "not found");
    }

    #[tokio::test]
    async fn route_delete_pool_returns_405() {
        let pool = make_pool();
        let (status, _) = dispatch(&Method::DELETE, "/pool", &pool).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn empty_pool_response() {
        let pool = Arc::new(Pool::new(vec![]));
        let (status, json) = dispatch(&Method::GET, "/pool", &pool).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["total"], 0);
        assert_eq!(json["healthy"], 0);
        assert_eq!(json["tunnels"].as_array().expect("array").len(), 0);
    }
}
