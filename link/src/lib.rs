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

#![cfg_attr(not(any(test, feature = "std")), no_std)]

// alloc is needed for std (which implies alloc) and features that use alloc
#[cfg(any(feature = "std", feature = "alloc"))]
extern crate alloc;

// Shared types and utilities (internal module, public items re-exported below)
pub(crate) mod shared;

// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led, MAX_VALUE_SIZE};

// Re-export TLV constants for sync implementations (ctl, net)
#[cfg(any(feature = "ctl", feature = "net"))]
pub use shared::{HEADER_SIZE, SYNC_WORD};

// Re-export uart_config module for chip firmware
pub use shared::uart_config;

// Re-export StackMonitor trait for chip-specific Board traits
#[cfg(any(feature = "mgmt", feature = "ui"))]
pub use shared::stack_monitor::StackMonitor;

// Re-export protocol_config module for ctl
#[cfg(feature = "ctl")]
pub use shared::protocol_config;

// Re-export timing module for ctl
#[cfg(feature = "ctl")]
pub use shared::timing;

// Re-export Tlv for ctl
#[cfg(feature = "ctl")]
pub use shared::tlv::Tlv;

// Re-export WifiSsid for ctl
#[cfg(feature = "ctl")]
pub use shared::wifi::WifiSsid;

// Re-export protocol types
pub use shared::protocol::{
    AdjDirection, AudioMode, ChannelId, CtlToMgmt, CtlToNet, CtlToUi, MgmtToCtl, NetLoopbackMode,
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
#[cfg(feature = "ctl")]
pub mod ctl;

// Integration tests - tests that involve two or more chips.
// Single-chip tests should go in their respective modules.
#[cfg(test)]
mod integration_tests;
