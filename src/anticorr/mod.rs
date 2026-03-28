//! Anti-correlation engine for houdinny.
//!
//! Rotating IPs alone is insufficient — a sophisticated observer can still
//! correlate requests by timing, packet size, and traffic patterns. This
//! module provides configurable countermeasures:
//!
//! - **Timing jitter** — random delay before forwarding a request.
//! - **Packet padding** — normalize request sizes so they all look alike.
//!
//! All features are **opt-in** (disabled by default). Privacy costs
//! performance, so callers enable only what they need.
//!
//! # Examples
//!
//! ```rust
//! use houdinny::anticorr::{AntiCorrelationConfig, AntiCorrelationLayer};
//!
//! // Everything disabled by default.
//! let config = AntiCorrelationConfig::default();
//! let layer = AntiCorrelationLayer::from_config(&config);
//! ```

use std::time::Duration;

use rand::rngs::OsRng;
use rand::{Rng, TryRngCore as _};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Serde default helpers
// ---------------------------------------------------------------------------

/// Default minimum jitter: 0 ms (no minimum).
fn default_min_ms() -> u64 {
    0
}

/// Default maximum jitter: 2000 ms.
fn default_max_ms() -> u64 {
    2000
}

/// Default padding target size: 1024 bytes.
fn default_target_size() -> usize {
    1024
}

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Top-level anti-correlation configuration.
///
/// Combines jitter and padding settings. All features are disabled by default.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AntiCorrelationConfig {
    /// Timing jitter configuration.
    #[serde(default)]
    pub jitter: JitterConfig,

    /// Packet padding configuration.
    #[serde(default)]
    pub padding: PaddingConfig,
}

/// Configuration for timing jitter.
#[derive(Debug, Clone, Deserialize)]
pub struct JitterConfig {
    /// Whether timing jitter is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Minimum jitter in milliseconds (default 0).
    #[serde(default = "default_min_ms")]
    pub min_ms: u64,

    /// Maximum jitter in milliseconds (default 2000).
    #[serde(default = "default_max_ms")]
    pub max_ms: u64,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_ms: default_min_ms(),
            max_ms: default_max_ms(),
        }
    }
}

/// Configuration for packet padding.
#[derive(Debug, Clone, Deserialize)]
pub struct PaddingConfig {
    /// Whether packet padding is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Target size in bytes to pad outgoing data to (default 1024).
    #[serde(default = "default_target_size")]
    pub target_size: usize,
}

impl Default for PaddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_size: default_target_size(),
        }
    }
}

// ---------------------------------------------------------------------------
// TimingJitter
// ---------------------------------------------------------------------------

/// Adds a random delay before forwarding a request.
///
/// Uses [`OsRng`] for security-sensitive randomness and [`tokio::time::sleep`]
/// for the actual delay. When disabled, [`apply`](Self::apply) returns
/// immediately with a zero duration.
#[derive(Debug, Clone)]
pub struct TimingJitter {
    /// Maximum jitter in milliseconds.
    max_ms: u64,
    /// Minimum jitter in milliseconds (0 = no minimum).
    min_ms: u64,
    /// Whether jitter is enabled.
    enabled: bool,
}

impl TimingJitter {
    /// Create a new [`TimingJitter`] with the given range.
    ///
    /// # Panics
    ///
    /// Does **not** panic. If `min_ms > max_ms` the range is effectively
    /// clamped at runtime (the random range becomes `max_ms..=max_ms`).
    pub fn new(min_ms: u64, max_ms: u64) -> Self {
        Self {
            max_ms,
            min_ms,
            enabled: true,
        }
    }

    /// Create a disabled [`TimingJitter`] that always returns zero delay.
    pub fn disabled() -> Self {
        Self {
            max_ms: 0,
            min_ms: 0,
            enabled: false,
        }
    }

    /// Apply jitter — sleeps for a random duration between `min_ms` and `max_ms`.
    ///
    /// Returns the actual delay applied. If disabled, returns [`Duration::ZERO`]
    /// immediately.
    pub async fn apply(&self) -> Duration {
        if !self.enabled {
            return Duration::ZERO;
        }

        // Clamp so min <= max.
        let lo = self.min_ms;
        let hi = self.max_ms.max(lo);

        let delay_ms = if lo == hi {
            lo
        } else {
            OsRng.unwrap_err().random_range(lo..=hi)
        };

        let delay = Duration::from_millis(delay_ms);

        tracing::debug!(delay_ms, "applying timing jitter");
        tokio::time::sleep(delay).await;

        delay
    }
}

// ---------------------------------------------------------------------------
// PacketPadder
// ---------------------------------------------------------------------------

/// Normalizes outgoing data sizes by padding to a fixed target.
///
/// Padding uses a simple scheme: the last 4 bytes of padded output encode
/// the original data length as a big-endian `u32`. This allows the receiver
/// to strip padding without external metadata.
///
/// When disabled, [`pad`](Self::pad) and [`unpad`](Self::unpad) pass data
/// through unchanged.
#[derive(Debug, Clone)]
pub struct PacketPadder {
    /// Target size to pad to (requests smaller than this get padded).
    target_size: usize,
    /// Padding byte (e.g., `0x00` or space).
    pad_byte: u8,
    /// Whether padding is enabled.
    enabled: bool,
}

/// Size of the length trailer appended during padding (4 bytes for a `u32`).
const LENGTH_TRAILER_SIZE: usize = 4;

impl PacketPadder {
    /// Create a new [`PacketPadder`] with the given target size.
    ///
    /// Uses `0x00` as the default padding byte.
    pub fn new(target_size: usize) -> Self {
        Self {
            target_size,
            pad_byte: 0x00,
            enabled: true,
        }
    }

    /// Create a disabled [`PacketPadder`] that passes data through unchanged.
    pub fn disabled() -> Self {
        Self {
            target_size: 0,
            pad_byte: 0x00,
            enabled: false,
        }
    }

    /// Pad data to `target_size`.
    ///
    /// The padded output has the original data, then padding bytes, then a
    /// 4-byte big-endian length trailer encoding the original data length.
    /// If the data (plus the 4-byte trailer) is already `>= target_size`,
    /// returns the data with only the trailer appended (no padding added).
    pub fn pad(&self, data: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return data.to_vec();
        }

        let original_len = data.len();
        // We need room for the original data + length trailer.
        let needed = original_len + LENGTH_TRAILER_SIZE;

        let total = if needed >= self.target_size {
            // Already at or above target — just append the trailer.
            needed
        } else {
            self.target_size
        };

        let padding_bytes = total - needed;

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(data);
        out.resize(out.len() + padding_bytes, self.pad_byte);
        out.extend_from_slice(&(original_len as u32).to_be_bytes());

        tracing::debug!(
            original_len,
            padded_len = out.len(),
            padding_added = padding_bytes,
            "padded outgoing data"
        );

        out
    }

    /// Remove padding from received data.
    ///
    /// Reads the 4-byte big-endian length trailer at the end, then returns
    /// only the original data slice. If disabled or the data is too short
    /// to contain a trailer, returns the data unchanged.
    pub fn unpad<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        if !self.enabled {
            return data;
        }

        if data.len() < LENGTH_TRAILER_SIZE {
            return data;
        }

        let trailer_start = data.len() - LENGTH_TRAILER_SIZE;
        let original_len = u32::from_be_bytes([
            data[trailer_start],
            data[trailer_start + 1],
            data[trailer_start + 2],
            data[trailer_start + 3],
        ]) as usize;

        if original_len > trailer_start {
            // Trailer claims a length larger than available data — malformed.
            // Return as-is to avoid panics.
            tracing::debug!(
                original_len,
                available = trailer_start,
                "unpad: trailer length exceeds available data, returning raw"
            );
            return data;
        }

        &data[..original_len]
    }
}

// ---------------------------------------------------------------------------
// AntiCorrStats
// ---------------------------------------------------------------------------

/// Statistics from applying anti-correlation measures to a single request.
#[derive(Debug, Clone)]
pub struct AntiCorrStats {
    /// How much timing jitter was applied.
    pub jitter_applied: Duration,
    /// How many padding bytes were added (0 if padding disabled).
    pub padding_added: usize,
}

// ---------------------------------------------------------------------------
// AntiCorrelationLayer
// ---------------------------------------------------------------------------

/// Applies all configured anti-correlation measures.
///
/// Combines [`TimingJitter`] and [`PacketPadder`] into a single layer that
/// can be inserted into the request pipeline.
pub struct AntiCorrelationLayer {
    jitter: TimingJitter,
    padder: PacketPadder,
}

impl AntiCorrelationLayer {
    /// Build from an [`AntiCorrelationConfig`].
    pub fn from_config(config: &AntiCorrelationConfig) -> Self {
        let jitter = if config.jitter.enabled {
            TimingJitter::new(config.jitter.min_ms, config.jitter.max_ms)
        } else {
            TimingJitter::disabled()
        };

        let padder = if config.padding.enabled {
            PacketPadder::new(config.padding.target_size)
        } else {
            PacketPadder::disabled()
        };

        Self { jitter, padder }
    }

    /// Build a fully-disabled layer (no jitter, no padding).
    pub fn disabled() -> Self {
        Self {
            jitter: TimingJitter::disabled(),
            padder: PacketPadder::disabled(),
        }
    }

    /// Apply pre-request anti-correlation (jitter before sending).
    ///
    /// Returns [`AntiCorrStats`] describing what was applied.
    pub async fn pre_request(&self) -> AntiCorrStats {
        let jitter_applied = self.jitter.apply().await;
        AntiCorrStats {
            jitter_applied,
            padding_added: 0,
        }
    }

    /// Pad outgoing data to the configured target size.
    pub fn pad_outgoing(&self, data: &[u8]) -> Vec<u8> {
        self.padder.pad(data)
    }

    /// Remove padding from incoming data.
    pub fn unpad_incoming<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        self.padder.unpad(data)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- TimingJitter tests --------------------------------------------------

    #[tokio::test]
    async fn jitter_applies_delay_within_range() {
        // Use a narrow range to keep the test fast.
        let jitter = TimingJitter::new(10, 50);
        let delay = jitter.apply().await;
        assert!(
            delay >= Duration::from_millis(10),
            "delay {delay:?} should be >= 10ms"
        );
        assert!(
            delay <= Duration::from_millis(50),
            "delay {delay:?} should be <= 50ms"
        );
    }

    #[tokio::test]
    async fn jitter_disabled_returns_zero() {
        let jitter = TimingJitter::disabled();
        let delay = jitter.apply().await;
        assert_eq!(delay, Duration::ZERO);
    }

    #[tokio::test]
    async fn jitter_equal_min_max() {
        let jitter = TimingJitter::new(25, 25);
        let delay = jitter.apply().await;
        assert_eq!(delay, Duration::from_millis(25));
    }

    #[tokio::test]
    async fn jitter_min_greater_than_max_clamps() {
        // min > max: should clamp to max (i.e. use max as both bounds).
        let jitter = TimingJitter::new(100, 50);
        let delay = jitter.apply().await;
        // After clamping, lo=100, hi=max(50,100)=100, so delay=100.
        assert_eq!(delay, Duration::from_millis(100));
    }

    // -- PacketPadder tests --------------------------------------------------

    #[test]
    fn padder_pads_short_data_to_target() {
        let padder = PacketPadder::new(64);
        let data = b"hello";
        let padded = padder.pad(data);
        assert_eq!(padded.len(), 64, "padded length should be target_size");
        // First 5 bytes are the original data.
        assert_eq!(&padded[..5], b"hello");
    }

    #[test]
    fn padder_does_not_shrink_large_data() {
        let padder = PacketPadder::new(16);
        let data = vec![0xAA; 100];
        let padded = padder.pad(&data);
        // 100 bytes data + 4 bytes trailer = 104
        assert_eq!(padded.len(), 104);
        assert_eq!(&padded[..100], &data[..]);
    }

    #[test]
    fn padder_unpad_strips_padding() {
        let padder = PacketPadder::new(128);
        let original = b"secret agent data";
        let padded = padder.pad(original);
        assert_eq!(padded.len(), 128);

        let unpadded = padder.unpad(&padded);
        assert_eq!(unpadded, original);
    }

    #[test]
    fn padder_round_trip_large_data() {
        let padder = PacketPadder::new(32);
        let original = vec![0x42; 256];
        let padded = padder.pad(&original);
        let unpadded = padder.unpad(&padded);
        assert_eq!(unpadded, &original[..]);
    }

    #[test]
    fn padder_disabled_returns_unchanged() {
        let padder = PacketPadder::disabled();
        let data = b"no padding please";
        let padded = padder.pad(data);
        assert_eq!(padded, data);

        let unpadded = padder.unpad(data);
        assert_eq!(unpadded, data);
    }

    #[test]
    fn padder_unpad_short_data_returns_unchanged() {
        let padder = PacketPadder::new(128);
        // Data shorter than the 4-byte trailer — can't unpad.
        let data = &[1u8, 2, 3];
        let unpadded = padder.unpad(data);
        assert_eq!(unpadded, data);
    }

    #[test]
    fn padder_unpad_malformed_trailer_returns_raw() {
        let padder = PacketPadder::new(128);
        // Craft data where the trailer claims a length > available data.
        let mut bad = vec![0u8; 10];
        // Write trailer claiming original length was 255 (impossible).
        bad.extend_from_slice(&255u32.to_be_bytes());
        let unpadded = padder.unpad(&bad);
        assert_eq!(unpadded.len(), bad.len(), "malformed data returned as-is");
    }

    // -- AntiCorrelationLayer tests ------------------------------------------

    #[tokio::test]
    async fn layer_from_config_disabled() {
        let config = AntiCorrelationConfig::default();
        let layer = AntiCorrelationLayer::from_config(&config);

        let stats = layer.pre_request().await;
        assert_eq!(stats.jitter_applied, Duration::ZERO);

        let data = b"test data";
        let padded = layer.pad_outgoing(data);
        assert_eq!(padded, data, "padding disabled should pass through");

        let unpadded = layer.unpad_incoming(data);
        assert_eq!(unpadded, data, "unpad disabled should pass through");
    }

    #[tokio::test]
    async fn layer_from_config_enabled() {
        let config = AntiCorrelationConfig {
            jitter: JitterConfig {
                enabled: true,
                min_ms: 5,
                max_ms: 20,
            },
            padding: PaddingConfig {
                enabled: true,
                target_size: 256,
            },
        };

        let layer = AntiCorrelationLayer::from_config(&config);

        // Jitter should produce a non-zero delay.
        let stats = layer.pre_request().await;
        assert!(stats.jitter_applied >= Duration::from_millis(5));
        assert!(stats.jitter_applied <= Duration::from_millis(20));

        // Padding should work.
        let data = b"payload";
        let padded = layer.pad_outgoing(data);
        assert_eq!(padded.len(), 256);

        let unpadded = layer.unpad_incoming(&padded);
        assert_eq!(unpadded, data);
    }

    #[tokio::test]
    async fn layer_disabled_constructor() {
        let layer = AntiCorrelationLayer::disabled();
        let stats = layer.pre_request().await;
        assert_eq!(stats.jitter_applied, Duration::ZERO);
        assert_eq!(stats.padding_added, 0);
    }

    // -- Config deserialization tests ----------------------------------------

    #[test]
    fn config_deserialize_empty_toml() {
        let config: AntiCorrelationConfig =
            toml::from_str("").expect("empty TOML should use defaults");
        assert!(!config.jitter.enabled);
        assert_eq!(config.jitter.min_ms, 0);
        assert_eq!(config.jitter.max_ms, 2000);
        assert!(!config.padding.enabled);
        assert_eq!(config.padding.target_size, 1024);
    }

    #[test]
    fn config_deserialize_full_toml() {
        let toml_str = r#"
[jitter]
enabled = true
min_ms = 100
max_ms = 500

[padding]
enabled = true
target_size = 2048
"#;
        let config: AntiCorrelationConfig =
            toml::from_str(toml_str).expect("full TOML should parse");
        assert!(config.jitter.enabled);
        assert_eq!(config.jitter.min_ms, 100);
        assert_eq!(config.jitter.max_ms, 500);
        assert!(config.padding.enabled);
        assert_eq!(config.padding.target_size, 2048);
    }

    #[test]
    fn config_deserialize_partial_toml() {
        let toml_str = r#"
[jitter]
enabled = true
"#;
        let config: AntiCorrelationConfig =
            toml::from_str(toml_str).expect("partial TOML should parse");
        assert!(config.jitter.enabled);
        assert_eq!(config.jitter.min_ms, 0);
        assert_eq!(config.jitter.max_ms, 2000);
        assert!(!config.padding.enabled);
        assert_eq!(config.padding.target_size, 1024);
    }

    #[test]
    fn jitter_config_default() {
        let jc = JitterConfig::default();
        assert!(!jc.enabled);
        assert_eq!(jc.min_ms, 0);
        assert_eq!(jc.max_ms, 2000);
    }

    #[test]
    fn padding_config_default() {
        let pc = PaddingConfig::default();
        assert!(!pc.enabled);
        assert_eq!(pc.target_size, 1024);
    }
}
