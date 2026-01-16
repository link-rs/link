//! TLV-based logging for the UI chip.
//!
//! Provides a macro that formats log messages and sends them through a channel
//! to be forwarded to MGMT (and then to CTL) for display.
//!
//! # Usage
//!
//! ```ignore
//! let log_sender = log_channel.sender();
//! tlv_log!(log_sender, "loopback enabled: {}", enabled);
//! ```

use embassy_sync::channel::Sender;

/// Maximum size of a log message in bytes.
pub const MAX_LOG_SIZE: usize = 128;

/// A log message ready to be sent.
pub type LogMessage = heapless::String<MAX_LOG_SIZE>;

/// Type alias for the log sender.
pub type LogSender<'a, M, const N: usize> = Sender<'a, M, LogMessage, N>;

/// Format and send a log message through a channel.
///
/// This macro formats the message into a fixed-size buffer and attempts
/// to send it through the provided channel sender. If the channel is full,
/// the message is silently dropped (non-blocking).
///
/// # Example
///
/// ```ignore
/// tlv_log!(log_sender, "button {} pressed", button_id);
/// ```
#[macro_export]
macro_rules! tlv_log {
    ($sender:expr, $($arg:tt)*) => {{
        use core::fmt::Write;
        let mut buf = $crate::ui::LogMessage::new();
        // Ignore write errors (message truncated if too long)
        let _ = core::write!(buf, $($arg)*);
        // Non-blocking send - drops if channel full
        let _ = $sender.try_send(buf);
    }};
}
