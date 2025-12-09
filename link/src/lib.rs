//! Link - Multi-chip communication framework.
//!
//! This crate provides the core application logic for the Link multi-chip system.
//! The code is `no_std` compatible for embedded use, but tests use `std` for the
//! tokio async runtime.

#![cfg_attr(not(test), no_std)]

// Shared types and utilities
pub mod shared;

// Chip-specific modules
pub mod ctl;
pub mod mgmt;
pub mod net;
pub mod ui;

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
