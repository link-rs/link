//! EEPROM storage for persistent data on the UI chip.

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

/// EEPROM storage for persistent data.
///
/// Provides access to version and SFrame key fields stored in I2C EEPROM.
/// Holds mutable references to shared I2C bus and delay.
pub struct Eeprom<'a, I, D> {
    i2c: &'a mut I,
    delay: &'a mut D,
}

impl<'a, I, D> Eeprom<'a, I, D>
where
    I: I2c,
    D: DelayNs,
{
    const I2C_ADDR: u8 = 0x50;
    const VERSION_OFFSET: u8 = 0;
    const SFRAME_KEY_OFFSET: u8 = 16;
    const WRITE_DELAY_NS: u32 = 10_000_000;

    /// Create a new EEPROM interface from shared I2C and delay references.
    pub fn new(i2c: &'a mut I, delay: &'a mut D) -> Self {
        Self { i2c, delay }
    }

    /// Read the version field (4 bytes big-endian u32 at offset 0).
    pub fn get_version(&mut self) -> Result<u32, I::Error> {
        let mut buf = [0u8; 4];
        self.i2c
            .write_read(Self::I2C_ADDR, &[Self::VERSION_OFFSET], &mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    /// Write the version field (4 bytes big-endian u32 at offset 0).
    pub fn set_version(&mut self, version: u32) -> Result<(), I::Error> {
        let mut write_data = [0u8; 5];
        write_data[0] = Self::VERSION_OFFSET;
        write_data[1..].copy_from_slice(&version.to_be_bytes());
        self.i2c.write(Self::I2C_ADDR, &write_data)?;
        self.delay.delay_ns(Self::WRITE_DELAY_NS);
        Ok(())
    }

    /// Read the SFrame key field (16 bytes at offset 16).
    pub fn get_sframe_key(&mut self) -> Result<[u8; 16], I::Error> {
        let mut buf = [0u8; 16];
        self.i2c
            .write_read(Self::I2C_ADDR, &[Self::SFRAME_KEY_OFFSET], &mut buf)?;
        Ok(buf)
    }

    /// Write the SFrame key field (16 bytes at offset 16).
    pub fn set_sframe_key(&mut self, key: &[u8; 16]) -> Result<(), I::Error> {
        let mut write_data = [0u8; 17];
        write_data[0] = Self::SFRAME_KEY_OFFSET;
        write_data[1..].copy_from_slice(key);
        self.i2c.write(Self::I2C_ADDR, &write_data)?;
        self.delay.delay_ns(Self::WRITE_DELAY_NS);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::mocks::{MockDelay, mock_i2c_with_eeprom};

    #[test]
    fn get_version_default() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_version().unwrap(), 0xffffffff);
    }

    #[test]
    fn set_and_get_version() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        eeprom.set_version(0x12345678).unwrap();
        assert_eq!(eeprom.get_version().unwrap(), 0x12345678);
    }

    #[test]
    fn set_and_get_version_zero() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        eeprom.set_version(0).unwrap();
        assert_eq!(eeprom.get_version().unwrap(), 0);
    }

    #[test]
    fn get_sframe_key_default() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        assert_eq!(eeprom.get_sframe_key().unwrap(), [0xff; 16]);
    }

    #[test]
    fn set_and_get_sframe_key() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        let key = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        eeprom.set_sframe_key(&key).unwrap();
        assert_eq!(eeprom.get_sframe_key().unwrap(), key);
    }

    #[test]
    fn set_and_get_sframe_key_zeros() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        let key = [0u8; 16];
        eeprom.set_sframe_key(&key).unwrap();
        assert_eq!(eeprom.get_sframe_key().unwrap(), key);
    }

    #[test]
    fn version_and_sframe_key_independent() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        eeprom.set_version(0xdeadbeef).unwrap();
        let key = [0xaa; 16];
        eeprom.set_sframe_key(&key).unwrap();
        assert_eq!(eeprom.get_version().unwrap(), 0xdeadbeef);
        assert_eq!(eeprom.get_sframe_key().unwrap(), key);
    }

    #[test]
    fn version_big_endian() {
        let mut i2c = mock_i2c_with_eeprom();
        let mut delay = MockDelay;
        let mut eeprom = Eeprom::new(&mut i2c, &mut delay);
        eeprom.set_version(0x01020304).unwrap();
        assert_eq!(eeprom.get_version().unwrap(), 0x01020304);
    }
}
