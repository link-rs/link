//! CTL (Controller) chip - the host computer interface.
//!
//! This module provides the async-first `CtlCore<P>` that works with any `CtlPort` implementation.
//!
//! For WASM/async usage, use the `ctl-core` or `async-ctl` features.
//! For CLI usage, use the `ctl` feature (adds flashing support).

extern crate alloc;

// Core async implementation (available with ctl-core feature)
pub mod port;
pub mod core;

// Re-export core types
pub use self::core::{CtlCore, CtlError, escape_non_ascii};
pub use self::port::CtlPort;

#[cfg(feature = "std")]
pub use self::port::{SetBaudRate, SetTimeout};

// STM32 bootloader support (async, works with ctl-core)
pub mod stm;

// espflash integration (requires ctl feature with std)
#[cfg(feature = "ctl")]
pub use ::espflash;

// Flashing support (requires ctl feature)
#[cfg(feature = "ctl")]
pub mod flash;

// Re-export ChannelConfig from shared
#[cfg(feature = "ctl")]
pub use crate::shared::ChannelConfig;

// Re-export espflash types for CLI usage
#[cfg(feature = "ctl")]
pub use espflash::flasher::{DeviceInfo, FlashSize, SecurityInfo};
#[cfg(feature = "ctl")]
pub use espflash::target::{DefaultProgressCallback, ProgressCallbacks, XtalFrequency};

/// Interpret ESP32 security fuse bits.
///
/// Returns `(secure_boot, flash_encryption)`.
#[cfg(feature = "ctl")]
pub fn interpret_esp32_security(info: &SecurityInfo) -> (bool, bool) {
    let secure_boot = (info.flags & 1) != 0;
    let flash_encryption = info.flash_crypt_cnt.count_ones() % 2 != 0;
    (secure_boot, flash_encryption)
}
