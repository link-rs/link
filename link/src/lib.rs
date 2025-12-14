//! Link - Multi-chip communication framework.
//!
//! This crate provides the core application logic for the Link multi-chip system.
//! The code is `no_std` compatible for embedded use. The `std` feature enables
//! the `ctl` module for host-side communication.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

// Shared types and utilities
pub mod shared;

// Chip-specific modules (embedded)
pub mod mgmt;
pub mod net;
pub mod ui;

// Host-side modules (requires std)
#[cfg(any(test, feature = "std"))]
pub mod ctl;

// Test utilities
#[cfg(test)]
pub(crate) mod mocks;
#[cfg(test)]
mod testing;

// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led};

// Conditional logging macros - use defmt when feature is enabled, otherwise no-op
#[cfg(feature = "defmt")]
macro_rules! info {
    ($($arg:tt)*) => { defmt::info!($($arg)*) };
}

#[cfg(not(feature = "defmt"))]
macro_rules! info {
    ($($arg:tt)*) => {};
}

// Make info! macro available to submodules
pub(crate) use info;
