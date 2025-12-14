//! STM32 USART bootloader protocol (AN3155) implementation.
//!
//! This module implements the host side of the STM32 bootloader protocol
//! as described in ST Application Note AN3155.

use embedded_io_async::{Read, ReadExactError, Write};

/// ACK byte sent by the bootloader to acknowledge a command.
const ACK: u8 = 0x79;

/// NACK byte sent by the bootloader to reject a command.
const NACK: u8 = 0x1F;

/// Initialization byte sent to start communication and trigger auto-baud detection.
const INIT: u8 = 0x7F;

/// Bootloader command codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// Get the bootloader version and supported commands.
    Get = 0x00,
    /// Get the bootloader protocol version.
    GetVersion = 0x01,
    /// Get the chip ID.
    GetId = 0x02,
    /// Read memory starting at an address.
    ReadMemory = 0x11,
    /// Jump to an address and execute code.
    Go = 0x21,
    /// Write memory starting at an address.
    WriteMemory = 0x31,
    /// Erase flash memory pages (legacy, single-byte page numbers).
    Erase = 0x43,
    /// Erase flash memory pages (extended, two-byte page numbers).
    ExtendedErase = 0x44,
    /// Enable write protection for sectors.
    WriteProtect = 0x63,
    /// Disable write protection for all sectors.
    WriteUnprotect = 0x73,
    /// Enable read protection.
    ReadoutProtect = 0x82,
    /// Disable read protection.
    ReadoutUnprotect = 0x92,
}

/// Errors that can occur during bootloader communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The bootloader sent a NACK response.
    Nack,
    /// An unexpected response was received.
    UnexpectedResponse(u8),
    /// An I/O error occurred.
    Io(E),
    /// Unexpected end of stream while reading.
    UnexpectedEof,
    /// The provided buffer is too small.
    BufferTooSmall,
    /// The data length exceeds the maximum allowed (256 bytes).
    DataTooLarge,
    /// Invalid page count for erase operation.
    InvalidPageCount,
}

impl<E> From<ReadExactError<E>> for Error<E> {
    fn from(e: ReadExactError<E>) -> Self {
        match e {
            ReadExactError::UnexpectedEof => Error::UnexpectedEof,
            ReadExactError::Other(e) => Error::Io(e),
        }
    }
}

/// Response from the Get command containing bootloader version and supported commands.
#[derive(Debug, Clone)]
pub struct GetResponse {
    /// Bootloader protocol version (e.g., 0x31 = v3.1).
    pub version: u8,
    /// List of supported command codes.
    pub commands: [u8; 16],
    /// Number of valid commands in the `commands` array.
    pub command_count: usize,
}

/// Response from the Get Version command.
#[derive(Debug, Clone, Copy)]
pub struct VersionResponse {
    /// Bootloader protocol version.
    pub version: u8,
    /// Option byte 1 (legacy, always 0x00).
    pub option1: u8,
    /// Option byte 2 (legacy, always 0x00).
    pub option2: u8,
}

/// Special erase codes for Extended Erase command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialErase {
    /// Erase all flash memory (mass erase).
    MassErase,
    /// Erase bank 1 only.
    Bank1Erase,
    /// Erase bank 2 only.
    Bank2Erase,
}

impl SpecialErase {
    fn code(self) -> u16 {
        match self {
            SpecialErase::MassErase => 0xFFFF,
            SpecialErase::Bank1Erase => 0xFFFE,
            SpecialErase::Bank2Erase => 0xFFFD,
        }
    }
}

/// STM32 bootloader client.
///
/// Wraps a serial connection (reader and writer) and provides methods
/// for interacting with the STM32 bootloader.
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

    /// Initialize communication with the bootloader.
    ///
    /// Sends the 0x7F byte to trigger auto-baud detection and waits for ACK.
    /// This must be called first after the STM32 enters bootloader mode.
    pub async fn init(&mut self) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.write_byte(INIT).await?;
        self.wait_ack().await
    }

    /// Execute the Get command to retrieve bootloader version and supported commands.
    pub async fn get(&mut self) -> Result<GetResponse, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::Get).await?;
        self.wait_ack().await?;

        // Read number of bytes (N) - version and commands follow
        let n = self.read_byte().await? as usize;

        // Read version
        let version = self.read_byte().await?;

        // Read supported commands (N bytes remaining)
        let mut commands = [0u8; 16];
        let command_count = n.min(16);
        for slot in commands.iter_mut().take(command_count) {
            *slot = self.read_byte().await?;
        }
        // Read any remaining commands beyond array capacity
        for _ in command_count..n {
            let _ = self.read_byte().await?;
        }

        self.wait_ack().await?;

        Ok(GetResponse {
            version,
            commands,
            command_count,
        })
    }

    /// Execute the Get Version command to retrieve the protocol version.
    pub async fn get_version(&mut self) -> Result<VersionResponse, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::GetVersion).await?;
        self.wait_ack().await?;

        let version = self.read_byte().await?;
        let option1 = self.read_byte().await?;
        let option2 = self.read_byte().await?;

        self.wait_ack().await?;

        Ok(VersionResponse {
            version,
            option1,
            option2,
        })
    }

    /// Execute the Get ID command to retrieve the chip product ID.
    ///
    /// Returns the product ID as a 16-bit value (MSB first from bootloader).
    pub async fn get_id(&mut self) -> Result<u16, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::GetId).await?;
        self.wait_ack().await?;

        // N = number of bytes - 1 (always 1 for STM32, meaning 2 bytes)
        let _n = self.read_byte().await?;

        // Read PID (2 bytes, MSB first)
        let msb = self.read_byte().await?;
        let lsb = self.read_byte().await?;

        self.wait_ack().await?;

        Ok(((msb as u16) << 8) | (lsb as u16))
    }

    /// Execute the Read Memory command to read data from memory.
    ///
    /// Reads up to 256 bytes starting at the given address into the provided buffer.
    /// Returns the number of bytes read.
    pub async fn read_memory(
        &mut self,
        address: u32,
        buffer: &mut [u8],
    ) -> Result<usize, Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if buffer.is_empty() {
            return Ok(0);
        }

        let len = buffer.len().min(256);

        self.send_command(Command::ReadMemory).await?;
        self.wait_ack().await?;

        // Send address with checksum
        self.send_address(address).await?;
        self.wait_ack().await?;

        // Send number of bytes - 1 and its complement
        let n = (len - 1) as u8;
        self.write_byte(n).await?;
        self.write_byte(!n).await?;
        self.wait_ack().await?;

        // Read data
        for slot in buffer.iter_mut().take(len) {
            *slot = self.read_byte().await?;
        }

        Ok(len)
    }

    /// Execute the Go command to jump to an address and execute code.
    ///
    /// The bootloader will initialize the stack pointer from address and
    /// jump to address+4 (the reset handler).
    pub async fn go(&mut self, address: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::Go).await?;
        self.wait_ack().await?;

        self.send_address(address).await?;
        self.wait_ack().await?;

        Ok(())
    }

    /// Execute the Write Memory command to write data to memory.
    ///
    /// Writes up to 256 bytes starting at the given address.
    /// The data length must be a multiple of 4 bytes.
    pub async fn write_memory(&mut self, address: u32, data: &[u8]) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if data.is_empty() {
            return Ok(());
        }

        if data.len() > 256 {
            return Err(Error::DataTooLarge);
        }

        self.send_command(Command::WriteMemory).await?;
        self.wait_ack().await?;

        // Send address with checksum
        self.send_address(address).await?;
        self.wait_ack().await?;

        // Send N (number of bytes - 1), data, and checksum
        let n = (data.len() - 1) as u8;
        let mut checksum = n;
        self.write_byte(n).await?;

        for &byte in data {
            self.write_byte(byte).await?;
            checksum ^= byte;
        }

        self.write_byte(checksum).await?;
        self.wait_ack().await?;

        Ok(())
    }

    /// Execute the Erase command (legacy, for devices with <=256 pages).
    ///
    /// Pass `None` for global erase, or `Some(&pages)` to erase specific pages.
    /// Each page number is a single byte.
    pub async fn erase(&mut self, pages: Option<&[u8]>) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::Erase).await?;
        self.wait_ack().await?;

        match pages {
            None => {
                // Global erase: send 0xFF, 0x00
                self.write_byte(0xFF).await?;
                self.write_byte(0x00).await?;
            }
            Some(pages) => {
                if pages.is_empty() || pages.len() > 256 {
                    return Err(Error::InvalidPageCount);
                }

                // Send N (number of pages - 1)
                let n = (pages.len() - 1) as u8;
                let mut checksum = n;
                self.write_byte(n).await?;

                // Send page numbers
                for &page in pages {
                    self.write_byte(page).await?;
                    checksum ^= page;
                }

                self.write_byte(checksum).await?;
            }
        }

        self.wait_ack().await?;
        Ok(())
    }

    /// Execute the Extended Erase command (for devices with >256 pages).
    ///
    /// Pass `None` for a special erase (mass/bank), or `Some(&pages)` to erase specific pages.
    /// Each page number is a 16-bit value.
    pub async fn extended_erase(
        &mut self,
        pages: Option<&[u16]>,
        special: Option<SpecialErase>,
    ) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::ExtendedErase).await?;
        self.wait_ack().await?;

        if let Some(special) = special {
            // Special erase (mass erase, bank erase)
            let code = special.code();
            let msb = (code >> 8) as u8;
            let lsb = (code & 0xFF) as u8;

            self.write_byte(msb).await?;
            self.write_byte(lsb).await?;
            self.write_byte(msb ^ lsb).await?; // Checksum
        } else if let Some(pages) = pages {
            if pages.is_empty() {
                return Err(Error::InvalidPageCount);
            }

            // Send N (number of pages - 1) as 2 bytes
            let n = (pages.len() - 1) as u16;
            let n_msb = (n >> 8) as u8;
            let n_lsb = (n & 0xFF) as u8;

            let mut checksum = n_msb ^ n_lsb;
            self.write_byte(n_msb).await?;
            self.write_byte(n_lsb).await?;

            // Send page numbers (2 bytes each, MSB first)
            for &page in pages {
                let msb = (page >> 8) as u8;
                let lsb = (page & 0xFF) as u8;
                self.write_byte(msb).await?;
                self.write_byte(lsb).await?;
                checksum ^= msb ^ lsb;
            }

            self.write_byte(checksum).await?;
        } else {
            return Err(Error::InvalidPageCount);
        }

        self.wait_ack().await?;
        Ok(())
    }

    /// Execute the Write Protect command to enable write protection for sectors.
    pub async fn write_protect(&mut self, sectors: &[u8]) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        if sectors.is_empty() {
            return Err(Error::InvalidPageCount);
        }

        self.send_command(Command::WriteProtect).await?;
        self.wait_ack().await?;

        // Send N (number of sectors - 1)
        let n = (sectors.len() - 1) as u8;
        let mut checksum = n;
        self.write_byte(n).await?;

        // Send sector codes
        for &sector in sectors {
            self.write_byte(sector).await?;
            checksum ^= sector;
        }

        self.write_byte(checksum).await?;
        self.wait_ack().await?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Write Unprotect command to disable write protection for all sectors.
    pub async fn write_unprotect(&mut self) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::WriteUnprotect).await?;
        self.wait_ack().await?;
        self.wait_ack().await?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Readout Protect command to enable flash read protection.
    pub async fn readout_protect(&mut self) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::ReadoutProtect).await?;
        self.wait_ack().await?;
        self.wait_ack().await?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Readout Unprotect command to disable flash read protection.
    ///
    /// WARNING: This will erase all flash memory!
    pub async fn readout_unprotect(&mut self) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.send_command(Command::ReadoutUnprotect).await?;
        self.wait_ack().await?;
        self.wait_ack().await?;

        // Note: Device will reset after this command and flash is erased
        Ok(())
    }

    // --- Helper methods ---

    async fn send_command(&mut self, cmd: Command) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let code = cmd as u8;
        self.write_byte(code).await?;
        self.write_byte(!code).await?;
        Ok(())
    }

    async fn send_address(&mut self, address: u32) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let bytes = address.to_be_bytes();
        let checksum = bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3];

        for &byte in &bytes {
            self.write_byte(byte).await?;
        }
        self.write_byte(checksum).await?;

        Ok(())
    }

    async fn wait_ack(&mut self) -> Result<(), Error<R::Error>> {
        let response = self.read_byte().await?;
        match response {
            ACK => Ok(()),
            NACK => Err(Error::Nack),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    async fn read_byte(&mut self) -> Result<u8, Error<R::Error>> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf).await?;
        Ok(buf[0])
    }

    async fn write_byte(&mut self, byte: u8) -> Result<(), Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        self.writer
            .write_all(&[byte])
            .await
            .map_err(|e| Error::Io(e.into()))?;
        self.writer.flush().await.map_err(|e| Error::Io(e.into()))?;
        Ok(())
    }
}
