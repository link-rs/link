//! Chip-specific hardware configuration constants.

/// Kilobyte constant for readability
pub const KB: usize = 1024;

/// STM32 chip constants
pub mod stm32 {
    use super::KB;

    /// STM32F072 constants (UI chip)
    pub mod f072 {
        use super::KB;

        /// Flash page size
        pub const PAGE_SIZE: usize = 2 * KB;

        /// Write chunk size for bootloader protocol
        pub const WRITE_CHUNK_SIZE: usize = 256;

        /// Base address of flash memory
        pub const FLASH_BASE: u32 = 0x0800_0000;
    }

    /// STM32F405 constants (MGMT chip)
    pub mod f405 {
        use super::KB;

        /// Write chunk size for bootloader protocol
        pub const WRITE_CHUNK_SIZE: usize = 256;

        /// Verify chunk size
        pub const VERIFY_CHUNK_SIZE: usize = 256;

        /// Base address of flash memory
        pub const FLASH_BASE: u32 = 0x0800_0000;

        /// Flash sector sizes (sectors 0-11)
        pub const SECTOR_SIZES: [usize; 12] = [
            16 * KB,  // Sector 0
            16 * KB,  // Sector 1
            16 * KB,  // Sector 2
            16 * KB,  // Sector 3
            64 * KB,  // Sector 4
            128 * KB, // Sector 5
            128 * KB, // Sector 6
            128 * KB, // Sector 7
            128 * KB, // Sector 8
            128 * KB, // Sector 9
            128 * KB, // Sector 10
            128 * KB, // Sector 11
        ];
    }
}

/// TLV protocol buffer constants
pub mod tlv {
    /// Extra padding added to TLV buffers for alignment/overhead
    /// Used in buffer size calculations like: data_size + PADDING_BYTES
    pub const PADDING_BYTES: usize = 256;
}
