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
/// Used for CTL-MGMT and MGMT-UI links (both endpoints are STM32).
pub const STM32_BOOTLOADER: Config = Config {
    baudrate: 115200,
    parity: Parity::Even,
    stop_bits: StopBits::One,
};

/// MGMT-NET link: 115200 baud, no parity, 1 stop bit.
pub const MGMT_NET: Config = Config {
    baudrate: 115200,
    parity: Parity::None,
    stop_bits: StopBits::One,
};

/// UI-NET link: 460800 baud, no parity, 2 stop bits.
pub const UI_NET: Config = Config {
    baudrate: 460800,
    parity: Parity::None,
    stop_bits: StopBits::Two,
};
