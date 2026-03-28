//! Payment handler — top layer (handles HTTP 402 flows).
//!
//! When an API returns HTTP 402 Payment Required, the proxy intercepts it,
//! delegates to a [`PaymentHandler`] plugin, pays, and retries with proof.
//! This only works in MITM mode where houdinny can inspect HTTP responses.
//!
//! # Architecture
//!
//! ```text
//! Agent request ──→ API returns 402 ──→ PaymentInterceptor
//!   ├── parse_402() extracts PaymentRequest
//!   ├── finds a handler via can_handle()
//!   ├── handler.pay() returns PaymentProof
//!   └── retry original request with proof header attached
//! ```

use crate::error::{Error, Result};
use async_trait::async_trait;

/// Details extracted from a 402 response.
///
/// Contains all the information a payment handler needs to execute a payment:
/// the destination URL, amount, currency, payment address, network, and the
/// raw headers/body for protocol-specific parsing.
#[derive(Debug, Clone)]
pub struct PaymentRequest {
    /// The URL that returned 402.
    pub url: String,
    /// Payment amount (from response headers/body).
    pub amount: Option<String>,
    /// Currency or token type.
    pub currency: Option<String>,
    /// Payment address/destination.
    pub pay_to: Option<String>,
    /// Payment network (e.g., "x402", "lightning", "zcash").
    pub network: Option<String>,
    /// Raw headers from the 402 response (for protocol-specific parsing).
    pub headers: Vec<(String, String)>,
    /// Raw body from the 402 response.
    pub body: Option<String>,
}

/// Proof that payment was made, attached to the retry request.
///
/// The proxy attaches this as an HTTP header on the retried request so the
/// server can verify payment was completed.
#[derive(Debug, Clone)]
pub struct PaymentProof {
    /// Header name to attach (e.g., "X-Payment-Proof", "Authorization").
    pub header_name: String,
    /// Header value (the proof token/receipt).
    pub header_value: String,
}

/// Payment plugin — top layer (handles 402s).
///
/// Each implementation speaks one payment protocol (x402, Lightning, Zcash, etc.).
/// The [`PaymentInterceptor`] holds multiple handlers and picks the right one
/// based on the 402 response.
#[async_trait]
pub trait PaymentHandler: Send + Sync {
    /// Check if this handler can process the given 402 response.
    fn can_handle(&self, request: &PaymentRequest) -> bool;

    /// Execute the payment and return proof.
    async fn pay(&self, request: &PaymentRequest) -> Result<PaymentProof>;

    /// Human-readable name of this payment method.
    fn name(&self) -> &str;
}

/// Orchestrates the HTTP 402 payment flow.
///
/// Holds a set of [`PaymentHandler`] plugins and, given a 402 response,
/// finds the right handler, executes payment, and returns proof that can
/// be attached to a retry request.
pub struct PaymentInterceptor {
    /// Registered payment handlers, tried in order.
    handlers: Vec<Box<dyn PaymentHandler>>,
    /// Maximum number of payment retry attempts (default: 3).
    max_retries: usize,
}

impl PaymentInterceptor {
    /// Create a new interceptor with no handlers and default settings.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            max_retries: 3,
        }
    }

    /// Register a payment handler plugin.
    pub fn add_handler(&mut self, handler: Box<dyn PaymentHandler>) {
        tracing::info!(name = handler.name(), "registered payment handler");
        self.handlers.push(handler);
    }

    /// Return the maximum number of payment retry attempts.
    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    /// Parse a 402 response into a [`PaymentRequest`].
    ///
    /// Extracts payment details from headers like:
    /// - `X-Payment-Amount`, `X-Payment-Currency`, `X-Payment-Address`
    /// - x402 headers (if present)
    /// - `WWW-Authenticate` for L402
    ///
    /// Returns `None` if the status code is not 402.
    pub fn parse_402(
        status: u16,
        headers: &[(String, String)],
        body: Option<&str>,
        url: &str,
    ) -> Option<PaymentRequest> {
        if status != 402 {
            return None;
        }

        let mut amount: Option<String> = None;
        let mut currency: Option<String> = None;
        let mut pay_to: Option<String> = None;
        let mut network: Option<String> = None;

        for (key, value) in headers {
            let lower = key.to_lowercase();
            match lower.as_str() {
                "x-payment-amount" => amount = Some(value.clone()),
                "x-payment-currency" => currency = Some(value.clone()),
                "x-payment-address" => pay_to = Some(value.clone()),
                "x-payment-network" => network = Some(value.clone()),
                _ => {}
            }

            // Detect x402 protocol from header prefix.
            if lower.starts_with("x-402") && network.is_none() {
                network = Some("x402".to_string());
            }

            // Detect L402 from WWW-Authenticate header.
            if lower == "www-authenticate"
                && value.to_lowercase().starts_with("l402")
                && network.is_none()
            {
                network = Some("l402".to_string());
            }
        }

        Some(PaymentRequest {
            url: url.to_string(),
            amount,
            currency,
            pay_to,
            network,
            headers: headers.to_vec(),
            body: body.map(String::from),
        })
    }

    /// Find a handler that can process this payment and execute it.
    ///
    /// Iterates through registered handlers in order and uses the first one
    /// that returns `true` from [`PaymentHandler::can_handle`].
    pub async fn handle_payment(&self, request: &PaymentRequest) -> Result<PaymentProof> {
        for handler in &self.handlers {
            if handler.can_handle(request) {
                tracing::info!(
                    handler = handler.name(),
                    url = %request.url,
                    "attempting payment"
                );
                match handler.pay(request).await {
                    Ok(proof) => {
                        tracing::info!(
                            handler = handler.name(),
                            url = %request.url,
                            header = %proof.header_name,
                            "payment succeeded"
                        );
                        return Ok(proof);
                    }
                    Err(e) => {
                        tracing::warn!(
                            handler = handler.name(),
                            url = %request.url,
                            error = %e,
                            "payment failed"
                        );
                        return Err(e);
                    }
                }
            }
        }

        Err(Error::Payment(format!(
            "no payment handler available for {}",
            request.url
        )))
    }
}

impl Default for PaymentInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

// ── Dummy implementation ────────────────────────────────────────────────

/// Always succeeds with a fake proof. Useful for testing the 402 flow
/// without real payments.
pub struct DummyPaymentHandler;

#[async_trait]
impl PaymentHandler for DummyPaymentHandler {
    fn can_handle(&self, _request: &PaymentRequest) -> bool {
        true
    }

    async fn pay(&self, request: &PaymentRequest) -> Result<PaymentProof> {
        tracing::info!(url = %request.url, "dummy payment: pretending to pay");

        // Generate a random 16-byte hex string as a dummy proof ID.
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

        Ok(PaymentProof {
            header_name: "X-Payment-Proof".to_string(),
            header_value: format!("dummy-proof-{hex}"),
        })
    }

    fn name(&self) -> &str {
        "dummy"
    }
}

// ── x402 stub ───────────────────────────────────────────────────────────

/// Stub for the x402 payment protocol (Coinbase's standard).
///
/// Parses x402 headers from 402 responses but does not execute real
/// payments yet. Returns an error from [`pay`](PaymentHandler::pay).
pub struct X402PaymentHandler;

#[async_trait]
impl PaymentHandler for X402PaymentHandler {
    fn can_handle(&self, request: &PaymentRequest) -> bool {
        // Check for x402-specific indicators.
        request.network.as_deref() == Some("x402")
            || request
                .headers
                .iter()
                .any(|(k, _)| k.to_lowercase().starts_with("x-402"))
    }

    async fn pay(&self, _request: &PaymentRequest) -> Result<PaymentProof> {
        Err(Error::Payment(
            "x402 payments not yet implemented".to_string(),
        ))
    }

    fn name(&self) -> &str {
        "x402"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DummyPaymentHandler tests ───────────────────────────────────────

    #[test]
    fn dummy_can_handle_returns_true_for_anything() {
        let handler = DummyPaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: None,
            headers: vec![],
            body: None,
        };
        assert!(handler.can_handle(&request));
    }

    #[tokio::test]
    async fn dummy_pay_returns_valid_proof() {
        let handler = DummyPaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: Some("100".to_string()),
            currency: Some("USD".to_string()),
            pay_to: Some("addr_123".to_string()),
            network: None,
            headers: vec![],
            body: None,
        };

        let proof = handler.pay(&request).await.expect("pay should succeed");
        assert_eq!(proof.header_name, "X-Payment-Proof");
        assert!(proof.header_value.starts_with("dummy-proof-"));
        // The hex part should be 32 chars (16 bytes * 2 hex chars each).
        let hex_part = proof.header_value.strip_prefix("dummy-proof-").unwrap();
        assert_eq!(hex_part.len(), 32);
    }

    #[test]
    fn dummy_name() {
        let handler = DummyPaymentHandler;
        assert_eq!(handler.name(), "dummy");
    }

    // ── X402PaymentHandler tests ────────────────────────────────────────

    #[test]
    fn x402_can_handle_with_network_field() {
        let handler = X402PaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: Some("x402".to_string()),
            headers: vec![],
            body: None,
        };
        assert!(handler.can_handle(&request));
    }

    #[test]
    fn x402_can_handle_with_x402_headers() {
        let handler = X402PaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: None,
            headers: vec![("X-402-Payment-Required".to_string(), "true".to_string())],
            body: None,
        };
        assert!(handler.can_handle(&request));
    }

    #[test]
    fn x402_cannot_handle_generic_request() {
        let handler = X402PaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: Some("100".to_string()),
            currency: Some("USD".to_string()),
            pay_to: None,
            network: None,
            headers: vec![
                ("X-Payment-Amount".to_string(), "100".to_string()),
                ("X-Payment-Currency".to_string(), "USD".to_string()),
            ],
            body: None,
        };
        assert!(!handler.can_handle(&request));
    }

    #[tokio::test]
    async fn x402_pay_returns_not_implemented_error() {
        let handler = X402PaymentHandler;
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: Some("x402".to_string()),
            headers: vec![],
            body: None,
        };

        let result = handler.pay(&request).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("not yet implemented"),
            "error should mention not yet implemented, got: {err}"
        );
    }

    #[test]
    fn x402_name() {
        let handler = X402PaymentHandler;
        assert_eq!(handler.name(), "x402");
    }

    // ── PaymentInterceptor tests ────────────────────────────────────────

    #[tokio::test]
    async fn interceptor_with_dummy_handler_processes_payment() {
        let mut interceptor = PaymentInterceptor::new();
        interceptor.add_handler(Box::new(DummyPaymentHandler));

        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: Some("50".to_string()),
            currency: Some("USD".to_string()),
            pay_to: None,
            network: None,
            headers: vec![],
            body: None,
        };

        let proof = interceptor
            .handle_payment(&request)
            .await
            .expect("payment should succeed");
        assert_eq!(proof.header_name, "X-Payment-Proof");
        assert!(proof.header_value.starts_with("dummy-proof-"));
    }

    #[tokio::test]
    async fn interceptor_with_no_handlers_returns_error() {
        let interceptor = PaymentInterceptor::new();

        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: None,
            headers: vec![],
            body: None,
        };

        let result = interceptor.handle_payment(&request).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("no payment handler available"),
            "error should mention no handler, got: {err}"
        );
    }

    #[tokio::test]
    async fn interceptor_picks_first_matching_handler() {
        let mut interceptor = PaymentInterceptor::new();
        // x402 handler first — it only handles x402 requests.
        interceptor.add_handler(Box::new(X402PaymentHandler));
        // Dummy handler second — it handles anything.
        interceptor.add_handler(Box::new(DummyPaymentHandler));

        // A generic request should skip x402 and use dummy.
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: None,
            headers: vec![],
            body: None,
        };

        let proof = interceptor
            .handle_payment(&request)
            .await
            .expect("dummy handler should succeed");
        assert!(proof.header_value.starts_with("dummy-proof-"));
    }

    #[tokio::test]
    async fn interceptor_x402_handler_returns_error_for_x402_request() {
        let mut interceptor = PaymentInterceptor::new();
        interceptor.add_handler(Box::new(X402PaymentHandler));
        interceptor.add_handler(Box::new(DummyPaymentHandler));

        // An x402 request should be matched by x402 handler (which errors).
        let request = PaymentRequest {
            url: "https://api.example.com/resource".to_string(),
            amount: None,
            currency: None,
            pay_to: None,
            network: Some("x402".to_string()),
            headers: vec![],
            body: None,
        };

        let result = interceptor.handle_payment(&request).await;
        assert!(result.is_err());
    }

    // ── parse_402 tests ─────────────────────────────────────────────────

    #[test]
    fn parse_402_extracts_payment_details_from_headers() {
        let headers = vec![
            ("X-Payment-Amount".to_string(), "100".to_string()),
            ("X-Payment-Currency".to_string(), "USDC".to_string()),
            ("X-Payment-Address".to_string(), "0xabc123".to_string()),
            ("X-Payment-Network".to_string(), "ethereum".to_string()),
        ];

        let result = PaymentInterceptor::parse_402(
            402,
            &headers,
            Some("payment required"),
            "https://api.example.com/data",
        );

        let req = result.expect("should parse 402");
        assert_eq!(req.url, "https://api.example.com/data");
        assert_eq!(req.amount.as_deref(), Some("100"));
        assert_eq!(req.currency.as_deref(), Some("USDC"));
        assert_eq!(req.pay_to.as_deref(), Some("0xabc123"));
        assert_eq!(req.network.as_deref(), Some("ethereum"));
        assert_eq!(req.body.as_deref(), Some("payment required"));
        assert_eq!(req.headers.len(), 4);
    }

    #[test]
    fn parse_402_returns_none_for_non_402() {
        let headers = vec![("X-Payment-Amount".to_string(), "100".to_string())];

        assert!(
            PaymentInterceptor::parse_402(200, &headers, None, "https://api.example.com/data")
                .is_none()
        );

        assert!(
            PaymentInterceptor::parse_402(404, &headers, None, "https://api.example.com/data")
                .is_none()
        );

        assert!(
            PaymentInterceptor::parse_402(500, &headers, None, "https://api.example.com/data")
                .is_none()
        );
    }

    #[test]
    fn parse_402_detects_x402_network_from_headers() {
        let headers = vec![("X-402-Version".to_string(), "1.0".to_string())];

        let req =
            PaymentInterceptor::parse_402(402, &headers, None, "https://api.example.com/data")
                .expect("should parse 402");

        assert_eq!(req.network.as_deref(), Some("x402"));
    }

    #[test]
    fn parse_402_detects_l402_from_www_authenticate() {
        let headers = vec![(
            "WWW-Authenticate".to_string(),
            "L402 macaroon=abc invoice=xyz".to_string(),
        )];

        let req =
            PaymentInterceptor::parse_402(402, &headers, None, "https://api.example.com/data")
                .expect("should parse 402");

        assert_eq!(req.network.as_deref(), Some("l402"));
    }

    #[test]
    fn parse_402_with_empty_headers_and_no_body() {
        let req = PaymentInterceptor::parse_402(402, &[], None, "https://api.example.com/data")
            .expect("should parse 402 even with no details");

        assert_eq!(req.url, "https://api.example.com/data");
        assert!(req.amount.is_none());
        assert!(req.currency.is_none());
        assert!(req.pay_to.is_none());
        assert!(req.network.is_none());
        assert!(req.body.is_none());
    }

    #[test]
    fn parse_402_explicit_network_takes_precedence_over_header_detection() {
        // When X-Payment-Network is set explicitly, it should take precedence
        // even if x402 headers are also present.
        let headers = vec![
            ("X-Payment-Network".to_string(), "lightning".to_string()),
            ("X-402-Version".to_string(), "1.0".to_string()),
        ];

        let req =
            PaymentInterceptor::parse_402(402, &headers, None, "https://api.example.com/data")
                .expect("should parse 402");

        // Explicit network header wins.
        assert_eq!(req.network.as_deref(), Some("lightning"));
    }

    #[test]
    fn interceptor_default_max_retries() {
        let interceptor = PaymentInterceptor::new();
        assert_eq!(interceptor.max_retries(), 3);
    }
}
