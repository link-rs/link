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
    use std::time::Duration;

    /// Default timeout for normal TLV operations
    pub const NORMAL_SECS: u64 = 3;
    pub const NORMAL: Duration = Duration::from_secs(NORMAL_SECS);

    /// Short timeout for quick operations
    pub const SHORT_MS: u64 = 500;
    pub const SHORT: Duration = Duration::from_millis(SHORT_MS);

    /// Monitor mode timeout (non-blocking reads)
    pub const MONITOR_MS: u64 = 100;
    pub const MONITOR: Duration = Duration::from_millis(MONITOR_MS);
}
