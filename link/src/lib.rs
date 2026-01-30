//! Link - Multi-chip communication framework.
//!
//! This crate provides the core application logic for the Link multi-chip system.
//! The code is `no_std` compatible for embedded use.
//!
//! # Features
//!
//! Each chip has its own feature gate:
//! - `mgmt` - MGMT chip module
//! - `net` - NET chip module
//! - `ui` - UI chip module
//! - `ctl` - Host-side control module (enables `std`)
//! - `all` - All modules (default for development/testing)

#![cfg_attr(not(any(test, feature = "std")), no_std)]

// Shared types and utilities (internal module, public items re-exported below)
pub(crate) mod shared;

// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led, MAX_VALUE_SIZE};

// Re-export TLV constants for sync implementations (ctl, esp-idf)
#[cfg(any(feature = "ctl", feature = "esp-idf"))]
pub use shared::{HEADER_SIZE, SYNC_WORD};

// Re-export uart_config module for chip firmware
pub use shared::uart_config;

// Re-export async TLV traits and types for web-ctl
#[cfg(feature = "async-ctl")]
pub use shared::tlv::{ReadTlv, WriteTlv, Tlv, HEADER_SIZE, SYNC_WORD};
#[cfg(feature = "async-ctl")]
pub use shared::wifi::WifiSsid;

// Re-export protocol types
pub use shared::protocol::{
    ChannelId, CtlToMgmt, LoopbackMode, MgmtToCtl, MgmtToNet, MgmtToUi, NetLoopback, NetToMgmt,
    NetToUi, UiToMgmt, UiToNet,
};

// Re-export logging macros (crate-internal, no-op when defmt disabled)
// Only for firmware modules
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui"))]
pub(crate) use shared::info;

// Chip-specific modules (feature-gated)
#[cfg(feature = "mgmt")]
pub mod mgmt;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "ui")]
pub mod ui;

// Host-side control module (requires std)
#[cfg(feature = "ctl")]
pub mod ctl;

// Integration tests - tests that involve two or more chips.
// Single-chip tests should go in their respective modules.
#[cfg(test)]
mod integration_tests;
