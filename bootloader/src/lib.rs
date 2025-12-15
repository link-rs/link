//! Bootloader protocol implementations for embedded devices.
//!
//! This crate provides host-side implementations of bootloader protocols
//! for flashing firmware to embedded devices over serial connections.
//!
//! # Supported Protocols
//!
//! - **STM32 (AN3155)**: USART bootloader protocol for STM32 microcontrollers
//! - **ESP32**: ROM bootloader protocol for ESP32 microcontrollers (coming soon)

//#![no_std]

pub mod esp;
pub mod stm;
