//! Protocol-specific constants and configuration values.

/// Retry limits for various operations
pub mod retries {
    /// Maximum hello() attempts when waiting for MGMT to boot
    pub const MGMT_BOOT_MAX_ATTEMPTS: usize = 50;

    /// Maximum TLV skip count before giving up sync
    pub const MAX_TLV_SKIP: usize = 1024;

    /// Maximum probe attempts during UI bootloader detection
    pub const UI_PROBE_MAX_ATTEMPTS: usize = 20;
}

/// Channel identifiers for chip communication
pub mod channels {
    /// PTT channel ID
    pub const PTT: u8 = 0;

    /// PTT AI channel ID
    pub const PTT_AI: u8 = 1;

    /// Chat AI channel ID
    pub const CHAT_AI: u8 = 3;
}

/// Timeout values for various operations
pub mod timeouts {
    /// Default timeout for normal TLV operations (in seconds)
    pub const NORMAL_SECS: u64 = 3;

    /// Short timeout for quick operations (in milliseconds)
    pub const SHORT_MS: u64 = 500;

    /// Monitor mode timeout (non-blocking reads, in milliseconds)
    pub const MONITOR_MS: u64 = 100;

    // Duration constants - only available with std
    #[cfg(feature = "std")]
    mod durations {
        use std::time::Duration;

        pub const NORMAL: Duration = Duration::from_secs(super::NORMAL_SECS);
        pub const SHORT: Duration = Duration::from_millis(super::SHORT_MS);
        pub const MONITOR: Duration = Duration::from_millis(super::MONITOR_MS);
    }

    #[cfg(feature = "std")]
    pub use durations::*;
}
