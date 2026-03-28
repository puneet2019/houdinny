//! houdinny — Privacy proxy for AI agents
//!
//! Rotates requests across tunnel pools with automatic payment handling.
//! Any agent in any language just points `HTTP_PROXY` at it.

pub mod admin;
pub mod anticorr;
pub mod config;
pub mod error;
pub mod mcp;
pub mod payment;
pub mod pool;
pub mod proxy;
pub mod relay;
pub mod route;
pub mod router;
pub mod transport;
