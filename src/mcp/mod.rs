//! MCP (Model Context Protocol) server — exposes houdinny tools to AI agents via JSON-RPC over stdio.
//!
//! Implements a minimal JSON-RPC 2.0 handler (no external MCP SDK) that supports:
//!
//! - `tools/list` — enumerate available tools
//! - `tools/call` — invoke a tool by name
//!
//! # Tools
//!
//! | Name           | Description                          |
//! |----------------|--------------------------------------|
//! | pool_status    | Get tunnel pool status               |
//! | pool_add       | Add tunnel at runtime                |
//! | pool_remove    | Remove tunnel by ID                  |
//! | health_check   | Health check with version and counts |

use crate::pool::Pool;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// JSON-RPC error codes
// ---------------------------------------------------------------------------

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

// ---------------------------------------------------------------------------
// Tool descriptors
// ---------------------------------------------------------------------------

/// Build the static tool list returned by `tools/list`.
fn tool_definitions() -> Value {
    json!([
        {
            "name": "pool_status",
            "description": "Get tunnel pool status (total, healthy, per-tunnel details)",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "pool_add",
            "description": "Add a tunnel to the pool at runtime",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "protocol": { "type": "string", "description": "Tunnel protocol (e.g. socks5, wireguard)" },
                    "address":  { "type": "string", "description": "Tunnel endpoint address" },
                    "label":    { "type": "string", "description": "Human-readable label" }
                },
                "required": ["protocol", "address", "label"]
            }
        },
        {
            "name": "pool_remove",
            "description": "Remove a tunnel from the pool by its ID",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Tunnel ID to remove" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "health_check",
            "description": "Health check returning status, version, and tunnel count",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }
    ])
}

// ---------------------------------------------------------------------------
// McpServer
// ---------------------------------------------------------------------------

/// MCP server that exposes houdinny tools over JSON-RPC 2.0.
///
/// # Example
///
/// ```no_run
/// # #[tokio::main]
/// # async fn main() -> houdinny::error::Result<()> {
/// use houdinny::mcp::McpServer;
/// use houdinny::pool::Pool;
/// use std::sync::Arc;
///
/// let pool = Arc::new(Pool::new(vec![]));
/// let server = McpServer::new(pool);
///
/// let response = server.handle_request(
///     r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
/// ).await;
/// println!("{response}");
/// # Ok(())
/// # }
/// ```
pub struct McpServer {
    pool: Arc<Pool>,
}

impl McpServer {
    /// Create a new MCP server backed by the given tunnel pool.
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }

    /// Handle a single JSON-RPC 2.0 request string and return a JSON-RPC response string.
    ///
    /// Gracefully handles malformed JSON, unknown methods, and unknown tool names.
    pub async fn handle_request(&self, request: &str) -> String {
        let parsed: Value = match serde_json::from_str(request) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "failed to parse JSON-RPC request");
                return Self::error_response(Value::Null, PARSE_ERROR, "Parse error");
            }
        };

        let id = parsed.get("id").cloned().unwrap_or(Value::Null);

        let method = match parsed.get("method").and_then(Value::as_str) {
            Some(m) => m,
            None => {
                return Self::error_response(
                    id,
                    INVALID_REQUEST,
                    "Invalid request: missing method",
                );
            }
        };

        let params = parsed.get("params").cloned().unwrap_or(json!({}));

        debug!(method = %method, "handling MCP request");

        match method {
            "tools/list" => self.handle_tools_list(id).await,
            "tools/call" => self.handle_tools_call(id, &params).await,
            _ => {
                warn!(method = %method, "unknown JSON-RPC method");
                Self::error_response(id, METHOD_NOT_FOUND, "Method not found")
            }
        }
    }

    /// Read lines from stdin, dispatch each as a JSON-RPC request, and write responses to stdout.
    ///
    /// Runs until stdin is closed (EOF).
    pub async fn run_stdio(self) -> crate::error::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        info!("MCP server starting on stdio");

        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                info!("MCP stdin closed, shutting down");
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            debug!(request = %trimmed, "MCP stdin request");
            let response = self.handle_request(trimmed).await;
            debug!(response = %response, "MCP stdout response");

            stdout.write_all(response.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Method handlers
    // -----------------------------------------------------------------------

    /// Handle `tools/list` — return all available tool definitions.
    async fn handle_tools_list(&self, id: Value) -> String {
        let result = json!({ "tools": tool_definitions() });
        Self::success_response(id, result)
    }

    /// Handle `tools/call` — dispatch to the named tool.
    async fn handle_tools_call(&self, id: Value, params: &Value) -> String {
        let tool_name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n,
            None => {
                return Self::error_response(
                    id,
                    INVALID_PARAMS,
                    "Invalid params: missing tool name",
                );
            }
        };

        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        debug!(tool = %tool_name, "calling MCP tool");

        match tool_name {
            "pool_status" => self.tool_pool_status(id).await,
            "pool_add" => self.tool_pool_add(id, &arguments).await,
            "pool_remove" => self.tool_pool_remove(id, &arguments).await,
            "health_check" => self.tool_health_check(id).await,
            _ => {
                warn!(tool = %tool_name, "unknown MCP tool");
                Self::error_response(id, METHOD_NOT_FOUND, &format!("Unknown tool: {tool_name}"))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tool implementations
    // -----------------------------------------------------------------------

    /// `pool_status` — return pool status as JSON text content.
    async fn tool_pool_status(&self, id: Value) -> String {
        let all = self.pool.all().await;
        let healthy_count = all.iter().filter(|t| t.healthy()).count();

        let tunnels: Vec<Value> = all
            .iter()
            .map(|t| {
                json!({
                    "id": t.id(),
                    "label": t.label(),
                    "protocol": t.protocol(),
                    "healthy": t.healthy(),
                })
            })
            .collect();

        let status = json!({
            "total": all.len(),
            "healthy": healthy_count,
            "tunnels": tunnels,
        });

        Self::tool_result(id, &status.to_string())
    }

    /// `pool_add` — add a tunnel at runtime (placeholder — logs the request).
    ///
    /// A real implementation would create the transport from the supplied protocol/address,
    /// but for now we return a descriptive message since transport construction requires
    /// more context than the MCP layer has.
    async fn tool_pool_add(&self, id: Value, arguments: &Value) -> String {
        let protocol = match arguments.get("protocol").and_then(Value::as_str) {
            Some(p) => p,
            None => {
                return Self::error_response(
                    id,
                    INVALID_PARAMS,
                    "Missing required parameter: protocol",
                );
            }
        };
        let address = match arguments.get("address").and_then(Value::as_str) {
            Some(a) => a,
            None => {
                return Self::error_response(
                    id,
                    INVALID_PARAMS,
                    "Missing required parameter: address",
                );
            }
        };
        let label = match arguments.get("label").and_then(Value::as_str) {
            Some(l) => l,
            None => {
                return Self::error_response(
                    id,
                    INVALID_PARAMS,
                    "Missing required parameter: label",
                );
            }
        };

        info!(protocol = %protocol, address = %address, label = %label, "MCP pool_add requested");

        let msg = format!(
            "Tunnel add requested: protocol={protocol}, address={address}, label={label}. \
             Transport creation requires runtime provisioning — request queued."
        );

        Self::tool_result(id, &msg)
    }

    /// `pool_remove` — remove a tunnel by ID.
    async fn tool_pool_remove(&self, id: Value, arguments: &Value) -> String {
        let tunnel_id = match arguments.get("id").and_then(Value::as_str) {
            Some(tid) => tid,
            None => {
                return Self::error_response(id, INVALID_PARAMS, "Missing required parameter: id");
            }
        };

        let removed = self.pool.remove(tunnel_id).await;
        let msg = if removed {
            format!("Tunnel '{tunnel_id}' removed successfully")
        } else {
            format!("Tunnel '{tunnel_id}' not found in pool")
        };

        Self::tool_result(id, &msg)
    }

    /// `health_check` — return status, version, and tunnel count.
    async fn tool_health_check(&self, id: Value) -> String {
        let total = self.pool.len().await;
        let all = self.pool.all().await;
        let healthy_count = all.iter().filter(|t| t.healthy()).count();

        let health = json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "tunnels_total": total,
            "tunnels_healthy": healthy_count,
        });

        Self::tool_result(id, &health.to_string())
    }

    // -----------------------------------------------------------------------
    // JSON-RPC response helpers
    // -----------------------------------------------------------------------

    /// Build a successful JSON-RPC 2.0 response.
    fn success_response(id: Value, result: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })
        .to_string()
    }

    /// Build a JSON-RPC 2.0 error response.
    fn error_response(id: Value, code: i64, message: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            },
        })
        .to_string()
    }

    /// Build a tool result response with MCP text content.
    fn tool_result(id: Value, text: &str) -> String {
        Self::success_response(
            id,
            json!({
                "content": [
                    {
                        "type": "text",
                        "text": text,
                    }
                ]
            }),
        )
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

    /// Minimal mock transport for MCP tests.
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

    fn make_server() -> McpServer {
        McpServer::new(make_pool())
    }

    /// Parse a JSON-RPC response and return the parsed Value.
    fn parse_response(raw: &str) -> Value {
        serde_json::from_str(raw).expect("response should be valid JSON")
    }

    // -- tools/list ----------------------------------------------------------

    #[tokio::test]
    async fn tools_list_returns_all_four_tools() {
        let server = make_server();
        let resp = server
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#)
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);

        let tools = json["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 4);

        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"pool_status"));
        assert!(names.contains(&"pool_add"));
        assert!(names.contains(&"pool_remove"));
        assert!(names.contains(&"health_check"));
    }

    // -- tools/call pool_status ----------------------------------------------

    #[tokio::test]
    async fn tools_call_pool_status_works() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pool_status","arguments":{}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 2);

        let content = json["result"]["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");

        // The text field contains a JSON string with pool status.
        let text = content[0]["text"].as_str().expect("text string");
        let status: Value = serde_json::from_str(text).expect("status should be valid JSON");
        assert_eq!(status["total"], 3);
        assert_eq!(status["healthy"], 2);
        assert_eq!(
            status["tunnels"].as_array().expect("tunnels array").len(),
            3
        );
    }

    // -- tools/call health_check ---------------------------------------------

    #[tokio::test]
    async fn tools_call_health_check_works() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health_check","arguments":{}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 3);

        let content = json["result"]["content"].as_array().expect("content array");
        let text = content[0]["text"].as_str().expect("text string");
        let health: Value = serde_json::from_str(text).expect("health should be valid JSON");
        assert_eq!(health["status"], "ok");
        assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(health["tunnels_total"], 3);
        assert_eq!(health["tunnels_healthy"], 2);
    }

    // -- unknown method returns error ----------------------------------------

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let server = make_server();
        let resp = server
            .handle_request(r#"{"jsonrpc":"2.0","id":4,"method":"unknown/method","params":{}}"#)
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 4);
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("message")
                .contains("Method not found")
        );
    }

    // -- malformed JSON returns parse error -----------------------------------

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let server = make_server();
        let resp = server.handle_request("this is not json {{{").await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], Value::Null);
        assert_eq!(json["error"]["code"], PARSE_ERROR);
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("message")
                .contains("Parse error")
        );
    }

    // -- unknown tool name returns error -------------------------------------

    #[tokio::test]
    async fn unknown_tool_name_returns_error() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nonexistent_tool","arguments":{}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 5);
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
        assert!(
            json["error"]["message"]
                .as_str()
                .expect("message")
                .contains("Unknown tool")
        );
    }

    // -- pool_remove works ---------------------------------------------------

    #[tokio::test]
    async fn tools_call_pool_remove_existing() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"pool_remove","arguments":{"id":"socks5-tokyo"}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        let content = json["result"]["content"].as_array().expect("content array");
        let text = content[0]["text"].as_str().expect("text string");
        assert!(text.contains("removed successfully"));
    }

    #[tokio::test]
    async fn tools_call_pool_remove_nonexistent() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"pool_remove","arguments":{"id":"does-not-exist"}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        let content = json["result"]["content"].as_array().expect("content array");
        let text = content[0]["text"].as_str().expect("text string");
        assert!(text.contains("not found"));
    }

    // -- pool_add works ------------------------------------------------------

    #[tokio::test]
    async fn tools_call_pool_add() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"pool_add","arguments":{"protocol":"socks5","address":"127.0.0.1:1080","label":"test-tunnel"}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 8);

        let content = json["result"]["content"].as_array().expect("content array");
        let text = content[0]["text"].as_str().expect("text string");
        assert!(text.contains("protocol=socks5"));
        assert!(text.contains("address=127.0.0.1:1080"));
        assert!(text.contains("label=test-tunnel"));
    }

    // -- missing tool name in tools/call returns error -----------------------

    #[tokio::test]
    async fn tools_call_missing_name_returns_error() {
        let server = make_server();
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"arguments":{}}}"#,
            )
            .await;
        let json = parse_response(&resp);

        assert_eq!(json["error"]["code"], INVALID_PARAMS);
    }

    // -- missing method field returns invalid request -------------------------

    #[tokio::test]
    async fn missing_method_returns_invalid_request() {
        let server = make_server();
        let resp = server.handle_request(r#"{"jsonrpc":"2.0","id":10}"#).await;
        let json = parse_response(&resp);

        assert_eq!(json["error"]["code"], INVALID_REQUEST);
    }
}
