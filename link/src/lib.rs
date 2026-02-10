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

// alloc is needed for std (which implies alloc) and ctl-core (which uses alloc feature)
#[cfg(any(feature = "std", feature = "alloc"))]
extern crate alloc;

// Shared types and utilities (internal module, public items re-exported below)
pub(crate) mod shared;

// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led, MAX_VALUE_SIZE};

// Re-export TLV constants for sync implementations (ctl, esp-idf)
#[cfg(any(feature = "ctl", feature = "esp-idf"))]
pub use shared::{HEADER_SIZE, SYNC_WORD};

// Re-export uart_config module for chip firmware
pub use shared::uart_config;

// Re-export async TLV traits and types for firmware modules and async-ctl
#[cfg(feature = "async-ctl")]
pub use shared::tlv::{ReadTlv, WriteTlv, Tlv};

// Re-export Tlv for ctl (not async traits)
#[cfg(all(feature = "ctl", not(feature = "async-ctl")))]
pub use shared::tlv::Tlv;

// Re-export TLV types and constants for ctl-core (used by CtlCore internally)
#[cfg(all(feature = "ctl-core", not(any(feature = "ctl", feature = "esp-idf", feature = "async-ctl"))))]
pub use shared::tlv::{HEADER_SIZE, SYNC_WORD};

// Re-export WifiSsid for async-ctl and ctl
#[cfg(any(feature = "async-ctl", feature = "ctl"))]
pub use shared::wifi::WifiSsid;

// Re-export protocol types
pub use shared::protocol::{
    ChannelId, CtlToMgmt, CtlToNet, CtlToUi, JitterStatsInfo, MgmtToCtl, NetLoopbackMode,
    NetToCtl, NetToUi, Pin, PinValue, StackInfo, UiLoopbackMode, UiToCtl, UiToNet,
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

// Host-side control module
// - ctl-core: Async-first core implementation (no_std compatible with alloc)
// - ctl: Full CLI support with sync I/O and flashing (requires std)
#[cfg(any(feature = "ctl-core", feature = "ctl"))]
pub mod ctl;

// Integration tests - tests that involve two or more chips.
// Single-chip tests should go in their respective modules.
#[cfg(test)]
mod integration_tests;
