//! Write a binary application to a target device
//!
//! The [Flasher] struct abstracts over various operations for writing a binary
//! application to a target device. It additionally provides some operations to
//! read information from the target device.


use alloc::borrow::Cow;
use core::str::FromStr;

use embedded_hal_async::delay::DelayNs;
use log::{debug, info, warn};


use object::{Endianness, read::elf::ElfFile32 as ElfFile};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, IntoEnumIterator, VariantNames};

// Re-export SecurityInfo from connection module for backward compatibility
// TODO: Remove in the next major release

pub use super::connection::SecurityInfo;

use super::target::{DefaultProgressCallback, ProgressCallbacks};
use super::{
    Error,
    target::{Chip, XtalFrequency},
};

use super::{
    command::{Command, CommandType},
    connection::{Connection, SerialInterface, reset::ResetBeforeOperation},
    error::{ConnectionError, ResultExt as _},
    flasher::stubs::{
        CHIP_DETECT_MAGIC_REG_ADDR,
        DEFAULT_TIMEOUT,
        EXPECTED_STUB_HANDSHAKE,
        FlashStub,
    },
    image_format::{ImageFormat, Segment, ram_segments, rom_segments},
};


pub(crate) mod stubs;

/// List of SPI parameters to try while detecting flash size

pub(crate) const TRY_SPI_PARAMS: [SpiAttachParams; 2] =
    [SpiAttachParams::default(), SpiAttachParams::esp32_pico_d4()];


pub(crate) const FLASH_SECTOR_SIZE: usize = 0x1000;
pub(crate) const FLASH_WRITE_SIZE: usize = 0x400;

/// Supported flash frequencies
///
/// Note that not all frequencies are supported by each target device.
#[derive(
    Debug, Default, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Display, VariantNames, Serialize, Deserialize,
)]
#[non_exhaustive]
#[repr(u8)]
pub enum FlashFrequency {
    /// 12 MHz
    #[serde(rename = "12MHz")]
    _12Mhz,
    /// 15 MHz
    #[serde(rename = "15MHz")]
    _15Mhz,
    /// 16 MHz
    #[serde(rename = "16MHz")]
    _16Mhz,
    /// 20 MHz
    #[serde(rename = "20MHz")]
    _20Mhz,
    /// 24 MHz
    #[serde(rename = "24MHz")]
    _24Mhz,
    /// 26 MHz
    #[serde(rename = "26MHz")]
    _26Mhz,
    /// 30 MHz
    #[serde(rename = "30MHz")]
    _30Mhz,
    /// 40 MHz
    #[serde(rename = "40MHz")]
    #[default]
    _40Mhz,
    /// 48 MHz
    #[serde(rename = "48MHz")]
    _48Mhz,
    /// 60 MHz
    #[serde(rename = "60MHz")]
    _60Mhz,
    /// 80 MHz
    #[serde(rename = "80MHz")]
    _80Mhz,
}

impl FlashFrequency {
    /// Encodes flash frequency into the format used by the bootloader.
    pub fn encode_flash_frequency(self: FlashFrequency, chip: Chip) -> Result<u8, Error> {
        let encodings = chip.flash_frequency_encodings();
        if let Some(&f) = encodings.get(&self) {
            Ok(f)
        } else {
            Err(Error::UnsupportedFlashFrequency {
                chip,
                frequency: self,
            })
        }
    }
}

/// Supported flash modes
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, VariantNames, Serialize, Deserialize,
)]
#[non_exhaustive]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum FlashMode {
    /// Quad I/O (4 pins used for address & data)
    Qio,
    /// Quad Output (4 pins used for data)
    Qout,
    /// Dual I/O (2 pins used for address & data)
    #[default]
    Dio,
    /// Dual Output (2 pins used for data)
    Dout,
}

/// Supported flash sizes
///
/// Note that not all sizes are supported by each target device.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Display,
    VariantNames,
    EnumIter,
    Deserialize,
    Serialize,
)]
#[non_exhaustive]
#[repr(u8)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
#[doc(alias("esp_image_flash_size_t"))]
pub enum FlashSize {
    /// 256 KB
    #[serde(rename = "256KB")]
    _256Kb,
    /// 512 KB
    #[serde(rename = "512KB")]
    _512Kb,
    /// 1 MB
    #[serde(rename = "1MB")]
    _1Mb,
    /// 2 MB
    #[serde(rename = "2MB")]
    _2Mb,
    /// 4 MB
    #[default]
    #[serde(rename = "4MB")]
    _4Mb,
    /// 8 MB
    #[serde(rename = "8MB")]
    _8Mb,
    /// 16 MB
    #[serde(rename = "16MB")]
    _16Mb,
    /// 32 MB
    #[serde(rename = "32MB")]
    _32Mb,
    /// 64 MB
    #[serde(rename = "64MB")]
    _64Mb,
    /// 128 MB
    #[serde(rename = "128MB")]
    _128Mb,
    /// 256 MB
    #[serde(rename = "256MB")]
    _256Mb,
}

impl FlashSize {
    /// Encodes flash size into the format used by the bootloader.
    ///
    /// ## Values:
    ///
    /// * <https://docs.espressif.com/projects/esptool/en/latest/esp32s3/advanced-topics/firmware-image-format.html#file-header>
    pub const fn encode_flash_size(self: FlashSize) -> Result<u8, Error> {
        use FlashSize::*;

        let encoded = match self {
            _1Mb => 0,
            _2Mb => 1,
            _4Mb => 2,
            _8Mb => 3,
            _16Mb => 4,
            _32Mb => 5,
            _64Mb => 6,
            _128Mb => 7,
            _256Mb => 8,
            _ => return Err(Error::UnsupportedFlash(self as u8)),
        };

        Ok(encoded)
    }

    /// Create a [FlashSize] from an [u8]
    ///
    /// [source](https://github.com/espressif/esptool/blob/f4d2510/esptool/cmds.py#L42)
    pub const fn from_detected(value: u8) -> Result<FlashSize, Error> {
        match value {
            0x12 | 0x32 => Ok(FlashSize::_256Kb),
            0x13 | 0x33 => Ok(FlashSize::_512Kb),
            0x14 | 0x34 => Ok(FlashSize::_1Mb),
            0x15 | 0x35 => Ok(FlashSize::_2Mb),
            0x16 | 0x36 => Ok(FlashSize::_4Mb),
            0x17 | 0x37 => Ok(FlashSize::_8Mb),
            0x18 | 0x38 => Ok(FlashSize::_16Mb),
            0x19 | 0x39 => Ok(FlashSize::_32Mb),
            0x20 | 0x1A | 0x3A => Ok(FlashSize::_64Mb),
            0x21 | 0x1B => Ok(FlashSize::_128Mb),
            0x22 | 0x1C => Ok(FlashSize::_256Mb),
            _ => Err(Error::UnsupportedFlash(value)),
        }
    }

    /// Returns the flash size in bytes
    pub const fn size(self) -> u32 {
        match self {
            FlashSize::_256Kb => 0x0040000,
            FlashSize::_512Kb => 0x0080000,
            FlashSize::_1Mb => 0x0100000,
            FlashSize::_2Mb => 0x0200000,
            FlashSize::_4Mb => 0x0400000,
            FlashSize::_8Mb => 0x0800000,
            FlashSize::_16Mb => 0x1000000,
            FlashSize::_32Mb => 0x2000000,
            FlashSize::_64Mb => 0x4000000,
            FlashSize::_128Mb => 0x8000000,
            FlashSize::_256Mb => 0x10000000,
        }
    }
}

impl FromStr for FlashSize {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FlashSize::VARIANTS
            .iter()
            .copied()
            .zip(FlashSize::iter())
            .find(|(name, _)| *name == s.to_uppercase())
            .map(|(_, variant)| variant)
            .ok_or_else(|| Error::InvalidFlashSize(s.to_string()))
    }
}

/// Flash settings to use when flashing a device.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FlashSettings {
    /// Flash mode.
    pub mode: Option<FlashMode>,
    /// Flash size.
    pub size: Option<FlashSize>,
    /// Flash frequency.
    #[serde(rename = "frequency")]
    pub freq: Option<FlashFrequency>,
}

impl FlashSettings {
    /// Returns the default [FlashSettings] with all fields set to `None`.
    pub const fn default() -> Self {
        FlashSettings {
            mode: None,
            size: None,
            freq: None,
        }
    }

    /// Creates a new [FlashSettings] with the specified mode, size, and
    /// frequency.
    pub fn new(
        mode: Option<FlashMode>,
        size: Option<FlashSize>,
        freq: Option<FlashFrequency>,
    ) -> Self {
        FlashSettings { mode, size, freq }
    }
}

/// Flash data and configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[non_exhaustive]
pub struct FlashData {
    /// Flash settings.
    pub flash_settings: FlashSettings,
    /// Minimum chip revision.
    pub min_chip_rev: u16,
    /// MMU page size.
    pub mmu_page_size: Option<u32>,
    /// Target chip.
    pub chip: Chip,
    /// Crystal frequency.
    pub xtal_freq: XtalFrequency,
}

impl FlashData {
    /// Creates a new [`FlashData`] object.
    pub fn new(
        flash_settings: FlashSettings,
        min_chip_rev: u16,
        mmu_page_size: Option<u32>,
        chip: Chip,
        xtal_freq: XtalFrequency,
    ) -> Self {
        FlashData {
            flash_settings,
            min_chip_rev,
            mmu_page_size,
            chip,
            xtal_freq,
        }
    }
}

/// Parameters of the attached SPI flash chip (sizes, etc).
///
/// See: <https://github.com/espressif/esptool/blob/da31d9d/esptool.py#L655>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[repr(C)]
pub struct SpiSetParams {
    /// Flash chip ID
    fl_id: u32,
    /// Total size in bytes
    total_size: u32,
    /// Block size
    block_size: u32,
    /// Sector size
    sector_size: u32,
    /// Page size
    page_size: u32,
    /// Status mask
    status_mask: u32,
}

impl SpiSetParams {
    /// Create a new [SpiSetParams] with the specified size.
    pub const fn default(size: u32) -> Self {
        SpiSetParams {
            fl_id: 0,
            total_size: size,
            block_size: 64 * 1024,
            sector_size: 4 * 1024,
            page_size: 256,
            status_mask: 0xFFFF,
        }
    }

    /// Encode the parameters into a byte array
    pub fn encode(&self) -> Vec<u8> {
        let mut encoded: Vec<u8> = Vec::new();
        encoded.extend_from_slice(&self.fl_id.to_le_bytes());
        encoded.extend_from_slice(&self.total_size.to_le_bytes());
        encoded.extend_from_slice(&self.block_size.to_le_bytes());
        encoded.extend_from_slice(&self.sector_size.to_le_bytes());
        encoded.extend_from_slice(&self.page_size.to_le_bytes());
        encoded.extend_from_slice(&self.status_mask.to_le_bytes());
        encoded
    }
}

/// Parameters for attaching to a target devices SPI flash
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[repr(C)]
pub struct SpiAttachParams {
    clk: u8,
    q: u8,
    d: u8,
    hd: u8,
    cs: u8,
}

impl SpiAttachParams {
    /// Create a new [SpiAttachParams] with default values.
    pub const fn default() -> Self {
        SpiAttachParams {
            clk: 0,
            q: 0,
            d: 0,
            hd: 0,
            cs: 0,
        }
    }

    /// Default SPI parameters for ESP32-PICO-D4.
    pub const fn esp32_pico_d4() -> Self {
        SpiAttachParams {
            clk: 6,
            q: 17,
            d: 8,
            hd: 11,
            cs: 16,
        }
    }

    /// Encode the parameters into a byte array
    pub fn encode(self, stub: bool) -> Vec<u8> {
        let packed = ((self.hd as u32) << 24)
            | ((self.cs as u32) << 18)
            | ((self.d as u32) << 12)
            | ((self.q as u32) << 6)
            | (self.clk as u32);

        let mut encoded: Vec<u8> = packed.to_le_bytes().to_vec();

        if !stub {
            encoded.append(&mut vec![0u8; 4]);
        }

        encoded
    }
}

/// Information about the connected device
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct DeviceInfo {
    /// The chip being used
    pub chip: Chip,
    /// The revision of the chip
    pub revision: Option<(u32, u32)>,
    /// The crystal frequency of the chip
    pub crystal_frequency: XtalFrequency,
    /// The total available flash size
    pub flash_size: FlashSize,
    /// Device features
    pub features: Vec<String>,
    /// MAC address
    pub mac_address: Option<String>,
}

impl DeviceInfo {
    #[doc(hidden)]
    pub fn rom(&self) -> Option<Vec<u8>> {
        match self.chip {
            Chip::Esp32 => {
                if let Some((_, minor)) = self.revision {
                    if minor >= 3 {
                        Some(include_bytes!("../resources/roms/esp32_rev300_rom.elf").into())
                    } else {
                        Some(include_bytes!("../resources/roms/esp32_rev0_rom.elf").into())
                    }
                } else {
                    None
                }
            }
            Chip::Esp32c2 => {
                Some(include_bytes!("../resources/roms/esp32c2_rev100_rom.elf").into())
            }
            Chip::Esp32c3 => {
                if let Some((_, minor)) = self.revision {
                    if minor >= 3 {
                        Some(include_bytes!("../resources/roms/esp32c3_rev3_rom.elf").into())
                    } else {
                        Some(include_bytes!("../resources/roms/esp32c3_rev0_rom.elf").into())
                    }
                } else {
                    None
                }
            }
            Chip::Esp32c5 => None,
            Chip::Esp32c6 => {
                Some(include_bytes!("../resources/roms/esp32c6_rev0_rom.elf").into())
            }
            Chip::Esp32h2 => {
                Some(include_bytes!("../resources/roms/esp32h2_rev0_rom.elf").into())
            }
            Chip::Esp32p4 => {
                Some(include_bytes!("../resources/roms/esp32p4_rev0_rom.elf").into())
            }
            Chip::Esp32s2 => {
                Some(include_bytes!("../resources/roms/esp32s2_rev0_rom.elf").into())
            }
            Chip::Esp32s3 => {
                Some(include_bytes!("../resources/roms/esp32s3_rev0_rom.elf").into())
            }
        }
    }
}

/// Connect to and flash a target device

#[derive(Debug)]
pub struct Flasher<P: SerialInterface> {
    /// Connection for flash operations
    connection: Connection<P>,
    /// Chip ID
    chip: Chip,
    /// Flash size, loaded from SPI flash
    flash_size: FlashSize,
    /// Configuration for SPI attached flash (0 to use fused values)
    spi_params: SpiAttachParams,
    /// Indicate RAM stub loader is in use
    use_stub: bool,
    /// Indicate verifying flash contents after flashing
    verify: bool,
    /// Indicate skipping of already flashed regions
    skip: bool,
}


impl<P: SerialInterface> Flasher<P> {
    /// The serial port's baud rate should be 115_200 to connect. After
    /// connecting, Flasher will change the baud rate to the `baud`
    /// parameter.
    pub async fn connect(
        mut connection: Connection<P>,
        use_stub: bool,
        verify: bool,
        skip: bool,
        chip: Option<Chip>,
        baud: Option<u32>,
    ) -> Result<Self, Error> {
        // The connection should already be established with the device using the
        // default baud rate of 115,200 and timeout of 3 seconds.
        connection.begin().await?;
        connection.set_timeout(DEFAULT_TIMEOUT)?;

        detect_sdm(&mut connection).await;

        let detected_chip = if connection.before_operation() != ResetBeforeOperation::NoResetNoSync
        {
            // Detect which chip we are connected to.
            let detected_chip = connection.detect_chip(use_stub).await?;
            if let Some(chip) = chip {
                if chip != detected_chip {
                    return Err(Error::ChipMismatch(
                        chip.to_string(),
                        detected_chip.to_string(),
                    ));
                }
            }
            detected_chip
        } else if connection.before_operation() == ResetBeforeOperation::NoResetNoSync
            && chip.is_some()
        {
            chip.unwrap()
        } else {
            return Err(Error::ChipNotProvided);
        };

        let mut flasher = Flasher {
            connection,
            chip: detected_chip,
            flash_size: FlashSize::_4Mb,
            spi_params: SpiAttachParams::default(),
            use_stub,
            verify,
            skip,
        };

        if flasher.connection.before_operation() == ResetBeforeOperation::NoResetNoSync {
            return Ok(flasher);
        }

        if !flasher.connection.secure_download_mode {
            // Load flash stub if enabled.
            if use_stub {
                info!("Using flash stub");
                flasher.load_stub().await?;
            }
            // Flash size autodetection doesn't work in Secure Download Mode.
            flasher.spi_autodetect().await?;
        } else if use_stub {
            warn!("Stub is not supported in Secure Download Mode, setting --no-stub");
            flasher.use_stub = false;
        }

        // Now that we have established a connection and detected the chip and flash
        // size, we can set the baud rate of the connection to the configured value.
        if let Some(baud) = baud {
            if baud > 115_200 {
                warn!("Setting baud rate higher than 115,200 can cause issues");
                flasher.change_baud(baud).await?;
            }
        }

        Ok(flasher)
    }

    /// Set the flash size.
    pub fn set_flash_size(&mut self, flash_size: FlashSize) {
        self.flash_size = flash_size;
    }

    /// Disable the watchdog timer.
    pub async fn disable_watchdog(&mut self) -> Result<(), Error> {
        let mut target = self
            .chip
            .flash_target(self.spi_params, self.use_stub, false, false);
        target.begin(&mut self.connection).await.flashing()?;
        Ok(())
    }

    async fn load_stub(&mut self) -> Result<(), Error> {
        debug!("Loading flash stub for chip: {:?}", self.chip);

        // Load flash stub
        let stub = FlashStub::get(self.chip);

        let mut ram_target = self
            .chip
            .ram_target(Some(stub.entry()), self.chip.max_ram_block_size());
        ram_target.begin(&mut self.connection).await.flashing()?;

        let (text_addr, text) = stub.text();
        debug!("Write {} byte stub text", text.len());

        ram_target
            .write_segment(
                &mut self.connection,
                Segment {
                    addr: text_addr,
                    data: Cow::Borrowed(&text),
                },
                &mut DefaultProgressCallback,
            )
            .await
            .flashing()?;

        let (data_addr, data) = stub.data();
        debug!("Write {} byte stub data", data.len());

        ram_target
            .write_segment(
                &mut self.connection,
                Segment {
                    addr: data_addr,
                    data: Cow::Borrowed(&data),
                },
                &mut DefaultProgressCallback,
            )
            .await
            .flashing()?;

        debug!("Finish stub write");
        ram_target.finish(&mut self.connection, true).await.flashing()?;

        debug!("Stub written!");

        match self.connection.read(EXPECTED_STUB_HANDSHAKE.len())? {
            Some(resp) if resp == EXPECTED_STUB_HANDSHAKE.as_bytes() => Ok(()),
            _ => Err(Error::Connection(Box::new(
                ConnectionError::InvalidStubHandshake,
            ))),
        }?;

        // Re-detect chip to check stub is up
        let chip = self.connection.detect_chip(self.use_stub).await?;
        debug!("Re-detected chip: {chip:?}");

        Ok(())
    }

    async fn spi_autodetect(&mut self) -> Result<(), Error> {
        // Loop over all available SPI parameters until we find one that successfully
        // reads the flash size.
        for spi_params in TRY_SPI_PARAMS.iter().copied() {
            debug!("Attempting flash enable with: {spi_params:?}");

            // Send `SpiAttach` to enable flash, in some instances this command
            // may fail while the flash connection succeeds
            if let Err(_e) = self.enable_flash(spi_params).await {
                debug!("Flash enable failed");
            }

            if let Some(flash_size) = self.flash_detect().await? {
                debug!("Flash detect OK!");

                // Flash detection was successful, so save the flash size and SPI parameters and
                // return.
                self.flash_size = flash_size;
                self.spi_params = spi_params;

                let spi_set_params = SpiSetParams::default(self.flash_size.size());
                let old_timeout = self.connection.serial.timeout();
                self.connection.serial.set_timeout(CommandType::SpiSetParams.timeout())?;
                let result = self.connection.command(Command::SpiSetParams {
                    spi_params: spi_set_params,
                }).await;
                self.connection.serial.set_timeout(old_timeout)?;
                result?;

                return Ok(());
            }

            debug!("Flash detect failed");
        }

        debug!("SPI flash autodetection failed");

        // None of the SPI parameters were successful.
        Err(Error::FlashConnect)
    }

    /// Detect the flash size of the connected device.
    pub async fn flash_detect(&mut self) -> Result<Option<FlashSize>, Error> {
        const FLASH_RETRY: u8 = 0xFF;

        let flash_id = self.spi_command(CommandType::FlashDetect, &[], 24).await?;
        let size_id = (flash_id >> 16) as u8;

        // This value indicates that an alternate detection method should be tried.
        if size_id == FLASH_RETRY {
            return Ok(None);
        }

        let flash_size = match FlashSize::from_detected(size_id) {
            Ok(size) => size,
            Err(_) => {
                warn!(
                    "Could not detect flash size (FlashID=0x{flash_id:02X}, SizeID=0x{size_id:02X}), defaulting to 4MB"
                );
                FlashSize::default()
            }
        };

        Ok(Some(flash_size))
    }

    async fn enable_flash(&mut self, spi_params: SpiAttachParams) -> Result<(), Error> {
        let old_timeout = self.connection.serial.timeout();
        self.connection.serial.set_timeout(CommandType::SpiAttach.timeout())?;
        let result = self.connection.command(if self.use_stub {
            Command::SpiAttachStub { spi_params }
        } else {
            Command::SpiAttach { spi_params }
        }).await;
        self.connection.serial.set_timeout(old_timeout)?;
        result?;

        Ok(())
    }

    async fn spi_command(
        &mut self,
        command: CommandType,
        data: &[u8],
        read_bits: u32,
    ) -> Result<u32, Error> {
        assert!(read_bits < 32);
        assert!(data.len() < 64);

        let spi_registers = self.chip.spi_registers();

        let old_spi_usr = self.connection.read_reg(spi_registers.usr()).await?;
        let old_spi_usr2 = self.connection.read_reg(spi_registers.usr2()).await?;

        let mut flags = 1 << 31;
        if !data.is_empty() {
            flags |= 1 << 27;
        }
        if read_bits > 0 {
            flags |= 1 << 28;
        }

        self.connection
            .write_reg(spi_registers.usr(), flags, None).await?;
        self.connection
            .write_reg(spi_registers.usr2(), (7 << 28) | command as u32, None).await?;

        if let (Some(mosi_data_length), Some(miso_data_length)) =
            (spi_registers.mosi_length(), spi_registers.miso_length())
        {
            if !data.is_empty() {
                self.connection
                    .write_reg(mosi_data_length, data.len() as u32 * 8 - 1, None).await?;
            }
            if read_bits > 0 {
                self.connection
                    .write_reg(miso_data_length, read_bits - 1, None).await?;
            }
        } else {
            let mosi_mask = if data.is_empty() {
                0
            } else {
                data.len() as u32 * 8 - 1
            };
            let miso_mask = if read_bits == 0 { 0 } else { read_bits - 1 };
            self.connection.write_reg(
                spi_registers.usr1(),
                (miso_mask << 8) | (mosi_mask << 17),
                None,
            ).await?;
        }

        if data.is_empty() {
            self.connection.write_reg(spi_registers.w0(), 0, None).await?;
        } else {
            for (i, bytes) in data.chunks(4).enumerate() {
                let mut data_bytes = [0; 4];
                data_bytes[0..bytes.len()].copy_from_slice(bytes);
                let data = u32::from_le_bytes(data_bytes);
                self.connection
                    .write_reg(spi_registers.w0() + i as u32, data, None).await?;
            }
        }

        self.connection
            .write_reg(spi_registers.cmd(), 1 << 18, None).await?;

        let mut i = 0;
        loop {
            self.connection.delay().delay_ms(1).await;
            if self.connection.read_reg(spi_registers.usr()).await? & (1 << 18) == 0 {
                break;
            }
            i += 1;
            if i > 10 {
                return Err(Error::Connection(Box::new(ConnectionError::Timeout(
                    command.into(),
                ))));
            }
        }

        let result = self.connection.read_reg(spi_registers.w0()).await?;
        self.connection
            .write_reg(spi_registers.usr(), old_spi_usr, None).await?;
        self.connection
            .write_reg(spi_registers.usr2(), old_spi_usr2, None).await?;

        Ok(result)
    }

    /// The active serial connection being used by the flasher
    pub fn connection(&mut self) -> &mut Connection<P> {
        &mut self.connection
    }

    /// The chip type that the flasher is connected to
    pub fn chip(&self) -> Chip {
        self.chip
    }

    /// Read and print any information we can about the connected device
    pub async fn device_info(&mut self) -> Result<DeviceInfo, Error> {
        let chip = self.chip();
        // chip_revision reads from efuse, which is not possible in Secure Download Mode
        let revision = if !self.connection.secure_download_mode {
            Some(chip.revision(&mut self.connection).await?)
        } else {
            None
        };

        let crystal_frequency = chip.xtal_frequency(&mut self.connection).await?;
        let features = chip
            .chip_features(&mut self.connection).await?
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();

        let mac_address = if !self.connection.secure_download_mode {
            Some(chip.mac_address(&mut self.connection).await?)
        } else {
            None
        };

        let info = DeviceInfo {
            chip,
            revision,
            crystal_frequency,
            flash_size: self.flash_size,
            features,
            mac_address,
        };

        Ok(info)
    }

    /// Load an ELF image to RAM and execute it
    ///
    /// Note that this will not touch the flash on the device
    pub async fn load_elf_to_ram(
        &mut self,
        elf_data: &[u8],
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), Error> {
        let elf = ElfFile::parse(elf_data)?;
        if rom_segments(self.chip, &elf).next().is_some() {
            return Err(Error::ElfNotRamLoadable);
        }

        let mut target = self.chip.ram_target(
            Some(elf.elf_header().e_entry.get(Endianness::Little)),
            self.chip.max_ram_block_size(),
        );
        target.begin(&mut self.connection).await.flashing()?;

        for segment in ram_segments(self.chip, &elf) {
            target
                .write_segment(&mut self.connection, segment, progress)
                .await
                .flashing()?;
        }

        target.finish(&mut self.connection, true).await.flashing()
    }

    /// Load an ELF image to flash and execute it
    pub async fn load_image_to_flash<'a>(
        &mut self,
        progress: &mut dyn ProgressCallbacks,
        image_format: ImageFormat<'a>,
    ) -> Result<(), Error> {
        let mut target =
            self.chip
                .flash_target(self.spi_params, self.use_stub, self.verify, self.skip);
        target.begin(&mut self.connection).await.flashing()?;

        for segment in image_format.flash_segments() {
            target
                .write_segment(&mut self.connection, segment, progress)
                .await
                .flashing()?;
        }

        target.finish(&mut self.connection, true).await.flashing()?;

        Ok(())
    }

    /// Load an bin image to flash at a specific address
    pub async fn write_bin_to_flash(
        &mut self,
        addr: u32,
        data: &[u8],
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), Error> {
        let mut segment = Segment {
            addr,
            data: Cow::from(data),
        };

        // If the file size is not divisible by 4, we need to pad `FF` bytes to the end
        let size = segment.data.len();
        if size % 4 != 0 {
            let padded_bytes = 4 - (size % 4);
            segment
                .data
                .to_mut()
                .extend(core::iter::repeat_n(0xFF, padded_bytes));
        }

        self.write_bins_to_flash(&[segment], progress).await?;

        info!("Binary successfully written to flash!");

        Ok(())
    }

    /// Load multiple bin images to flash at specific addresses
    pub async fn write_bins_to_flash(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), Error> {
        if self.connection.secure_download_mode {
            // NOTE: SDM check disabled for tunnel-based flashing where the custom
            // protocol was previously able to flash. Proceed with caution.
            log::warn!("Secure Download Mode detected but proceeding anyway");
        }

        let mut target =
            self.chip
                .flash_target(self.spi_params, self.use_stub, self.verify, self.skip);

        target.begin(&mut self.connection).await.flashing()?;

        for segment in segments {
            target.write_segment(&mut self.connection, segment.borrow(), progress).await?;
        }

        target.finish(&mut self.connection, true).await.flashing()?;

        Ok(())
    }

    /// Get MD5 of region
    pub async fn checksum_md5(&mut self, addr: u32, length: u32) -> Result<u128, Error> {
        let old_timeout = self.connection.serial.timeout();
        self.connection.serial.set_timeout(CommandType::FlashMd5.timeout_for_size(length))?;
        let result = self.connection
            .command(Command::FlashMd5 {
                offset: addr,
                size: length,
            }).await;
        self.connection.serial.set_timeout(old_timeout)?;
        result?.try_into()
    }

    /// Get security info.
    // TODO: Deprecate this method in the next major release
    pub async fn security_info(&mut self) -> Result<SecurityInfo, Error> {
        self.connection.security_info(self.use_stub).await
    }

    /// Change the baud rate of the connection.
    pub async fn change_baud(&mut self, baud: u32) -> Result<(), Error> {
        debug!("Change baud to: {baud}");

        let prior_baud = match self.use_stub {
            true => self.connection.baud()?,
            false => 0,
        };

        let xtal_freq = self.chip.xtal_frequency(&mut self.connection).await?;

        // Probably this is just a temporary solution until the next chip revision.
        //
        // The ROM code thinks it uses a 40 MHz XTAL. Recompute the baud rate in order
        // to trick the ROM code to set the correct baud rate for a 26 MHz XTAL.
        let mut new_baud = baud;
        if self.chip == Chip::Esp32c2 && !self.use_stub && xtal_freq == XtalFrequency::_26Mhz {
            new_baud = new_baud * 40 / 26;
        }

        let old_timeout = self.connection.serial.timeout();
        self.connection.serial.set_timeout(CommandType::ChangeBaudrate.timeout())?;
        let result = self.connection.command(Command::ChangeBaudrate {
            new_baud,
            prior_baud,
        }).await;
        self.connection.serial.set_timeout(old_timeout)?;
        result?;
        self.connection.set_baud(baud)?;
        self.connection.delay().delay_ms(50).await;
        self.connection.flush()?;

        Ok(())
    }

    /// Erase a region of flash.
    pub async fn erase_region(&mut self, offset: u32, size: u32) -> Result<(), Error> {
        debug!("Erasing region of 0x{size:x}B at 0x{offset:08x}");

        let old_timeout = self.connection.serial.timeout();
        self.connection.serial.set_timeout(CommandType::EraseRegion.timeout_for_size(size))?;
        let result = self.connection.command(Command::EraseRegion { offset, size }).await;
        self.connection.serial.set_timeout(old_timeout)?;
        result?;
        self.connection.delay().delay_ms(50).await;
        self.connection.flush()?;
        Ok(())
    }

    /// Erase entire flash.
    pub async fn erase_flash(&mut self) -> Result<(), Error> {
        debug!("Erasing the entire flash");

        let old_timeout = self.connection.serial.timeout();
        self.connection.serial.set_timeout(CommandType::EraseFlash.timeout())?;
        let result = self.connection.command(Command::EraseFlash).await;
        self.connection.serial.set_timeout(old_timeout)?;
        result?;
        self.connection.delay().delay_ms(50).await;
        self.connection.flush()?;

        Ok(())
    }

    /// Verify the minimum chip revision.
    pub async fn verify_minimum_revision(&mut self, minimum: u16) -> Result<(), Error> {
        let chip = self.chip;
        let (major, minor) = chip.revision(&mut self.connection).await?;
        let revision = (major * 100 + minor) as u16;
        if revision < minimum {
            return Err(Error::UnsupportedChipRevision {
                major: minimum / 100,
                minor: minimum % 100,
                found_major: revision / 100,
                found_minor: revision % 100,
            });
        }

        Ok(())
    }

    /// Consume self and return the underlying connection.
    pub fn into_connection(self) -> Connection<P> {
        self.connection
    }
}


async fn detect_sdm<P: SerialInterface>(connection: &mut Connection<P>) {
    if let Ok(security_info) = connection.security_info(false).await {
        // Newer chips tell us if SDM is enabled.
        connection.secure_download_mode =
            security_info.security_flag_status("SECURE_DOWNLOAD_ENABLE");
    } else if connection.read_reg(CHIP_DETECT_MAGIC_REG_ADDR).await.is_err() {
        // On older chips, we have to guess by reading something. On these chips, there
        // is always something readable at 0x40001000.
        log::warn!("Secure Download Mode is enabled on this chip");
        connection.secure_download_mode = true;
    }
}


impl<P: SerialInterface> From<Flasher<P>> for Connection<P> {
    fn from(flasher: Flasher<P>) -> Self {
        flasher.into_connection()
    }
}
