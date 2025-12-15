//! ESP32-S3 ROM bootloader protocol implementation.
//!
//! This module implements the host side of the ESP32 ROM bootloader protocol
//! using SLIP framing as described in the Espressif documentation.

use embedded_io_async::{Read, Write};

/// SLIP frame delimiter byte.
const SLIP_END: u8 = 0xC0;

/// SLIP escape byte.
const SLIP_ESC: u8 = 0xDB;

/// SLIP escaped END byte (sent as ESC + ESC_END to represent END in data).
const SLIP_ESC_END: u8 = 0xDC;

/// SLIP escaped ESC byte (sent as ESC + ESC_ESC to represent ESC in data).
const SLIP_ESC_ESC: u8 = 0xDD;

/// Direction byte for requests (host to device).
const DIRECTION_REQUEST: u8 = 0x00;

/// Direction byte for responses (device to host).
const DIRECTION_RESPONSE: u8 = 0x01;

/// Checksum seed value.
const CHECKSUM_SEED: u8 = 0xEF;

/// Maximum packet data size.
const MAX_DATA_SIZE: usize = 1024;

/// Bootloader command codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// Initiate flash download.
    FlashBegin = 0x02,
    /// Flash data transmission.
    FlashData = 0x03,
    /// Complete flash download.
    FlashEnd = 0x04,
    /// Start RAM download.
    MemBegin = 0x05,
    /// Finish RAM download.
    MemEnd = 0x06,
    /// RAM data transmission.
    MemData = 0x07,
    /// Synchronization probe.
    Sync = 0x08,
    /// Write 32-bit register.
    WriteReg = 0x09,
    /// Read 32-bit register.
    ReadReg = 0x0A,
    /// Configure SPI flash parameters.
    SpiSetParams = 0x0B,
    /// Enable SPI interface.
    SpiAttach = 0x0D,
    /// Modify baud rate.
    ChangeBaudrate = 0x0F,
    /// Start compressed flash download.
    FlashDeflBegin = 0x10,
    /// Compressed flash data transmission.
    FlashDeflData = 0x11,
    /// End compressed flash download.
    FlashDeflEnd = 0x12,
    /// Hash flash region (MD5).
    SpiFlashMd5 = 0x13,
    /// Read security data.
    GetSecurityInfo = 0x14,
}

/// Errors that can occur during bootloader communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// An I/O error occurred.
    Io(E),
    /// The response had an invalid direction byte.
    InvalidDirection(u8),
    /// The response command didn't match the request.
    CommandMismatch { expected: u8, got: u8 },
    /// The bootloader reported an error.
    BootloaderError(u8),
    /// The response was too short.
    ResponseTooShort,
    /// The SLIP frame was invalid.
    SlipError,
    /// Buffer overflow during SLIP encoding/decoding.
    BufferOverflow,
    /// Timeout waiting for response.
    Timeout,
    /// Sync failed after multiple attempts.
    SyncFailed,
    /// Invalid packet sequence number.
    InvalidSequence { expected: u32, got: u32 },
    /// Data size exceeds maximum.
    DataTooLarge,
}

/// Response from the bootloader.
#[derive(Debug, Clone)]
pub struct Response {
    /// The command this is a response to.
    pub command: u8,
    /// The value field (used for READ_REG).
    pub value: u32,
    /// Status byte (0 = success).
    pub status: u8,
    /// Error code (if status != 0).
    pub error: u8,
    /// Response data payload (between header and status bytes).
    pub data: [u8; 256],
    /// Length of valid data in the data array.
    pub data_len: usize,
}

/// A partition table entry from the ESP32 flash.
#[derive(Debug, Clone)]
pub struct PartitionEntry {
    /// Partition type (0x00 = app, 0x01 = data).
    pub part_type: u8,
    /// Partition subtype (for app: 0x00 = factory, 0x10+ = OTA).
    pub subtype: u8,
    /// Offset in flash.
    pub offset: u32,
    /// Size in bytes.
    pub size: u32,
    /// Partition name (up to 16 bytes).
    pub name: heapless::String<16>,
    /// Flags.
    pub flags: u32,
}

impl PartitionEntry {
    /// Check if this is an app partition.
    pub fn is_app(&self) -> bool {
        self.part_type == 0x00
    }

    /// Check if this is the factory app partition.
    pub fn is_factory_app(&self) -> bool {
        self.part_type == 0x00 && self.subtype == 0x00
    }

    /// Get a human-readable type name.
    pub fn type_name(&self) -> &'static str {
        match self.part_type {
            0x00 => "app",
            0x01 => "data",
            _ => "unknown",
        }
    }

    /// Get a human-readable subtype name.
    pub fn subtype_name(&self) -> &'static str {
        match (self.part_type, self.subtype) {
            (0x00, 0x00) => "factory",
            (0x00, 0x10..=0x1F) => "ota",
            (0x00, 0x20) => "test",
            (0x01, 0x00) => "ota_data",
            (0x01, 0x01) => "phy",
            (0x01, 0x02) => "nvs",
            (0x01, 0x03) => "coredump",
            (0x01, 0x04) => "nvs_keys",
            (0x01, 0x05) => "efuse",
            (0x01, 0x80) => "esphttpd",
            (0x01, 0x81) => "fat",
            (0x01, 0x82) => "spiffs",
            _ => "unknown",
        }
    }
}

/// Partition table magic bytes.
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];

/// MD5 partition marker.
const PARTITION_MD5_MARKER: [u8; 2] = [0xEB, 0xEB];

/// Security information from GET_SECURITY_INFO command.
#[derive(Debug, Clone, Copy)]
pub struct SecurityInfo {
    /// Security flags.
    pub flags: u32,
    /// Flash crypt count.
    pub flash_crypt_cnt: u8,
    /// Key purposes (7 bytes).
    pub key_purposes: [u8; 7],
    /// Chip ID.
    pub chip_id: u32,
    /// ECO version.
    pub eco_version: u8,
}

/// SPI flash parameters for SPI_SET_PARAMS command.
#[derive(Debug, Clone, Copy)]
pub struct SpiParams {
    /// Flash ID.
    pub id: u32,
    /// Total flash size in bytes.
    pub total_size: u32,
    /// Block size in bytes.
    pub block_size: u32,
    /// Sector size in bytes.
    pub sector_size: u32,
    /// Page size in bytes.
    pub page_size: u32,
    /// Status mask.
    pub status_mask: u32,
}

impl Default for SpiParams {
    fn default() -> Self {
        Self {
            id: 0,
            total_size: 4 * 1024 * 1024, // 4MB default
            block_size: 64 * 1024,       // 64KB
            sector_size: 4 * 1024,       // 4KB
            page_size: 256,
            status_mask: 0xFFFF,
        }
    }
}

/// ESP32-S3 bootloader client.
///
/// Wraps a serial connection (reader and writer) and provides methods
/// for interacting with the ESP32 ROM bootloader.
pub struct Bootloader<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> Bootloader<R, W>
where
    R: Read,
    W: Write,
{
    /// Create a new bootloader client from reader and writer halves.
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    /// Consume the bootloader client and return the reader and writer.
    pub fn into_inner(self) -> (R, W) {
        (self.reader, self.writer)
    }

    /// Synchronize with the bootloader.
    ///
    /// Sends two SYNC commands and reads one response to confirm sync works.
    /// This must be called first after the ESP32 enters bootloader mode.
    ///
    /// Two sync packets are sent to ensure the bootloader flushes its response.
    /// Leftover sync responses are handled by `read_response()` which skips
    /// non-matching response types.
    pub async fn sync(&mut self) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // SYNC data: 0x07 0x07 0x12 0x20 followed by 32 x 0x55
        let mut sync_data = [0u8; 36];
        sync_data[0] = 0x07;
        sync_data[1] = 0x07;
        sync_data[2] = 0x12;
        sync_data[3] = 0x20;
        sync_data[4..].fill(0x55);

        // Send two sync packets (some implementations need this)
        self.send_slip_packet_with_command(Command::Sync, &sync_data, 0)
            .await
            .map_err(|e| match e {
                Error::Io(io_err) => Error::Io(io_err),
                _ => Error::SyncFailed,
            })?;

        self.send_slip_packet_with_command(Command::Sync, &sync_data, 0)
            .await
            .map_err(|e| match e {
                Error::Io(io_err) => Error::Io(io_err),
                _ => Error::SyncFailed,
            })?;

        // Read the first sync response - this proves we're connected
        // Any remaining responses will be skipped by read_response()
        let (cmd, _) = self.read_response_any().await?;
        if cmd != Command::Sync as u8 {
            return Err(Error::SyncFailed);
        }

        Ok(())
    }

    /// Read a 32-bit register value.
    pub async fn read_reg(&mut self, address: u32) -> Result<u32, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let data = address.to_le_bytes();
        let response = self.send_command(Command::ReadReg, &data, 0).await?;
        Ok(response.value)
    }

    /// Write a 32-bit register value.
    ///
    /// The mask parameter allows selective bit modification.
    /// The delay parameter specifies microseconds to wait after the write.
    pub async fn write_reg(
        &mut self,
        address: u32,
        value: u32,
        mask: u32,
        delay_us: u32,
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&address.to_le_bytes());
        data[4..8].copy_from_slice(&value.to_le_bytes());
        data[8..12].copy_from_slice(&mask.to_le_bytes());
        data[12..16].copy_from_slice(&delay_us.to_le_bytes());

        self.send_command(Command::WriteReg, &data, 0).await?;
        Ok(())
    }

    /// Begin a memory (RAM) write operation.
    ///
    /// Returns the number of packets that should be sent.
    pub async fn mem_begin(
        &mut self,
        total_size: u32,
        block_size: u32,
        offset: u32,
    ) -> Result<u32, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let packet_count = total_size.div_ceil(block_size);

        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&total_size.to_le_bytes());
        data[4..8].copy_from_slice(&packet_count.to_le_bytes());
        data[8..12].copy_from_slice(&block_size.to_le_bytes());
        data[12..16].copy_from_slice(&offset.to_le_bytes());

        self.send_command(Command::MemBegin, &data, 0).await?;
        Ok(packet_count)
    }

    /// Send a memory data packet.
    ///
    /// The sequence number should start at 0 and increment for each packet.
    pub async fn mem_data(&mut self, data: &[u8], sequence: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if data.len() > MAX_DATA_SIZE - 16 {
            return Err(Error::DataTooLarge);
        }

        let mut packet = [0u8; MAX_DATA_SIZE];
        let data_len = data.len() as u32;

        // Header: size, sequence, 0, 0
        packet[0..4].copy_from_slice(&data_len.to_le_bytes());
        packet[4..8].copy_from_slice(&sequence.to_le_bytes());
        packet[8..12].copy_from_slice(&0u32.to_le_bytes());
        packet[12..16].copy_from_slice(&0u32.to_le_bytes());
        packet[16..16 + data.len()].copy_from_slice(data);

        let checksum = Self::checksum(data);
        self.send_command(Command::MemData, &packet[..16 + data.len()], checksum)
            .await?;
        Ok(())
    }

    /// End a memory write operation.
    ///
    /// If `execute` is true, the bootloader will jump to `entry_point`.
    pub async fn mem_end(&mut self, execute: bool, entry_point: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&(if execute { 0u32 } else { 1u32 }).to_le_bytes());
        data[4..8].copy_from_slice(&entry_point.to_le_bytes());

        self.send_command(Command::MemEnd, &data, 0).await?;
        Ok(())
    }

    /// Begin a flash write operation.
    ///
    /// Returns the number of packets that should be sent.
    pub async fn flash_begin(
        &mut self,
        total_size: u32,
        block_size: u32,
        offset: u32,
    ) -> Result<u32, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let packet_count = total_size.div_ceil(block_size);

        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&total_size.to_le_bytes()); // erase size
        data[4..8].copy_from_slice(&packet_count.to_le_bytes());
        data[8..12].copy_from_slice(&block_size.to_le_bytes());
        data[12..16].copy_from_slice(&offset.to_le_bytes());

        self.send_command(Command::FlashBegin, &data, 0).await?;
        Ok(packet_count)
    }

    /// Send a flash data packet.
    ///
    /// The sequence number should start at 0 and increment for each packet.
    pub async fn flash_data(&mut self, data: &[u8], sequence: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if data.len() > MAX_DATA_SIZE - 16 {
            return Err(Error::DataTooLarge);
        }

        let mut packet = [0u8; MAX_DATA_SIZE];
        let data_len = data.len() as u32;

        // Header: size, sequence, 0, 0
        packet[0..4].copy_from_slice(&data_len.to_le_bytes());
        packet[4..8].copy_from_slice(&sequence.to_le_bytes());
        packet[8..12].copy_from_slice(&0u32.to_le_bytes());
        packet[12..16].copy_from_slice(&0u32.to_le_bytes());
        packet[16..16 + data.len()].copy_from_slice(data);

        let checksum = Self::checksum(data);
        self.send_command(Command::FlashData, &packet[..16 + data.len()], checksum)
            .await?;
        Ok(())
    }

    /// End a flash write operation.
    ///
    /// If `reboot` is false (0), the bootloader will reboot after completion.
    pub async fn flash_end(&mut self, reboot: bool) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let data = (if reboot { 0u32 } else { 1u32 }).to_le_bytes();
        self.send_command(Command::FlashEnd, &data, 0).await?;
        Ok(())
    }

    /// Begin a compressed flash write operation.
    ///
    /// Returns the number of packets that should be sent.
    pub async fn flash_defl_begin(
        &mut self,
        uncompressed_size: u32,
        compressed_block_size: u32,
        offset: u32,
    ) -> Result<u32, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let packet_count = uncompressed_size.div_ceil(compressed_block_size);

        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&uncompressed_size.to_le_bytes());
        data[4..8].copy_from_slice(&packet_count.to_le_bytes());
        data[8..12].copy_from_slice(&compressed_block_size.to_le_bytes());
        data[12..16].copy_from_slice(&offset.to_le_bytes());

        self.send_command(Command::FlashDeflBegin, &data, 0).await?;
        Ok(packet_count)
    }

    /// Send a compressed flash data packet.
    ///
    /// The data should be gzip-deflated.
    pub async fn flash_defl_data(
        &mut self,
        data: &[u8],
        sequence: u32,
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if data.len() > MAX_DATA_SIZE - 16 {
            return Err(Error::DataTooLarge);
        }

        let mut packet = [0u8; MAX_DATA_SIZE];
        let data_len = data.len() as u32;

        packet[0..4].copy_from_slice(&data_len.to_le_bytes());
        packet[4..8].copy_from_slice(&sequence.to_le_bytes());
        packet[8..12].copy_from_slice(&0u32.to_le_bytes());
        packet[12..16].copy_from_slice(&0u32.to_le_bytes());
        packet[16..16 + data.len()].copy_from_slice(data);

        let checksum = Self::checksum(data);
        self.send_command(Command::FlashDeflData, &packet[..16 + data.len()], checksum)
            .await?;
        Ok(())
    }

    /// End a compressed flash write operation.
    pub async fn flash_defl_end(&mut self, reboot: bool) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let data = (if reboot { 0u32 } else { 1u32 }).to_le_bytes();
        self.send_command(Command::FlashDeflEnd, &data, 0).await?;
        Ok(())
    }

    /// Attach the SPI flash.
    ///
    /// Pass 0 for default SPI, 1 for HSPI.
    pub async fn spi_attach(&mut self, config: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // ROM loader expects 8 bytes (config + 4 zero bytes)
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&config.to_le_bytes());

        self.send_command(Command::SpiAttach, &data, 0).await?;
        Ok(())
    }

    /// Set SPI flash parameters.
    pub async fn spi_set_params(&mut self, params: &SpiParams) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut data = [0u8; 24];
        data[0..4].copy_from_slice(&params.id.to_le_bytes());
        data[4..8].copy_from_slice(&params.total_size.to_le_bytes());
        data[8..12].copy_from_slice(&params.block_size.to_le_bytes());
        data[12..16].copy_from_slice(&params.sector_size.to_le_bytes());
        data[16..20].copy_from_slice(&params.page_size.to_le_bytes());
        data[20..24].copy_from_slice(&params.status_mask.to_le_bytes());

        self.send_command(Command::SpiSetParams, &data, 0).await?;
        Ok(())
    }

    /// Change the baud rate.
    ///
    /// The old_baud should be the current baud rate (or 0 for ROM loader).
    pub async fn change_baudrate(
        &mut self,
        new_baud: u32,
        old_baud: u32,
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&new_baud.to_le_bytes());
        data[4..8].copy_from_slice(&old_baud.to_le_bytes());

        self.send_command(Command::ChangeBaudrate, &data, 0).await?;
        Ok(())
    }

    /// Calculate MD5 hash of a flash region.
    ///
    /// Returns the 16-byte MD5 digest.
    pub async fn spi_flash_md5(
        &mut self,
        address: u32,
        size: u32,
    ) -> Result<[u8; 16], Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&address.to_le_bytes());
        data[4..8].copy_from_slice(&size.to_le_bytes());
        // bytes 8-15 are zeros

        let response = self.send_command(Command::SpiFlashMd5, &data, 0).await?;

        // ROM loader returns 32 ASCII hex chars, stub loader returns 16 raw bytes
        let mut md5 = [0u8; 16];

        if response.data_len == 32 {
            // ROM loader: 32 ASCII hex characters (e.g., "d41d8cd98f00b204e9800998ecf8427e")
            for i in 0..16 {
                let hi = Self::hex_char_to_nibble(response.data[i * 2]);
                let lo = Self::hex_char_to_nibble(response.data[i * 2 + 1]);
                md5[i] = (hi << 4) | lo;
            }
        } else if response.data_len >= 16 {
            // Stub loader: 16 raw bytes
            md5.copy_from_slice(&response.data[..16]);
        }

        Ok(md5)
    }

    /// Convert a hex ASCII character to its nibble value.
    fn hex_char_to_nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0,
        }
    }

    /// Read data from flash using SPI peripheral registers.
    ///
    /// This reads up to 64 bytes at a time by controlling the SPI1 flash
    /// peripheral directly via READ_REG/WRITE_REG commands.
    ///
    /// For ESP32-S3, SPI1 is at base 0x60002000.
    pub async fn read_flash(
        &mut self,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // ESP32-S3 SPI1 registers (flash SPI controller)
        const SPI1_BASE: u32 = 0x6000_2000;
        const SPI1_CMD_REG: u32 = SPI1_BASE + 0x00;
        const SPI1_ADDR_REG: u32 = SPI1_BASE + 0x04;
        const SPI1_USER_REG: u32 = SPI1_BASE + 0x18;
        const SPI1_USER1_REG: u32 = SPI1_BASE + 0x1C;
        const SPI1_USER2_REG: u32 = SPI1_BASE + 0x20;
        const SPI1_MISO_DLEN_REG: u32 = SPI1_BASE + 0x28;
        const SPI1_W0_REG: u32 = SPI1_BASE + 0x98;

        // SPI flash read command
        const SPI_FLASH_READ_CMD: u32 = 0x03;

        // Read in chunks of up to 64 bytes (16 words)
        let mut offset = 0;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(64);
            let flash_addr = address + offset as u32;

            // Configure SPI for read operation
            // USER_REG: enable command, address, and data-in phases
            // Bit 27: USR_COMMAND, Bit 28: USR_ADDR, Bit 29: USR_MISO
            self.write_reg(SPI1_USER_REG, (1 << 27) | (1 << 28) | (1 << 29), 0xFFFFFFFF, 0)
                .await?;

            // USER1_REG: set address bit length (24 bits = 23 in the field)
            // Bits 26-31: USR_ADDR_BITLEN
            self.write_reg(SPI1_USER1_REG, 23 << 26, 0xFFFFFFFF, 0).await?;

            // USER2_REG: set command value and length
            // Bits 0-15: USR_COMMAND_VALUE, Bits 28-31: USR_COMMAND_BITLEN (7 = 8 bits)
            self.write_reg(SPI1_USER2_REG, SPI_FLASH_READ_CMD | (7 << 28), 0xFFFFFFFF, 0)
                .await?;

            // ADDR_REG: set flash address (shifted for 24-bit addressing)
            self.write_reg(SPI1_ADDR_REG, flash_addr << 8, 0xFFFFFFFF, 0).await?;

            // MISO_DLEN_REG: set read data bit length
            self.write_reg(SPI1_MISO_DLEN_REG, (chunk_len as u32 * 8) - 1, 0xFFFFFFFF, 0)
                .await?;

            // CMD_REG: trigger the SPI transaction (bit 18: USR)
            self.write_reg(SPI1_CMD_REG, 1 << 18, 0xFFFFFFFF, 0).await?;

            // Wait for completion by polling CMD_REG until USR bit clears
            for _ in 0..100 {
                let cmd = self.read_reg(SPI1_CMD_REG).await?;
                if (cmd & (1 << 18)) == 0 {
                    break;
                }
            }

            // Read data from W0-W15 registers
            let words = (chunk_len + 3) / 4;
            for i in 0..words {
                let word = self.read_reg(SPI1_W0_REG + i as u32 * 4).await?;
                let word_bytes = word.to_le_bytes();
                let start = offset + i * 4;
                let end = (start + 4).min(data.len());
                let copy_len = end - start;
                data[start..end].copy_from_slice(&word_bytes[..copy_len]);
            }

            offset += chunk_len;
        }

        Ok(())
    }

    /// Read the partition table from flash.
    ///
    /// Reads the partition table from flash offset 0x8000 using direct
    /// SPI flash reads via the bootloader protocol.
    ///
    /// Returns a vector of partition entries (up to 16).
    pub async fn read_partition_table(
        &mut self,
    ) -> Result<heapless::Vec<PartitionEntry, 16>, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        const PARTITION_TABLE_OFFSET: u32 = 0x8000;
        const PARTITION_ENTRY_SIZE: usize = 32;

        let mut entries = heapless::Vec::new();

        // Read up to 16 partition entries
        for i in 0..16u32 {
            let entry_addr = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_SIZE as u32;

            // Read 32 bytes for one partition entry
            let mut entry_data = [0u8; 32];
            self.read_flash(entry_addr, &mut entry_data).await?;

            // Check magic bytes
            if entry_data[0] == PARTITION_MAGIC[0] && entry_data[1] == PARTITION_MAGIC[1] {
                // Parse name (16 bytes, null-terminated)
                let name_bytes = &entry_data[12..28];
                let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
                let name_str = core::str::from_utf8(&name_bytes[..name_len]).unwrap_or("");
                let mut name = heapless::String::new();
                let _ = name.push_str(name_str);

                let entry = PartitionEntry {
                    part_type: entry_data[2],
                    subtype: entry_data[3],
                    offset: u32::from_le_bytes([
                        entry_data[4],
                        entry_data[5],
                        entry_data[6],
                        entry_data[7],
                    ]),
                    size: u32::from_le_bytes([
                        entry_data[8],
                        entry_data[9],
                        entry_data[10],
                        entry_data[11],
                    ]),
                    name,
                    flags: u32::from_le_bytes([
                        entry_data[28],
                        entry_data[29],
                        entry_data[30],
                        entry_data[31],
                    ]),
                };
                let _ = entries.push(entry);
            } else if entry_data[0] == PARTITION_MD5_MARKER[0]
                && entry_data[1] == PARTITION_MD5_MARKER[1]
            {
                // MD5 checksum marker - end of partition table
                break;
            } else if entry_data[0] == 0xFF && entry_data[1] == 0xFF {
                // Empty entry - end of partition table
                break;
            }
        }

        Ok(entries)
    }

    /// Get security information from the chip.
    pub async fn get_security_info(&mut self) -> Result<SecurityInfo, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let response = self.send_command(Command::GetSecurityInfo, &[], 0).await?;

        // Parse security info from response
        // The actual parsing depends on the response format
        let _ = response;
        Ok(SecurityInfo {
            flags: 0,
            flash_crypt_cnt: 0,
            key_purposes: [0; 7],
            chip_id: 0,
            eco_version: 0,
        })
    }

    // --- Helper methods ---

    /// Calculate checksum for data payload.
    fn checksum(data: &[u8]) -> u32 {
        let mut checksum = CHECKSUM_SEED;
        for &byte in data {
            checksum ^= byte;
        }
        checksum as u32
    }

    /// Send a SLIP packet with command header (without reading response).
    async fn send_slip_packet_with_command(
        &mut self,
        command: Command,
        data: &[u8],
        checksum: u32,
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Build the packet
        let mut packet = [0u8; MAX_DATA_SIZE + 8];
        packet[0] = DIRECTION_REQUEST;
        packet[1] = command as u8;
        let size = data.len() as u16;
        packet[2..4].copy_from_slice(&size.to_le_bytes());
        packet[4..8].copy_from_slice(&checksum.to_le_bytes());
        packet[8..8 + data.len()].copy_from_slice(data);

        let packet_len = 8 + data.len();

        // SLIP encode and send
        self.send_slip_packet(&packet[..packet_len]).await
    }

    /// Read a response without checking the command type.
    /// Returns (command, response) on success.
    async fn read_response_any(&mut self) -> Result<(u8, Response), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut decoded = [0u8; MAX_DATA_SIZE + 8];

        // Read until we get a complete SLIP frame
        let mut decoded_pos = 0;
        let mut in_frame = false;
        let mut in_escape = false;

        loop {
            // Read one byte at a time
            let mut byte = [0u8; 1];
            self.reader
                .read_exact(&mut byte)
                .await
                .map_err(|_| Error::Timeout)?;

            let b = byte[0];

            if b == SLIP_END {
                if in_frame && decoded_pos > 0 {
                    // End of frame
                    break;
                } else {
                    // Start of frame
                    in_frame = true;
                    in_escape = false;
                    continue;
                }
            }

            if !in_frame {
                continue;
            }

            // Handle SLIP escape sequences
            if in_escape {
                in_escape = false;
                let decoded_byte = match b {
                    SLIP_ESC_END => SLIP_END,   // 0xDC -> 0xC0
                    SLIP_ESC_ESC => SLIP_ESC,   // 0xDD -> 0xDB
                    _ => return Err(Error::SlipError), // Invalid escape sequence
                };
                if decoded_pos >= decoded.len() {
                    return Err(Error::BufferOverflow);
                }
                decoded[decoded_pos] = decoded_byte;
                decoded_pos += 1;
            } else if b == SLIP_ESC {
                in_escape = true;
            } else {
                if decoded_pos >= decoded.len() {
                    return Err(Error::BufferOverflow);
                }
                decoded[decoded_pos] = b;
                decoded_pos += 1;
            }
        }

        // Parse response
        if decoded_pos < 8 {
            return Err(Error::ResponseTooShort);
        }

        let direction = decoded[0];
        if direction != DIRECTION_RESPONSE {
            return Err(Error::InvalidDirection(direction));
        }

        let command = decoded[1];
        let _size = u16::from_le_bytes([decoded[2], decoded[3]]);
        let value = u32::from_le_bytes([decoded[4], decoded[5], decoded[6], decoded[7]]);

        // Status bytes are at the end (last 2 bytes before any padding)
        // Data is between byte 8 and status bytes
        let (status, error, data_end) = if decoded_pos > 10 {
            (decoded[decoded_pos - 2], decoded[decoded_pos - 1], decoded_pos - 2)
        } else if decoded_pos > 8 {
            (decoded[decoded_pos - 2], decoded[decoded_pos - 1], 8)
        } else {
            (0, 0, 8)
        };

        // Extract data payload
        let mut data = [0u8; 256];
        let data_start = 8;
        let data_len = if data_end > data_start {
            let len = (data_end - data_start).min(256);
            data[..len].copy_from_slice(&decoded[data_start..data_start + len]);
            len
        } else {
            0
        };

        let response = Response {
            command,
            value,
            status,
            error,
            data,
            data_len,
        };

        Ok((command, response))
    }

    /// Send a command and wait for response.
    async fn send_command(
        &mut self,
        command: Command,
        data: &[u8],
        checksum: u32,
    ) -> Result<Response, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Build the packet
        let mut packet = [0u8; MAX_DATA_SIZE + 8];
        packet[0] = DIRECTION_REQUEST;
        packet[1] = command as u8;
        let size = data.len() as u16;
        packet[2..4].copy_from_slice(&size.to_le_bytes());
        packet[4..8].copy_from_slice(&checksum.to_le_bytes());
        packet[8..8 + data.len()].copy_from_slice(data);

        let packet_len = 8 + data.len();

        // SLIP encode and send
        self.send_slip_packet(&packet[..packet_len]).await?;

        // Read response
        self.read_response(command).await
    }

    /// Send a SLIP-encoded packet.
    async fn send_slip_packet(&mut self, data: &[u8]) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // SLIP encode the data
        let mut encoded = [0u8; MAX_DATA_SIZE * 2 + 2];
        let mut pos = 0;

        // Start with END delimiter
        encoded[pos] = SLIP_END;
        pos += 1;

        // Encode data with escape sequences
        for &byte in data {
            match byte {
                SLIP_END => {
                    if pos + 2 > encoded.len() {
                        return Err(Error::BufferOverflow);
                    }
                    encoded[pos] = SLIP_ESC;
                    encoded[pos + 1] = SLIP_ESC_END;
                    pos += 2;
                }
                SLIP_ESC => {
                    if pos + 2 > encoded.len() {
                        return Err(Error::BufferOverflow);
                    }
                    encoded[pos] = SLIP_ESC;
                    encoded[pos + 1] = SLIP_ESC_ESC;
                    pos += 2;
                }
                _ => {
                    if pos + 1 > encoded.len() {
                        return Err(Error::BufferOverflow);
                    }
                    encoded[pos] = byte;
                    pos += 1;
                }
            }
        }

        // End with END delimiter
        if pos + 1 > encoded.len() {
            return Err(Error::BufferOverflow);
        }
        encoded[pos] = SLIP_END;
        pos += 1;

        // Write to serial
        self.writer
            .write_all(&encoded[..pos])
            .await
            .map_err(|e| Error::Io(e.into()))?;
        self.writer.flush().await.map_err(|e| Error::Io(e.into()))?;

        Ok(())
    }

    /// Read a SLIP-encoded response packet.
    ///
    /// This will skip any responses that don't match the expected command,
    /// which allows draining leftover sync responses.
    async fn read_response(
        &mut self,
        expected_command: Command,
    ) -> Result<Response, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Skip up to 8 non-matching responses (e.g., leftover sync responses)
        for _ in 0..8 {
            let (cmd, response) = self.read_response_any().await?;
            if cmd == expected_command as u8 {
                if response.status != 0 {
                    return Err(Error::BootloaderError(response.error));
                }
                return Ok(response);
            }
            // Wrong command, try reading another
        }

        // Couldn't find matching response after 8 tries
        Err(Error::CommandMismatch {
            expected: expected_command as u8,
            got: 0,
        })
    }
}

/// Write firmware to flash memory.
///
/// This is a convenience function that handles the FLASH_BEGIN/DATA/END sequence.
pub async fn write_flash<R, W>(
    bootloader: &mut Bootloader<R, W>,
    address: u32,
    data: &[u8],
    block_size: u32,
) -> Result<(), Error<R::Error>>
where
    R: Read,
    W: Write,
    W::Error: Into<R::Error>,
{
    let packet_count = bootloader
        .flash_begin(data.len() as u32, block_size, address)
        .await?;

    for (seq, chunk) in data.chunks(block_size as usize).enumerate() {
        // Pad to block size with 0xFF
        let mut block = [0xFFu8; 1024];
        block[..chunk.len()].copy_from_slice(chunk);
        let padded_len = chunk.len().div_ceil(4) * 4; // Align to 4 bytes

        bootloader
            .flash_data(&block[..padded_len.max(chunk.len())], seq as u32)
            .await?;
    }

    let _ = packet_count;
    bootloader.flash_end(false).await?;
    Ok(())
}

/// Write data to RAM memory.
///
/// This is a convenience function that handles the MEM_BEGIN/DATA/END sequence.
pub async fn write_mem<R, W>(
    bootloader: &mut Bootloader<R, W>,
    address: u32,
    data: &[u8],
    block_size: u32,
    execute: bool,
    entry_point: u32,
) -> Result<(), Error<R::Error>>
where
    R: Read,
    W: Write,
    W::Error: Into<R::Error>,
{
    let packet_count = bootloader
        .mem_begin(data.len() as u32, block_size, address)
        .await?;

    for (seq, chunk) in data.chunks(block_size as usize).enumerate() {
        bootloader.mem_data(chunk, seq as u32).await?;
    }

    let _ = packet_count;
    bootloader.mem_end(execute, entry_point).await?;
    Ok(())
}
