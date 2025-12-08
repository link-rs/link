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

// Trace IO errors with the error value when trace-tlv is enabled
#[cfg(feature = "trace-tlv")]
macro_rules! trace_io_error {
    ($label:expr, $msg:expr, $err:expr) => {
        defmt::debug!("{}: {} {:?}", $label, $msg, defmt::Debug2Format(&$err))
    };
}

#[cfg(not(feature = "trace-tlv"))]
macro_rules! trace_io_error {
    ($label:expr, $msg:expr, $err:expr) => {
        let _ = &$err; // suppress unused warning
    };
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
    pub async fn encode(tlv_type: T, value: &[u8]) -> TlvVec
    where
        T: Into<u16>,
    {
        let mut enc = TlvVec::new();
        enc.resize(HEADER_SIZE + value.len(), 0).unwrap();
        enc.as_mut_slice().write_tlv(tlv_type, value).await.unwrap();
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

/// A labeled TLV reader that includes a label in all trace output.
pub struct LabeledReader<R> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    reader: R,
}

impl<R> LabeledReader<R> {
    pub fn new(label: &'static str, reader: R) -> Self {
        Self { label, reader }
    }
}

impl<R> embedded_io_async::ErrorType for LabeledReader<R>
where
    R: Read,
{
    type Error = R::Error;
}

impl<R> Read for LabeledReader<R>
where
    R: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.reader.read(buf).await
    }
}

impl<T, R> ReadTlv<T> for LabeledReader<R>
where
    T: TryFrom<u16>,
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        trace!("{}: scanning for sync word", self.label);

        // Scan for sync word, draining any garbage
        let mut matched = 0usize;
        let mut discarded = 0usize;
        loop {
            let mut byte = [0u8; 1];
            match self.reader.read(&mut byte).await {
                Ok(0) => {
                    // No data yet - yield to let other tasks run (important for tests)
                    #[cfg(test)]
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(_) => {
                    if byte[0] == SYNC_WORD[matched] {
                        matched += 1;
                        if matched == SYNC_WORD.len() {
                            if discarded > 0 {
                                trace!(
                                    "{}: sync found after discarding {} bytes",
                                    self.label,
                                    discarded
                                );
                            } else {
                                trace!("{}: sync found", self.label);
                            }
                            break;
                        }
                    } else {
                        discarded += matched + 1;
                        matched = 0;
                        if byte[0] == SYNC_WORD[0] {
                            matched = 1;
                            discarded -= 1;
                        }
                    }
                }
                Err(e) => {
                    // IO errors like Overrun can happen with garbage. Continue scanning.
                    trace_io_error!(self.label, "IO error during sync scan (continuing)", e);
                    discarded += 1;
                    matched = 0;
                    continue;
                }
            }
        }

        // Sync word found, now read the header
        trace!("{}: reading header ({} bytes)", self.label, HEADER_SIZE);

        let mut header = [0u8; HEADER_SIZE];
        for i in 0..HEADER_SIZE {
            match self.reader.read_exact(&mut header[i..i + 1]).await {
                Ok(()) => {
                    trace!("{}: header[{}] = {:#04x}", self.label, i, header[i]);
                }
                Err(ReadExactError::UnexpectedEof) => {
                    trace!("{}: unexpected EOF at header[{}]", self.label, i);
                    return Ok(None);
                }
                Err(ReadExactError::Other(e)) => {
                    trace_io_error!(self.label, "IO error at header", e);
                    return Err(ReadError::Io(e));
                }
            }
        }

        trace!("{}: header complete: {=[u8]:02x}", self.label, header);

        let (tlv_type, length) = match decode_header(&header) {
            Ok(v) => v,
            Err(_) => {
                trace!("{}: invalid type in header", self.label);
                return Err(ReadError::InvalidType);
            }
        };

        trace!(
            "{}: type={:#06x}, length={:#x}",
            self.label,
            u16::from_be_bytes([header[0], header[1]]),
            length
        );

        let mut value = Value::new();
        if value.resize(length, 0).is_err() {
            trace!("{}: value too long ({:#x})", self.label, length);
            return Err(ReadError::TooLong);
        }

        for i in 0..length {
            match self.reader.read_exact(&mut value[i..i + 1]).await {
                Ok(()) => {
                    trace!("{}: value[{}] = {:#04x}", self.label, i, value[i]);
                }
                Err(ReadExactError::UnexpectedEof) => {
                    trace!("{}: unexpected EOF at value[{}]", self.label, i);
                    return Ok(None);
                }
                Err(ReadExactError::Other(e)) => {
                    trace_io_error!(self.label, "IO error at value", e);
                    return Err(ReadError::Io(e));
                }
            }
        }

        trace!(
            "{}: complete, value={=[u8]:02x}",
            self.label,
            value.as_slice()
        );

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

/// A labeled TLV writer that includes a label in all trace output.
/// Automatically sends sync word prefix at the start of each write sequence.
pub struct LabeledWriter<W> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    writer: W,
    /// Track if sync word needs to be sent (reset after flush)
    needs_sync: bool,
}

impl<W> LabeledWriter<W> {
    pub fn new(label: &'static str, writer: W) -> Self {
        Self {
            label,
            writer,
            needs_sync: true,
        }
    }
}

impl<W> embedded_io_async::ErrorType for LabeledWriter<W>
where
    W: Write,
{
    type Error = W::Error;
}

impl<W> Write for LabeledWriter<W>
where
    W: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // Send sync word at start of write sequence
        if self.needs_sync {
            trace!("{}: sending sync word", self.label);
            self.writer.write_all(&SYNC_WORD).await?;
            self.needs_sync = false;
        }
        self.writer.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let result = self.writer.flush().await;
        self.needs_sync = true; // Next write sequence needs sync word
        result
    }
}

impl<T, W> WriteTlv<T> for LabeledWriter<W>
where
    T: Into<u16>,
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), W::Error> {
        let type_val: u16 = tlv_type.into();
        trace!(
            "{}: write sync + type={:#06x}, length={:#x}",
            self.label,
            type_val,
            value.len()
        );
        // Send sync word prefix (if not already sent in this sequence)
        if self.needs_sync {
            self.writer.write_all(&SYNC_WORD).await?;
            self.needs_sync = false;
        }
        let header = encode_header(type_val, value.len());
        self.writer.write_all(&header).await?;
        self.writer.write_all(&value).await?;
        self.writer.flush().await?;
        self.needs_sync = true; // Reset for next write sequence
        trace!("{}: write complete", self.label);
        Ok(())
    }
}

/// WriteTlv impl for byte slices - used internally for encoding TLVs to buffers.
/// No logging since this is just in-memory encoding.
impl<T: Into<u16>> WriteTlv<T> for &mut [u8] {
    type Error = core::convert::Infallible;

    async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), Self::Error> {
        let header = encode_header(tlv_type, value.len());
        // Write header
        let header_len = header.len();
        self[..header_len].copy_from_slice(&header);
        *self = &mut core::mem::take(self)[header_len..];
        // Write value
        let value_len = value.len();
        self[..value_len].copy_from_slice(value);
        *self = &mut core::mem::take(self)[value_len..];
        Ok(())
    }
}

/// ReadTlv impl for byte slices - used internally for decoding TLVs from buffers.
/// No logging since this is just in-memory decoding.
impl<T: TryFrom<u16>> ReadTlv<T> for &[u8] {
    type Error = ReadError<core::convert::Infallible>;

    async fn read_tlv(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        if self.len() < HEADER_SIZE {
            return Ok(None);
        }

        let mut header = Header::default();
        header.copy_from_slice(&self[..HEADER_SIZE]);
        *self = &self[HEADER_SIZE..];

        let (tlv_type, length) = match decode_header(&header) {
            Ok(v) => v,
            Err(_) => return Err(ReadError::InvalidType),
        };

        if self.len() < length {
            return Err(ReadError::TooLong);
        }

        let mut value = Value::new();
        if value.resize(length, 0).is_err() {
            return Err(ReadError::TooLong);
        }
        value.copy_from_slice(&self[..length]);
        *self = &self[length..];

        Ok(Some(Tlv { tlv_type, value }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use embedded_io_adapters::futures_03::FromFutures;

    // ==================== (a) Known-answer tests ====================

    #[tokio::test]
    async fn write_tlv_known_answer() {
        // CtlToMgmt::Ping = 0x0000, value = "hi" (2 bytes)
        // Expected: type=0x0000, length=0x00000002, value=0x68 0x69
        let mut buf = [0u8; 32];
        let mut slice = buf.as_mut_slice();
        slice.write_tlv(CtlToMgmt::Ping, b"hi").await.unwrap();

        let expected = [
            0x00, 0x00, // type: CtlToMgmt::Ping = 0x0000
            0x00, 0x00, 0x00, 0x02, // length: 2
            0x68, 0x69, // value: "hi"
        ];
        assert_eq!(&buf[..8], &expected);
    }

    #[tokio::test]
    async fn read_tlv_known_answer() {
        // MgmtToCtl::Pong = 0x0010, value = [0xAB, 0xCD]
        let data: &[u8] = &[
            0x00, 0x10, // type: MgmtToCtl::Pong = 0x0010
            0x00, 0x00, 0x00, 0x02, // length: 2
            0xAB, 0xCD, // value
        ];
        let mut slice = data;
        let tlv: Tlv<MgmtToCtl> = slice.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
        assert_eq!(tlv.value.as_slice(), &[0xAB, 0xCD]);
    }

    // ==================== (b) Round-trip tests ====================

    #[tokio::test]
    async fn roundtrip_byte_slice() {
        let original_type = MgmtToUi::CircularPing;
        let original_value = b"round trip test";

        // Write
        let mut buf = [0u8; 64];
        let mut write_slice = buf.as_mut_slice();
        write_slice
            .write_tlv(original_type, original_value)
            .await
            .unwrap();

        // Read back
        let read_slice: &[u8] = &buf[..HEADER_SIZE + original_value.len()];
        let mut read_slice = read_slice;
        let tlv: Tlv<MgmtToUi> = read_slice.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, original_type);
        assert_eq!(tlv.value.as_slice(), original_value);
    }

    #[tokio::test]
    async fn roundtrip_labeled_reader_writer() {
        // Use async_ringbuffer for a realistic async read/write scenario
        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let writer = FromFutures::new(writer);
        let reader = FromFutures::new(reader);

        let mut labeled_writer = LabeledWriter::new("test-writer", writer);
        let mut labeled_reader = LabeledReader::new("test-reader", reader);

        let original_type = NetToMgmt::Pong;
        let original_value = b"async roundtrip";

        // Write (includes sync word automatically)
        labeled_writer
            .write_tlv(original_type, original_value)
            .await
            .unwrap();

        // Read (scans for sync word automatically)
        let tlv: Tlv<NetToMgmt> = labeled_reader.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, original_type);
        assert_eq!(tlv.value.as_slice(), original_value);
    }

    #[tokio::test]
    async fn roundtrip_with_garbage_prefix() {
        // Test that reader can sync after garbage bytes
        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let reader = FromFutures::new(reader);

        // Write garbage first
        use embedded_io_async::Write;
        writer.write_all(b"garbage data here").await.unwrap();
        writer.flush().await.unwrap();

        // Now write a valid TLV with sync word
        let mut labeled_writer = LabeledWriter::new("test-writer", writer);
        labeled_writer
            .write_tlv(UiToMgmt::Pong, b"found me")
            .await
            .unwrap();

        // Reader should skip garbage and find the TLV
        let mut labeled_reader = LabeledReader::new("test-reader", reader);
        let tlv: Tlv<UiToMgmt> = labeled_reader.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
        assert_eq!(tlv.value.as_slice(), b"found me");
    }

    // ==================== (c) Error condition tests ====================

    #[tokio::test]
    async fn read_error_invalid_type() {
        // Type 0xFF00 is not a valid CtlToMgmt variant
        let data: &[u8] = &[
            0xFF, 0x00, // type: invalid
            0x00, 0x00, 0x00, 0x01, // length: 1
            0x42, // value
        ];
        let mut slice = data;
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = slice.read_tlv().await;

        assert!(matches!(result, Err(ReadError::InvalidType)));
    }

    #[tokio::test]
    async fn read_error_length_exceeds_max() {
        // Length exceeds MAX_VALUE_SIZE (32)
        let data: &[u8] = &[
            0x00, 0x00, // type: CtlToMgmt::Ping
            0x00, 0x00, 0x00,
            0x64, // length: 100 (exceeds MAX_VALUE_SIZE=32)
                  // no value bytes needed - error happens before reading value
        ];
        let mut slice = data;
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = slice.read_tlv().await;

        assert!(matches!(result, Err(ReadError::TooLong)));
    }

    #[tokio::test]
    async fn read_error_length_exceeds_available() {
        // Length says 10 bytes but only 3 are available
        let data: &[u8] = &[
            0x00, 0x00, // type: CtlToMgmt::Ping
            0x00, 0x00, 0x00, 0x0A, // length: 10
            0x01, 0x02, 0x03, // only 3 bytes of value
        ];
        let mut slice = data;
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = slice.read_tlv().await;

        assert!(matches!(result, Err(ReadError::TooLong)));
    }

    #[tokio::test]
    async fn read_returns_none_for_short_buffer() {
        // Buffer too short to contain header
        let data: &[u8] = &[0x00, 0x01, 0x02]; // only 3 bytes, need 6 for header
        let mut slice = data;
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = slice.read_tlv().await;

        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn labeled_reader_invalid_type() {
        use embedded_io_async::Write;

        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let reader = FromFutures::new(reader);

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

        let mut labeled_reader = LabeledReader::new("test-reader", reader);
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = labeled_reader.read_tlv().await;

        assert!(matches!(result, Err(ReadError::InvalidType)));
    }

    #[tokio::test]
    async fn labeled_reader_length_exceeds_max() {
        use embedded_io_async::Write;

        let (writer, reader) = async_ringbuffer::ring_buffer(256);
        let mut writer = FromFutures::new(writer);
        let reader = FromFutures::new(reader);

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

        let mut labeled_reader = LabeledReader::new("test-reader", reader);
        let result: Result<Option<Tlv<CtlToMgmt>>, _> = labeled_reader.read_tlv().await;

        assert!(matches!(result, Err(ReadError::TooLong)));
    }
}
