//! STM32 USART bootloader protocol (AN3155) implementation.
//!
//! This module implements the host side of the STM32 bootloader protocol
//! as described in ST Application Note AN3155.

use std::io::{Read, Write};

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
#[derive(Debug)]
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

impl From<std::io::Error> for Error<std::io::Error> {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::UnexpectedEof
        } else {
            Error::Io(e)
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
    pub fn init(&mut self) -> Result<(), Error<std::io::Error>> {
        self.write_byte(INIT)?;
        self.wait_ack()
    }

    /// Execute the Get command to retrieve bootloader version and supported commands.
    pub fn get(&mut self) -> Result<GetResponse, Error<std::io::Error>> {
        self.send_command(Command::Get)?;
        self.wait_ack()?;

        // Read number of bytes (N) - version and commands follow
        let n = self.read_byte()? as usize;

        // Read version
        let version = self.read_byte()?;

        // Read supported commands (N bytes remaining)
        let mut commands = [0u8; 16];
        let command_count = n.min(16);
        for slot in commands.iter_mut().take(command_count) {
            *slot = self.read_byte()?;
        }
        // Read any remaining commands beyond array capacity
        for _ in command_count..n {
            let _ = self.read_byte()?;
        }

        self.wait_ack()?;

        Ok(GetResponse {
            version,
            commands,
            command_count,
        })
    }

    /// Execute the Get Version command to retrieve the protocol version.
    pub fn get_version(&mut self) -> Result<VersionResponse, Error<std::io::Error>> {
        self.send_command(Command::GetVersion)?;
        self.wait_ack()?;

        let version = self.read_byte()?;
        let option1 = self.read_byte()?;
        let option2 = self.read_byte()?;

        self.wait_ack()?;

        Ok(VersionResponse {
            version,
            option1,
            option2,
        })
    }

    /// Execute the Get ID command to retrieve the chip product ID.
    ///
    /// Returns the product ID as a 16-bit value (MSB first from bootloader).
    pub fn get_id(&mut self) -> Result<u16, Error<std::io::Error>> {
        self.send_command(Command::GetId)?;
        self.wait_ack()?;

        // N = number of bytes - 1 (always 1 for STM32, meaning 2 bytes)
        let _n = self.read_byte()?;

        // Read PID (2 bytes, MSB first)
        let msb = self.read_byte()?;
        let lsb = self.read_byte()?;

        self.wait_ack()?;

        Ok(((msb as u16) << 8) | (lsb as u16))
    }

    /// Execute the Read Memory command to read data from memory.
    ///
    /// Reads up to 256 bytes starting at the given address into the provided buffer.
    /// Returns the number of bytes read.
    pub fn read_memory(
        &mut self,
        address: u32,
        buffer: &mut [u8],
    ) -> Result<usize, Error<std::io::Error>> {
        if buffer.is_empty() {
            return Ok(0);
        }

        let len = buffer.len().min(256);

        self.send_command(Command::ReadMemory)?;
        self.wait_ack()?;

        // Send address with checksum
        self.send_address(address)?;
        self.wait_ack()?;

        // Send number of bytes - 1 and its complement
        let n = (len - 1) as u8;
        self.write_bytes(&[n, !n])?;
        self.wait_ack()?;

        // Read data in bulk
        self.reader.read_exact(&mut buffer[..len])?;

        Ok(len)
    }

    /// Execute the Go command to jump to an address and execute code.
    ///
    /// The bootloader will initialize the stack pointer from address and
    /// jump to address+4 (the reset handler).
    pub fn go(&mut self, address: u32) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::Go)?;
        self.wait_ack()?;

        self.send_address(address)?;
        self.wait_ack()?;

        Ok(())
    }

    /// Execute the Write Memory command to write data to memory.
    ///
    /// Writes up to 256 bytes starting at the given address.
    /// The data length must be a multiple of 4 bytes.
    pub fn write_memory(&mut self, address: u32, data: &[u8]) -> Result<(), Error<std::io::Error>> {
        if data.is_empty() {
            return Ok(());
        }

        if data.len() > 256 {
            return Err(Error::DataTooLarge);
        }

        self.send_command(Command::WriteMemory)?;
        self.wait_ack()?;

        // Send address with checksum
        self.send_address(address)?;
        self.wait_ack()?;

        // Send N (number of bytes - 1), data, and checksum
        let n = (data.len() - 1) as u8;
        let mut checksum = n;
        for &byte in data {
            checksum ^= byte;
        }

        self.write_byte(n)?;
        self.write_bytes(data)?;
        self.write_byte(checksum)?;
        self.wait_ack()?;

        Ok(())
    }

    /// Execute the Erase command (legacy, for devices with <=256 pages).
    ///
    /// Pass `None` for global erase, or `Some(&pages)` to erase specific pages.
    /// Each page number is a single byte.
    pub fn erase(&mut self, pages: Option<&[u8]>) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::Erase)?;
        self.wait_ack()?;

        match pages {
            None => {
                // Global erase: send 0xFF, 0x00
                self.write_bytes(&[0xFF, 0x00])?;
            }
            Some(pages) => {
                if pages.is_empty() || pages.len() > 256 {
                    return Err(Error::InvalidPageCount);
                }

                // Build packet: N, page numbers, checksum
                let n = (pages.len() - 1) as u8;
                let mut checksum = n;
                for &page in pages {
                    checksum ^= page;
                }

                self.write_byte(n)?;
                self.write_bytes(pages)?;
                self.write_byte(checksum)?;
            }
        }

        self.wait_ack()?;
        Ok(())
    }

    /// Execute the Extended Erase command (for devices with >256 pages).
    ///
    /// Pass `None` for a special erase (mass/bank), or `Some(&pages)` to erase specific pages.
    /// Each page number is a 16-bit value.
    pub fn extended_erase(
        &mut self,
        pages: Option<&[u16]>,
        special: Option<SpecialErase>,
    ) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::ExtendedErase)?;
        self.wait_ack()?;

        if let Some(special) = special {
            // Special erase (mass erase, bank erase)
            let code = special.code();
            let msb = (code >> 8) as u8;
            let lsb = (code & 0xFF) as u8;
            self.write_bytes(&[msb, lsb, msb ^ lsb])?;
        } else if let Some(pages) = pages {
            if pages.is_empty() {
                return Err(Error::InvalidPageCount);
            }

            // Send N (number of pages - 1) as 2 bytes
            let n = (pages.len() - 1) as u16;
            let n_msb = (n >> 8) as u8;
            let n_lsb = (n & 0xFF) as u8;

            let mut checksum = n_msb ^ n_lsb;
            self.write_bytes(&[n_msb, n_lsb])?;

            // Send page numbers (2 bytes each, MSB first)
            for &page in pages {
                let msb = (page >> 8) as u8;
                let lsb = (page & 0xFF) as u8;
                self.write_bytes(&[msb, lsb])?;
                checksum ^= msb ^ lsb;
            }

            self.write_byte(checksum)?;
        } else {
            return Err(Error::InvalidPageCount);
        }

        self.wait_ack()?;
        Ok(())
    }

    /// Execute the Write Protect command to enable write protection for sectors.
    pub fn write_protect(&mut self, sectors: &[u8]) -> Result<(), Error<std::io::Error>> {
        if sectors.is_empty() {
            return Err(Error::InvalidPageCount);
        }

        self.send_command(Command::WriteProtect)?;
        self.wait_ack()?;

        // Send N (number of sectors - 1)
        let n = (sectors.len() - 1) as u8;
        let mut checksum = n;
        self.write_byte(n)?;

        // Send sector codes
        for &sector in sectors {
            self.write_byte(sector)?;
            checksum ^= sector;
        }

        self.write_byte(checksum)?;
        self.wait_ack()?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Write Unprotect command to disable write protection for all sectors.
    pub fn write_unprotect(&mut self) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::WriteUnprotect)?;
        self.wait_ack()?;
        self.wait_ack()?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Readout Protect command to enable flash read protection.
    pub fn readout_protect(&mut self) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::ReadoutProtect)?;
        self.wait_ack()?;
        self.wait_ack()?;

        // Note: Device will reset after this command
        Ok(())
    }

    /// Execute the Readout Unprotect command to disable flash read protection.
    ///
    /// WARNING: This will erase all flash memory!
    pub fn readout_unprotect(&mut self) -> Result<(), Error<std::io::Error>> {
        self.send_command(Command::ReadoutUnprotect)?;
        self.wait_ack()?;
        self.wait_ack()?;

        // Note: Device will reset after this command and flash is erased
        Ok(())
    }

    // --- Helper methods ---

    fn send_command(&mut self, cmd: Command) -> Result<(), Error<std::io::Error>> {
        let code = cmd as u8;
        self.write_bytes(&[code, !code])?;
        Ok(())
    }

    fn send_address(&mut self, address: u32) -> Result<(), Error<std::io::Error>> {
        let bytes = address.to_be_bytes();
        let checksum = bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3];
        let packet = [bytes[0], bytes[1], bytes[2], bytes[3], checksum];
        self.write_bytes(&packet)?;
        Ok(())
    }

    fn wait_ack(&mut self) -> Result<(), Error<std::io::Error>> {
        self.flush()?;
        let response = self.read_byte()?;
        match response {
            ACK => Ok(()),
            NACK => Err(Error::Nack),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    fn read_byte(&mut self) -> Result<u8, Error<std::io::Error>> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn write_byte(&mut self, byte: u8) -> Result<(), Error<std::io::Error>> {
        self.writer.write_all(&[byte])?;
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error<std::io::Error>> {
        self.writer.write_all(bytes)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error<std::io::Error>> {
        self.writer.flush()?;
        Ok(())
    }
}

/// Get a human-readable name for an STM32 chip product ID.
///
/// Product IDs are returned by the Get ID bootloader command.
pub fn chip_name(product_id: u16) -> &'static str {
    match product_id {
        0x410 => "STM32F1 Medium-density",
        0x411 => "STM32F2",
        0x412 => "STM32F1 Low-density",
        0x413 => "STM32F4 (405/407/415/417)",
        0x414 => "STM32F1 High-density",
        0x415 => "STM32L4 (75/76)",
        0x416 => "STM32L1 Medium-density",
        0x417 => "STM32L0 (51/52/53/62/63)",
        0x418 => "STM32F1 Connectivity line",
        0x419 => "STM32F4 (27/29/37/39/69/79)",
        0x420 => "STM32F1 Medium-density VL",
        0x421 => "STM32F446",
        0x440 => "STM32F0 (30/51/71)",
        0x442 => "STM32F0 (30/91/98)",
        0x443 => "STM32F0 (3/4/5)",
        0x444 => "STM32F0 (3/4) small",
        0x445 => "STM32F0 (4/7)",
        0x448 => "STM32F0 (70/71/72)",
        0x460 => "STM32G0 (70/71/B1)",
        0x466 => "STM32G0 (30/31/41)",
        0x467 => "STM32G0 (B0/C1)",
        _ => "Unknown",
    }
}

/// Get a human-readable name for a bootloader command code.
pub fn command_name(code: u8) -> &'static str {
    match code {
        0x00 => "Get",
        0x01 => "Get Version",
        0x02 => "Get ID",
        0x11 => "Read Memory",
        0x21 => "Go",
        0x31 => "Write Memory",
        0x43 => "Erase",
        0x44 => "Extended Erase",
        0x63 => "Write Protect",
        0x73 => "Write Unprotect",
        0x82 => "Readout Protect",
        0x92 => "Readout Unprotect",
        _ => "Unknown",
    }
}
