//! Link - Multi-chip communication framework.
//!
//! This crate provides the core application logic for the Link multi-chip system.
//! The code is `no_std` compatible for embedded use. The `std` feature enables
//! the `ctl` module for host-side communication.

#![cfg_attr(not(any(test, feature = "std")), no_std)]

// CLAUDE the `shared` module should be pub(crate), and we should deliberately export what is
// needed outside of this crate here.

// Shared types and utilities
pub mod shared;

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

// CLAUDE The mocks module should move to the `shared`, since it is used by the individual chip
// modules.

// CLAUDE The `testing` module should be `integration_tests`.  It should only cover systemic
// behavior that involve two or more chips.  Tests that only involve one chip should go in the
// individual chip modules (plus `ctl`).

// Test utilities
#[cfg(test)]
pub(crate) mod mocks;
#[cfg(test)]
mod testing;

// CLAUDE These re-exports should go near the `mod shared` declaration
// Re-export commonly used types at the crate root for convenience
pub use shared::{Color, InvertedPin, Led};

// CLAUDE These logging macros should go under `shared` and get re-exported as above.
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
