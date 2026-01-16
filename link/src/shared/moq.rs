//! MoQ (Media over QUIC) types shared between ctl and net.
//!
//! This module is only compiled when the `ctl` or `net` feature is enabled.

use num_enum::{IntoPrimitive, TryFromPrimitive};

/// Maximum length for MoQ namespace.
pub const MAX_MOQ_NAMESPACE_LEN: usize = 64;

/// Maximum length for MoQ track name.
pub const MAX_MOQ_TRACK_NAME_LEN: usize = 64;

/// MoQ example types.
#[derive(Copy, Clone, Debug, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum MoqExampleType {
    /// Clock example - publishes timestamps every second.
    Clock = 0,
    /// Chat example - publishes/subscribes chat messages.
    Chat = 1,
    /// Benchmark example - publishes data at target FPS for throughput testing.
    Benchmark = 2,
}

/// MoQ errors.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum MoqError {
    /// Connection to relay failed.
    ConnectionFailed,
    /// Operation timed out.
    Timeout,
    /// Invalid configuration.
    InvalidConfig,
    /// Track setup failed.
    TrackSetupFailed,
    /// Publish failed.
    PublishFailed,
}
