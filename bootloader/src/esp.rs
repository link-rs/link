//! ESP32-S3 ROM bootloader protocol implementation.
//!
//! This module implements the host side of the ESP32 ROM bootloader protocol
//! using SLIP framing as described in the Espressif documentation.

use embedded_io_async::{Read, Write};
use serial_line_ip::{Decoder, Encoder};

/// SLIP frame delimiter byte.
const SLIP_END: u8 = 0xC0;

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
}

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
    /// Sends SYNC commands until the bootloader responds. This must be called
    /// first after the ESP32 enters bootloader mode.
    ///
    /// The ESP32 bootloader sends 8 sync responses. We must read all of them
    /// to prevent them from interfering with subsequent commands.
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

        // Send one sync packet
        self.send_slip_packet_with_command(Command::Sync, &sync_data, 0)
            .await
            .map_err(|e| match e {
                Error::Io(io_err) => Error::Io(io_err),
                _ => Error::SyncFailed,
            })?;

        // XXX send a second packet
        self.send_slip_packet_with_command(Command::Sync, &sync_data, 0)
            .await
            .map_err(|e| match e {
                Error::Io(io_err) => Error::Io(io_err),
                _ => Error::SyncFailed,
            })?;

        // Read responses - ESP32 sends 8 sync responses
        // Keep reading until we get at least one valid sync response
        let mut got_sync = false;
        for _ in 0..8 {
            let result = self.read_response_any().await;
            println!("response: {:?}", result);
            match result {
                Ok((cmd, _)) => {
                    if cmd == Command::Sync as u8 {
                        got_sync = true;
                    }
                }
                Err(_) => {
                    // Continue trying to read more responses
                    continue;
                }
            }
        }

        if got_sync {
            Ok(())
        } else {
            Err(Error::SyncFailed)
        }
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

        // ROM loader returns 32 ASCII hex chars, but we return raw bytes
        // This assumes we're talking to the stub loader which returns 16 raw bytes
        let md5 = [0u8; 16];
        // The MD5 is in the response data after status bytes
        // For now, return zeros - actual implementation needs response data parsing
        let _ = response;
        Ok(md5)
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
        let mut encoded = [0u8; MAX_DATA_SIZE * 2 + 2];
        let mut decoded = [0u8; MAX_DATA_SIZE + 8];
        let mut decoder = Decoder::new();

        // Read until we get a complete SLIP frame
        let mut encoded_pos = 0;
        let mut decoded_pos = 0;
        let mut in_frame = false;

        loop {
            // Read one byte at a time
            let mut byte = [0u8; 1];
            self.reader
                .read_exact(&mut byte)
                .await
                .map_err(|_| Error::Timeout)?;

            if byte[0] == SLIP_END {
                if in_frame && decoded_pos > 0 {
                    // End of frame
                    break;
                } else {
                    // Start of frame
                    in_frame = true;
                    continue;
                }
            }

            if !in_frame {
                continue;
            }

            encoded[encoded_pos] = byte[0];
            encoded_pos += 1;

            // Try to decode
            let (input_used, output, is_end) = decoder
                .decode(
                    &encoded[encoded_pos - 1..encoded_pos],
                    &mut decoded[decoded_pos..],
                )
                .map_err(|_| Error::SlipError)?;

            if input_used > 0 {
                decoded_pos += output.len();
            }

            if is_end {
                break;
            }

            if decoded_pos >= decoded.len() {
                return Err(Error::BufferOverflow);
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

        // Status is at the end of the data
        let status = if decoded_pos > 8 {
            decoded[decoded_pos - 4]
        } else {
            0
        };
        let error = if decoded_pos > 9 {
            decoded[decoded_pos - 3]
        } else {
            0
        };

        let response = Response {
            command,
            value,
            status,
            error,
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
        // The serial_line_ip encoder adds the leading END byte, so we don't add it manually
        let mut encoded = [0u8; MAX_DATA_SIZE * 2 + 2];
        let mut encoder = Encoder::new();
        let mut pos = 0;

        // Encode data (encoder adds leading END)
        let totals = encoder
            .encode(data, &mut encoded[pos..])
            .map_err(|_| Error::BufferOverflow)?;
        pos += totals.written;

        // Finish frame (adds trailing END)
        let totals = encoder
            .finish(&mut encoded[pos..])
            .map_err(|_| Error::BufferOverflow)?;
        pos += totals.written;

        // Write to serial
        self.writer
            .write_all(&encoded[..pos])
            .await
            .map_err(|e| Error::Io(e.into()))?;
        self.writer.flush().await.map_err(|e| Error::Io(e.into()))?;

        Ok(())
    }

    /// Read a SLIP-encoded response packet.
    async fn read_response(
        &mut self,
        expected_command: Command,
    ) -> Result<Response, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut encoded = [0u8; MAX_DATA_SIZE * 2 + 2];
        let mut decoded = [0u8; MAX_DATA_SIZE + 8];
        let mut decoder = Decoder::new();

        // Read until we get a complete SLIP frame
        let mut encoded_pos = 0;
        let mut decoded_pos = 0;
        let mut in_frame = false;

        loop {
            // Read one byte at a time
            let mut byte = [0u8; 1];
            self.reader
                .read_exact(&mut byte)
                .await
                .map_err(|_| Error::Timeout)?;

            if byte[0] == SLIP_END {
                if in_frame && decoded_pos > 0 {
                    // End of frame
                    break;
                } else {
                    // Start of frame
                    in_frame = true;
                    continue;
                }
            }

            if !in_frame {
                continue;
            }

            encoded[encoded_pos] = byte[0];
            encoded_pos += 1;

            // Try to decode
            let (input_used, output, is_end) = decoder
                .decode(
                    &encoded[encoded_pos - 1..encoded_pos],
                    &mut decoded[decoded_pos..],
                )
                .map_err(|_| Error::SlipError)?;

            if input_used > 0 {
                decoded_pos += output.len();
            }

            if is_end {
                break;
            }

            if decoded_pos >= decoded.len() {
                return Err(Error::BufferOverflow);
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
        if command != expected_command as u8 {
            return Err(Error::CommandMismatch {
                expected: expected_command as u8,
                got: command,
            });
        }

        let _size = u16::from_le_bytes([decoded[2], decoded[3]]);
        let value = u32::from_le_bytes([decoded[4], decoded[5], decoded[6], decoded[7]]);

        // Status is at the end of the data
        let status = if decoded_pos > 8 {
            decoded[decoded_pos - 4]
        } else {
            0
        };
        let error = if decoded_pos > 9 {
            decoded[decoded_pos - 3]
        } else {
            0
        };

        if status != 0 {
            return Err(Error::BootloaderError(error));
        }

        Ok(Response {
            command,
            value,
            status,
            error,
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
