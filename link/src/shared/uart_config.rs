//! Centralized UART configuration constants.
//!
//! This module defines the UART parameters for all inter-chip communication.
//! Each firmware crate converts these to their HAL-specific config types.

/// Parity mode for UART.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Parity {
    None,
    Even,
}

/// Number of stop bits for UART.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StopBits {
    One,
    Two,
}

/// UART configuration parameters.
#[derive(Copy, Clone, Debug)]
pub struct Config {
    pub baudrate: u32,
    pub parity: Parity,
    pub stop_bits: StopBits,
}

/// STM32 bootloader-compatible config: 115200 baud, even parity, 1 stop bit.
/// Used only during firmware flashing for STM32 bootloader compatibility.
pub const STM32_BOOTLOADER: Config = Config {
    baudrate: 115200,
    parity: Parity::Even,
    stop_bits: StopBits::One,
};

/// High-speed config: 1000000 baud, even parity, 1 stop bit.
/// Used for all normal inter-chip communication.
pub const HIGH_SPEED: Config = Config {
    baudrate: 1000000,
    parity: Parity::Even,
    stop_bits: StopBits::One,
};

/// CTL-MGMT link: 1000000 baud, even parity, 1 stop bit.
/// For bootloader flashing, use STM32_BOOTLOADER (115200 baud, even parity).
pub const CTL_MGMT: Config = HIGH_SPEED;

/// MGMT-UI link: 1000000 baud, even parity, 1 stop bit.
/// For bootloader flashing, use STM32_BOOTLOADER (115200 baud, even parity).
pub const MGMT_UI: Config = HIGH_SPEED;

/// MGMT-NET link: 1000000 baud, no parity, 1 stop bit.
/// Uses no parity to match ESP32 bootloader and user firmware.
pub const MGMT_NET: Config = Config {
    baudrate: 1000000,
    parity: Parity::None,
    stop_bits: StopBits::One,
};

/// UI-NET link: 1000000 baud, even parity, 1 stop bit.
pub const UI_NET: Config = HIGH_SPEED;

/// Trait for types that can have their baud rate changed at runtime.
///
/// This is used by the MGMT firmware to change UART baud rates on demand.
/// Implementations should ensure any pending writes are flushed before
/// the baud rate change takes effect.
#[cfg(feature = "mgmt")]
#[allow(async_fn_in_trait)]
pub trait SetBaudRate {
    /// Set the baud rate for this UART connection.
    ///
    /// This should update both TX and RX sides of the connection.
    /// Implementations should flush any pending data before changing the rate.
    async fn set_baud_rate(&mut self, baud_rate: u32);
}
