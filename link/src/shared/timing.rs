//! Centralized timing constants for flashing operations.
//!
//! All timing values are in milliseconds unless otherwise noted.

/// Hardware reset timing
pub mod reset {
    /// Delay between reset signal changes for STM32 chips (DTR/RTS transitions)
    pub const STM32_SIGNAL_TRANSITION_MS: u64 = 50;

    /// Initial stabilization after STM32 reset signal sequence
    pub const STM32_INITIAL_STABILIZATION_MS: u64 = 100;

    /// ESP32 reset signal hold time (between RST low/high transitions)
    pub const ESP32_RESET_HOLD_MS: u64 = 10;

    /// ESP32 extended reset delay (used in some reset strategies)
    pub const ESP32_EXTENDED_RESET_MS: u64 = 100;

    /// Port initialization stabilization delay
    pub const PORT_INIT_STABILIZATION_MS: u64 = 100;
}

/// Bootloader communication timeouts
pub mod bootloader {
    /// Short timeout for bootloader probe attempts
    pub const PROBE_TIMEOUT_MS: u64 = 100;

    /// Timeout for bootloader hello handshake
    pub const HELLO_TIMEOUT_MS: u64 = 100;

    /// Interval between bootloader probe retry attempts
    pub const PROBE_RETRY_INTERVAL_MS: u64 = 50;

    /// Maximum time to wait for bootloader ready (polling timeout)
    pub const MAX_WAIT_MS: u64 = 2000;
}
