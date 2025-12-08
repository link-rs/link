use embedded_io_async::{Read, ReadExactError, Write};
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive};

// Verbose TLV tracing - enable with `trace-tlv` feature, easy to disable
#[cfg(feature = "trace-tlv")]
macro_rules! trace {
    ($($arg:tt)*) => { defmt::debug!($($arg)*) };
}

#[cfg(not(feature = "trace-tlv"))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}

pub type Type = u16;
pub type Length = u32;
pub type Value = Vec<u8, MAX_VALUE_SIZE>;

pub const HEADER_SIZE: usize = core::mem::size_of::<Type>() + core::mem::size_of::<Length>();
pub type Header = [u8; HEADER_SIZE];

pub const MAX_VALUE_SIZE: usize = 32;
pub const MAX_TLV_SIZE: usize = HEADER_SIZE + MAX_VALUE_SIZE;
pub type TlvVec = Vec<u8, MAX_TLV_SIZE>;

/// Sync word prefix for guarded TLV communication.
/// Used to synchronize after bootloader garbage or other noise.
/// Spells "LINK" in ASCII.
pub const SYNC_WORD: [u8; 4] = [0x4C, 0x49, 0x4E, 0x4B];

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToMgmt {
    Ping = 0x00,
    ToUi,
    ToNet,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToCtl {
    Pong = 0x10,
    FromUi,
    FromNet,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToUi {
    Ping = 0x20,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToMgmt {
    Pong = 0x30,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToNet {
    Ping = 0x40,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToMgmt {
    Pong = 0x50,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToUi {
    CircularPing = 0x70,
}

fn decode_header<T: TryFrom<u16>>(header: &Header) -> Result<(T, usize), T::Error> {
    let raw_type = Type::from_be_bytes([header[0], header[1]]);
    let tlv_type = T::try_from(raw_type)?;
    let length = Length::from_be_bytes([header[2], header[3], header[4], header[5]]);
    Ok((tlv_type, length as usize))
}

fn encode_header(tlv_type: impl Into<u16>, length: usize) -> Header {
    let mut header = Header::default();
    let type_val: Type = tlv_type.into();
    let length_val: Length = length as Length;
    header[0..2].copy_from_slice(&type_val.to_be_bytes());
    header[2..6].copy_from_slice(&length_val.to_be_bytes());
    header
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv<T> {
    pub tlv_type: T,
    pub value: Value,
}

impl<T> Tlv<T> {
    /// Encode a TLV to bytes (header + value, no sync word).
    pub fn encode(tlv_type: T, value: &[u8]) -> TlvVec
    where
        T: Into<u16>,
    {
        let mut enc = TlvVec::new();
        let header = encode_header(tlv_type, value.len());
        enc.extend_from_slice(&header).unwrap();
        enc.extend_from_slice(value).unwrap();
        enc
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ReadError<E> {
    Io(E),
    InvalidType,
    TooLong,
}

#[allow(async_fn_in_trait)]
pub trait ReadTlv<T: TryFrom<u16>> {
    type Error: core::fmt::Debug;

    async fn read_tlv(&mut self) -> Result<Option<Tlv<T>>, Self::Error>;

    async fn must_read_tlv(&mut self) -> Tlv<T> {
        self.read_tlv().await.unwrap().unwrap()
    }
}

impl<T, R> ReadTlv<T> for R
where
    T: TryFrom<u16>,
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        trace!("scanning for sync word");

        // Scan for sync word, draining any garbage
        let mut matched = 0usize;
        while matched < SYNC_WORD.len() {
            let mut byte = [0u8; 1];
            let Ok(1) = self.read(&mut byte).await else {
                continue;
            };

            if byte[0] == SYNC_WORD[matched] {
                matched += 1;
            } else {
                matched = 0;
                if byte[0] == SYNC_WORD[0] {
                    matched = 1;
                }
            }
        }

        // Sync word found, now read the header
        trace!("reading header");

        let mut header = [0u8; HEADER_SIZE];
        match self.read_exact(&mut header).await {
            Ok(()) => {
                trace!("header = {=[u8]:02x}", header);
            }
            Err(ReadExactError::UnexpectedEof) => {
                trace!("unexpected EOF at header");
                return Ok(None);
            }
            Err(ReadExactError::Other(e)) => {
                trace!("IO error at header");
                return Err(ReadError::Io(e));
            }
        }

        let Ok((tlv_type, length)) = decode_header(&header) else {
            trace!("invalid type in header");
            return Err(ReadError::InvalidType);
        };

        trace!(
            "type={:#06x}, length={:#x}",
            u16::from_be_bytes([header[0], header[1]]),
            length
        );

        let mut value = Value::new();
        if value.resize(length, 0).is_err() {
            trace!("value too long ({:#x})", length);
            return Err(ReadError::TooLong);
        }

        match self.read_exact(&mut value).await {
            Ok(()) => {
                trace!("value = {=[u8]:02x}", value.as_slice());
            }
            Err(ReadExactError::UnexpectedEof) => {
                trace!("unexpected EOF at value");
                return Ok(None);
            }
            Err(ReadExactError::Other(e)) => {
                trace!("IO error at value");
                return Err(ReadError::Io(e));
            }
        }

        trace!("complete, value={=[u8]:02x}", value.as_slice());

        Ok(Some(Tlv { tlv_type, value }))
    }
}

#[allow(async_fn_in_trait)]
pub trait WriteTlv<T: Into<u16>> {
    type Error: core::fmt::Debug;

    async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), Self::Error>;

    async fn must_write_tlv(&mut self, tlv_type: T, value: &[u8]) {
        self.write_tlv(tlv_type, value).await.unwrap()
    }
}

impl<T, W> WriteTlv<T> for W
where
    T: Into<u16>,
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), W::Error> {
        let type_val: u16 = tlv_type.into();
        trace!(
            "write sync + type={:#06x}, length={:#x}",
            type_val,
            value.len()
        );
        self.write_all(&SYNC_WORD).await?;
        let header = encode_header(type_val, value.len());
        self.write_all(&header).await?;
        self.write_all(&value).await?;
        self.flush().await?;
        trace!("write complete");
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use embedded_io_adapters::futures_03::FromFutures;

    #[tokio::test]
    async fn encode_tlv_known_answer() {
        // CtlToMgmt::Ping = 0x0000, value = "hi" (2 bytes)
        // Expected: type=0x0000, length=0x00000002, value=0x68 0x69
        let encoded = Tlv::encode(CtlToMgmt::Ping, b"hi");

        let expected = [
            0x00, 0x00, // type: CtlToMgmt::Ping = 0x0000
            0x00, 0x00, 0x00, 0x02, // length: 2
            0x68, 0x69, // value: "hi"
        ];
        assert_eq!(encoded.as_slice(), &expected);
    }

    #[tokio::test]
    async fn roundtrip_async_reader_writer() {
        // Use async_ringbuffer for a realistic async read/write scenario
        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let mut reader = FromFutures::new(reader);

        let original_type = NetToMgmt::Pong;
        let original_value = b"async roundtrip";

        // Write (includes sync word automatically via WriteTlv blanket impl)
        writer
            .write_tlv(original_type, original_value)
            .await
            .unwrap();

        // Read (scans for sync word automatically via ReadTlv blanket impl)
        let tlv: Tlv<NetToMgmt> = reader.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, original_type);
        assert_eq!(tlv.value.as_slice(), original_value);
    }

    #[tokio::test]
    async fn roundtrip_with_garbage_prefix() {
        // Test that reader can sync after garbage bytes
        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let mut reader = FromFutures::new(reader);

        // Write garbage first
        use embedded_io_async::Write;
        writer.write_all(b"garbage data here").await.unwrap();
        writer.flush().await.unwrap();

        // Now write a valid TLV with sync word (via WriteTlv blanket impl)
        writer.write_tlv(UiToMgmt::Pong, b"found me").await.unwrap();

        // Reader should skip garbage and find the TLV
        let tlv: Tlv<UiToMgmt> = reader.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
        assert_eq!(tlv.value.as_slice(), b"found me");
    }

    #[tokio::test]
    async fn async_reader_invalid_type() {
        use embedded_io_async::Write;

        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let mut reader = FromFutures::new(reader);

        // Write raw bytes directly: sync word + invalid header
        writer.write_all(&SYNC_WORD).await.unwrap();
        writer
            .write_all(&[
                0xFF, 0xFF, // invalid type
                0x00, 0x00, 0x00, 0x01, // length: 1
                0x42, // value
            ])
            .await
            .unwrap();
        writer.flush().await.unwrap();

        let result: Result<Option<Tlv<CtlToMgmt>>, _> = reader.read_tlv().await;

        assert!(matches!(result, Err(ReadError::InvalidType)));
    }

    #[tokio::test]
    async fn async_reader_length_exceeds_max() {
        use embedded_io_async::Write;

        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let mut reader = FromFutures::new(reader);

        // Write raw bytes directly: sync word + header with excessive length
        writer.write_all(&SYNC_WORD).await.unwrap();
        writer
            .write_all(&[
                0x00, 0x00, // type: CtlToMgmt::Ping
                0x00, 0x00, 0x01, 0x00, // length: 256 (exceeds MAX_VALUE_SIZE=32)
            ])
            .await
            .unwrap();
        writer.flush().await.unwrap();

        let result: Result<Option<Tlv<CtlToMgmt>>, _> = reader.read_tlv().await;

        assert!(matches!(result, Err(ReadError::TooLong)));
    }
}
