/// Core error types for houdinny.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no healthy tunnels available")]
    NoHealthyTunnels,

    #[error("tunnel connection failed: {0}")]
    TunnelConnect(String),

    #[error("tunnel provisioning failed: {0}")]
    TunnelProvision(String),

    #[error("proxy error: {0}")]
    Proxy(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("payment error: {0}")]
    Payment(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
