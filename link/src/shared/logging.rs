//! Conditional logging macros.
//!
//! Uses defmt when the `defmt` feature is enabled, otherwise no-op.

#[cfg(feature = "defmt")]
macro_rules! info {
    ($($arg:tt)*) => { defmt::info!($($arg)*) };
}

#[cfg(not(feature = "defmt"))]
macro_rules! info {
    ($($arg:tt)*) => {};
}

pub(crate) use info;
