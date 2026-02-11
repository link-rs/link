//! Persistent storage trait for MGMT configuration.

/// Persistent storage for MGMT configuration.
pub trait BaudRateStorage {
    /// Read the stored CTL baud rate.
    /// Returns None if no valid stored value exists.
    fn get(&mut self) -> Option<u32>;

    /// Store a new CTL baud rate value.
    /// Returns true if storage succeeded.
    fn set(&mut self, baud_rate: u32) -> bool;
}
