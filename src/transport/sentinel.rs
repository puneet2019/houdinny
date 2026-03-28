//! Sentinel dVPN transport — decentralized VPN with on-chain session payments.
//!
//! This is a stub implementation. The provision() method documents the
//! on-chain flow but returns an error until the Sentinel SDK is integrated.
//!
//! ## Flow (once implemented)
//!
//! 1. Pay DVPN tokens on-chain -> get session credentials
//! 2. Establish WireGuard tunnel with those credentials
//! 3. Route traffic through the tunnel
//! 4. Session expires -> pay again or close

use super::{AsyncReadWrite, Transport};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

/// Sentinel dVPN transport — decentralized VPN with on-chain session payments.
///
/// This is a stub implementation. The provision() method documents the
/// on-chain flow but returns an error until the Sentinel SDK is integrated.
pub struct SentinelTransport {
    /// On-chain node address (e.g., "sentnode1qx...")
    node: String,
    /// Payment denomination (e.g., "udvpn")
    denom: String,
    /// Session deposit amount (e.g., "1000000udvpn")
    deposit: String,
    /// Path to wallet key file
    wallet_key_path: String,
    label: String,
    id: String,
    healthy: AtomicBool,
    /// Once provisioned, this holds the WireGuard config for the session
    session: RwLock<Option<SentinelSession>>,
}

/// Holds WireGuard session credentials obtained from a Sentinel node
/// after a successful on-chain deposit.
pub struct SentinelSession {
    /// WireGuard endpoint obtained from the node
    pub wg_endpoint: String,
    /// WireGuard private key for this session
    pub wg_private_key: String,
    /// WireGuard public key of the node
    pub wg_public_key: String,
    /// Session ID on-chain
    pub session_id: String,
    /// When this session expires
    pub expires_at: Option<String>,
}

impl SentinelTransport {
    /// Create a new Sentinel dVPN transport (stub).
    ///
    /// # Arguments
    ///
    /// * `node` — on-chain node address (e.g. `"sentnode1qx..."`)
    /// * `denom` — payment denomination (e.g. `"udvpn"`)
    /// * `deposit` — session deposit amount (e.g. `"1000000udvpn"`)
    /// * `wallet_key_path` — path to the wallet key file
    /// * `label` — human-readable label for logging
    pub fn new(
        node: impl Into<String>,
        denom: impl Into<String>,
        deposit: impl Into<String>,
        wallet_key_path: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        let node = node.into();
        let denom = denom.into();
        let deposit = deposit.into();
        let wallet_key_path = wallet_key_path.into();
        let label = label.into();
        let id = format!("sentinel-{label}");
        Self {
            node,
            denom,
            deposit,
            wallet_key_path,
            label,
            id,
            healthy: AtomicBool::new(false),
            session: RwLock::new(None),
        }
    }

    /// Returns the on-chain node address.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Returns the payment denomination.
    pub fn denom(&self) -> &str {
        &self.denom
    }

    /// Returns the session deposit amount.
    pub fn deposit(&self) -> &str {
        &self.deposit
    }

    /// Returns the wallet key path.
    pub fn wallet_key_path(&self) -> &str {
        &self.wallet_key_path
    }
}

#[async_trait]
impl Transport for SentinelTransport {
    async fn provision(&mut self) -> Result<()> {
        // The real flow would be:
        // 1. Query the Sentinel node for pricing/availability
        // 2. Create an on-chain transaction to deposit DVPN tokens
        // 3. Wait for transaction confirmation
        // 4. Receive WireGuard credentials from the node
        // 5. Store credentials in self.session
        //
        // For now, return an error indicating this is not yet implemented.
        Err(Error::TunnelProvision(
            "Sentinel dVPN provisioning not yet implemented — requires Cosmos SDK integration"
                .into(),
        ))
    }

    async fn connect(&self, _addr: &str, _port: u16) -> Result<Box<dyn AsyncReadWrite>> {
        // Would use the WireGuard credentials from self.session
        // to connect through the Sentinel node's tunnel
        Err(Error::TunnelConnect(
            "Sentinel dVPN not yet implemented".into(),
        ))
    }

    fn healthy(&self) -> bool {
        // Never healthy until provisioned
        self.healthy.load(Ordering::Relaxed)
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn protocol(&self) -> &str {
        "sentinel"
    }

    async fn close(&self) -> Result<()> {
        self.healthy.store(false, Ordering::Relaxed);
        // The real implementation would:
        // 1. Close the WireGuard tunnel
        // 2. Optionally end the on-chain session to reclaim unused deposit
        let mut session = self.session.write().await;
        *session = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_transport() -> SentinelTransport {
        SentinelTransport::new(
            "sentnode1qxtest123",
            "udvpn",
            "1000000udvpn",
            "~/.houdinny/sentinel_key",
            "test-sentinel",
        )
    }

    #[test]
    fn construction_and_accessors() {
        let t = make_transport();
        assert_eq!(t.node(), "sentnode1qxtest123");
        assert_eq!(t.denom(), "udvpn");
        assert_eq!(t.deposit(), "1000000udvpn");
        assert_eq!(t.wallet_key_path(), "~/.houdinny/sentinel_key");
        assert_eq!(t.label(), "test-sentinel");
        assert_eq!(t.id(), "sentinel-test-sentinel");
    }

    #[tokio::test]
    async fn provision_returns_not_implemented() {
        let mut t = make_transport();
        let result = t.provision().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not yet implemented"),
            "expected 'not yet implemented' in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn connect_returns_not_implemented() {
        let t = make_transport();
        let result: std::result::Result<Box<dyn AsyncReadWrite>, Error> =
            t.connect("example.com", 443).await;
        match result {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("not yet implemented"),
                    "expected 'not yet implemented' in error, got: {msg}"
                );
            }
            Ok(_) => panic!("expected connect to return an error"),
        }
    }

    #[test]
    fn healthy_returns_false() {
        let t = make_transport();
        assert!(!t.healthy());
    }

    #[test]
    fn protocol_returns_sentinel() {
        let t = make_transport();
        assert_eq!(t.protocol(), "sentinel");
    }

    #[test]
    fn session_struct_fields_accessible() {
        let session = SentinelSession {
            wg_endpoint: "1.2.3.4:51820".to_string(),
            wg_private_key: "privkey123".to_string(),
            wg_public_key: "pubkey456".to_string(),
            session_id: "session-789".to_string(),
            expires_at: Some("2026-04-01T00:00:00Z".to_string()),
        };
        assert_eq!(session.wg_endpoint, "1.2.3.4:51820");
        assert_eq!(session.wg_private_key, "privkey123");
        assert_eq!(session.wg_public_key, "pubkey456");
        assert_eq!(session.session_id, "session-789");
        assert_eq!(session.expires_at.as_deref(), Some("2026-04-01T00:00:00Z"));
    }

    #[test]
    fn session_expires_at_can_be_none() {
        let session = SentinelSession {
            wg_endpoint: "1.2.3.4:51820".to_string(),
            wg_private_key: "privkey123".to_string(),
            wg_public_key: "pubkey456".to_string(),
            session_id: "session-789".to_string(),
            expires_at: None,
        };
        assert!(session.expires_at.is_none());
    }

    #[tokio::test]
    async fn close_succeeds() {
        let t = make_transport();
        let result = t.close().await;
        assert!(result.is_ok());
    }
}
