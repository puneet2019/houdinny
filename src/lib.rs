//! houdinny — Privacy proxy for AI agents
//!
//! Rotates requests across tunnel pools with automatic payment handling.
//! Any agent in any language just points `HTTP_PROXY` at it.

pub mod config;
pub mod error;
pub mod pool;
pub mod proxy;
pub mod router;
pub mod transport;
