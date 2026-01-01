//! Link - Multi-chip communication framework.
//!
//! This crate provides the core application logic for the Link multi-chip system.
//! The code is `no_std` compatible for embedded use. The `std` feature enables
//! the `ctl` module for host-side communication.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

// Shared types and utilities (internal module, public items re-exported below)
pub(crate) mod shared;

// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led, MAX_VALUE_SIZE};

// Re-export uart_config module for chip firmware
pub use shared::uart_config;

// Re-export protocol types
pub use shared::protocol::{
    CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, NetToUi, UiToMgmt, UiToNet,
};

// Re-export logging macros (crate-internal)
pub(crate) use shared::info;

// CLAUDE We should feature-gate each chip's module, so that there are `mgmt`, `net`, `ui`, and
// `ctl` features which enable the corresponding modules.  All of the modules should be enabled by
// default, but each instantiation crate should only enable the feature it depends on.  We can then
// remove the `std` feature at that point, and enable `std` whenever `ctl` is built.  There should
// be a `integration_test` feature that enables all of the modules, and the `integration_test`
// feature should be on by default (but disabled in the instantiations).

// Chip-specific modules
pub mod mgmt;
pub mod net;
pub mod ui;

// Host-side modules (requires std)
#[cfg(any(test, feature = "std"))]
pub mod ctl;

// Integration tests - tests that involve two or more chips.
// Single-chip tests should go in their respective modules.
#[cfg(test)]
mod integration_tests;
