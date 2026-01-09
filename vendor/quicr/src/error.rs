//! Error types for the quicr crate
//!
//! All errors are compatible with both std and no_std environments.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(feature = "std")]
use std::ffi::CStr;
#[cfg(not(feature = "std"))]
use core::ffi::CStr;

#[cfg(not(feature = "std"))]
use alloc::string::{String, ToString};
#[cfg(feature = "std")]
use std::string::String;

use thiserror::Error;

/// Result type alias for quicr operations
pub type Result<T> = core::result::Result<T, Error>;

/// Errors that can occur in quicr operations
#[derive(Error, Debug)]
pub enum Error {
    /// Client is not connected
    #[error("client is not connected")]
    NotConnected,

    /// Connection failed
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Configuration error
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// Track not found
    #[error("track not found")]
    TrackNotFound,

    /// Not authorized
    #[error("not authorized")]
    NotAuthorized,

    /// Publish error
    #[error("publish error: {0}")]
    PublishError(String),

    /// Subscribe error
    #[error("subscribe error: {0}")]
    SubscribeError(String),

    /// Internal error from libquicr
    #[error("internal error: {0}")]
    Internal(String),

    /// FFI error
    #[error("ffi error: {0}")]
    Ffi(String),

    /// Timeout error
    #[error("operation timed out")]
    Timeout,

    /// Channel closed
    #[error("channel closed")]
    ChannelClosed,

    /// Invalid argument
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Resource exhausted
    #[error("resource exhausted")]
    ResourceExhausted,

    /// Invalid state
    #[error("invalid state: {0}")]
    InvalidState(String),
}

impl Error {
    /// Create an error from the last FFI error message
    pub(crate) fn from_ffi() -> Self {
        // Safety: quicr_last_error returns a pointer to a static or thread-local string
        let msg = unsafe {
            let ptr = crate::ffi::quicr_last_error();
            if ptr.is_null() {
                "unknown error".to_string()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        Error::Ffi(msg)
    }

    /// Check if the error is recoverable (can retry)
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Error::Timeout | Error::NotConnected | Error::ChannelClosed
        )
    }

    // Convenience constructors for no_std compatibility

    /// Create a "not connected" error
    #[inline]
    pub fn not_connected() -> Self {
        Self::NotConnected
    }

    /// Create a "connection failed" error
    pub fn connection_failed(reason: &str) -> Self {
        Self::ConnectionFailed(reason.into())
    }

    /// Create a "config error"
    pub fn config(msg: &str) -> Self {
        Self::ConfigError(msg.into())
    }

    /// Create a "track not found" error
    #[inline]
    pub fn track_not_found() -> Self {
        Self::TrackNotFound
    }

    /// Create a "not authorized" error
    #[inline]
    pub fn not_authorized() -> Self {
        Self::NotAuthorized
    }

    /// Create a "publish error"
    pub fn publish(msg: &str) -> Self {
        Self::PublishError(msg.into())
    }

    /// Create a "subscribe error"
    pub fn subscribe(msg: &str) -> Self {
        Self::SubscribeError(msg.into())
    }

    /// Create an "internal error"
    pub fn internal(msg: &str) -> Self {
        Self::Internal(msg.into())
    }

    /// Create a "timeout" error
    #[inline]
    pub fn timeout() -> Self {
        Self::Timeout
    }

    /// Create a "channel closed" error
    #[inline]
    pub fn channel_closed() -> Self {
        Self::ChannelClosed
    }

    /// Create an "invalid argument" error
    pub fn invalid_argument(msg: &str) -> Self {
        Self::InvalidArgument(msg.into())
    }

    /// Create a "resource exhausted" error
    #[inline]
    pub fn resource_exhausted() -> Self {
        Self::ResourceExhausted
    }

    /// Create an "invalid state" error
    pub fn invalid_state(msg: &str) -> Self {
        Self::InvalidState(msg.into())
    }
}

// Convert from FFI status codes
impl From<crate::ffi::QuicrStatus> for Error {
    fn from(status: crate::ffi::QuicrStatus) -> Self {
        match status {
            crate::ffi::QuicrStatus_QUICR_STATUS_OK => {
                Error::Internal("unexpected OK status converted to error".into())
            }
            crate::ffi::QuicrStatus_QUICR_STATUS_DISCONNECTED => Error::NotConnected,
            crate::ffi::QuicrStatus_QUICR_STATUS_ERROR => Error::from_ffi(),
            crate::ffi::QuicrStatus_QUICR_STATUS_IDLE_TIMEOUT => Error::Timeout,
            _ => Error::from_ffi(),
        }
    }
}

// Convert from Embassy timeout error
impl From<embassy_time::TimeoutError> for Error {
    fn from(_: embassy_time::TimeoutError) -> Self {
        Self::Timeout
    }
}
