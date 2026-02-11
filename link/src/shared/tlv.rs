//! TLV (Type-Length-Value) encoding and decoding.

// Async TLV imports - for firmware modules and async-ctl
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
use embedded_io_async::{Read, ReadExactError, Write};
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
use heapless::Vec;

// Verbose TLV tracing - enable with `trace-tlv` feature, easy to disable
#[cfg(feature = "trace-tlv")]
macro_rules! trace {
    ($($arg:tt)*) => { defmt::debug!($($arg)*) };
}

#[cfg(all(
    not(feature = "trace-tlv"),
    any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl")
))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}

type Type = u16;
type Length = u32;

/// Value buffer for TLV messages.
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
pub type Value = Vec<u8, MAX_VALUE_SIZE>;

pub const HEADER_SIZE: usize = core::mem::size_of::<Type>() + core::mem::size_of::<Length>();
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
type Header = [u8; HEADER_SIZE];

pub const MAX_VALUE_SIZE: usize = 640;

/// Sync word prefix for guarded TLV communication.
/// Used to synchronize after bootloader garbage or other noise.
/// Spells "LINK" in ASCII.
pub const SYNC_WORD: [u8; 4] = [0x4C, 0x49, 0x4E, 0x4B];

// ============================================================================
// Universal header encoding/decoding (available to all modules)
// ============================================================================

/// Decode a TLV header into raw type and length.
///
/// This is the canonical header decoding function used by all modules.
fn decode_header_bytes(header: &[u8; HEADER_SIZE]) -> (u16, usize) {
    let raw_type = u16::from_be_bytes([header[0], header[1]]);
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]);
    (raw_type, length as usize)
}

/// Encode a TLV header from type and length.
///
/// This is the canonical header encoding function used by all modules.
fn encode_header_bytes(tlv_type: u16, length: usize) -> [u8; HEADER_SIZE] {
    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&tlv_type.to_be_bytes());
    header[2..6].copy_from_slice(&(length as u32).to_be_bytes());
    header
}

// Typed header decoding for async modules
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
fn decode_header<T: TryFrom<u16>>(header: &Header) -> Result<(T, usize), T::Error> {
    let (raw_type, length) = decode_header_bytes(header);
    let tlv_type = T::try_from(raw_type)?;
    Ok((tlv_type, length))
}

// Typed header encoding for async modules
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
fn encode_header(tlv_type: impl Into<u16>, length: usize) -> Header {
    encode_header_bytes(tlv_type.into(), length)
}

/// TLV message structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv<T> {
    pub tlv_type: T,
    // Use async Value type when any firmware feature is enabled
    #[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
    pub value: Value,
    // Use heapless::Vec directly when only ctl feature (no firmware features)
    #[cfg(not(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl")))]
    pub value: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

// Async TLV types and traits - for firmware modules
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
mod async_tlv {
    use super::*;

    #[cfg(test)]
    type TlvVec = Vec<u8, { HEADER_SIZE + MAX_VALUE_SIZE }>;

    #[cfg(test)]
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
    pub trait WriteTlv<T: Into<u16> + Copy> {
        type Error: core::fmt::Debug;

        async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), Self::Error>;

        /// Write a TLV from multiple data segments without concatenating them first.
        /// The total value length is the sum of all segment lengths.
        ///
        /// Default implementation concatenates parts into a temporary buffer.
        /// Implementations for actual writers should override to write parts directly.
        #[allow(dead_code)]
        async fn write_tlv_parts(
            &mut self,
            tlv_type: T,
            parts: &[&[u8]],
        ) -> Result<(), Self::Error> {
            // Default: concatenate into a buffer and call write_tlv
            let mut buf: Vec<u8, MAX_VALUE_SIZE> = Vec::new();
            for part in parts {
                let _ = buf.extend_from_slice(part);
            }
            self.write_tlv(tlv_type, &buf).await
        }

        async fn must_write_tlv(&mut self, tlv_type: T, value: &[u8]) {
            self.write_tlv(tlv_type, value).await.unwrap()
        }

        #[allow(dead_code)]
        async fn must_write_tlv_parts(&mut self, tlv_type: T, parts: &[&[u8]]) {
            self.write_tlv_parts(tlv_type, parts).await.unwrap()
        }
    }

    impl<T, W> WriteTlv<T> for W
    where
        T: Into<u16> + Copy,
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

        async fn write_tlv_parts(
            &mut self,
            tlv_type: T,
            parts: &[&[u8]],
        ) -> Result<(), W::Error> {
            let type_val: u16 = tlv_type.into();
            let total_len: usize = parts.iter().map(|p| p.len()).sum();
            trace!(
                "write sync + type={:#06x}, length={:#x} (parts={})",
                type_val,
                total_len,
                parts.len()
            );
            self.write_all(&SYNC_WORD).await?;
            let header = encode_header(type_val, total_len);
            self.write_all(&header).await?;
            for part in parts {
                self.write_all(part).await?;
            }
            self.flush().await?;
            trace!("write complete");
            Ok(())
        }
    }
}

#[cfg(any(feature = "mgmt", feature = "net", feature = "ui", feature = "async-ctl"))]
#[allow(unused_imports)] // Re-exported for public API, may not be used internally
pub use async_tlv::{ReadTlv, WriteTlv};

// ============================================================================
// Buffer-based parsing utilities (for ctl module)
// ============================================================================

#[cfg(any(feature = "std", feature = "alloc"))]
pub mod buffer {
    //! Pure functions for parsing TLVs from byte buffers.
    //!
    //! These utilities are used by the ctl module for stream demultiplexing,
    //! where TLV data may be fragmented across multiple transport messages.

    use super::*;

    /// Errors that can occur during buffer parsing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ParseError {
        /// Invalid TLV type.
        InvalidType(u16),
        /// TLV value length exceeds maximum.
        TooLong,
        /// Incomplete TLV (need more data).
        Incomplete,
        /// Invalid length field.
        InvalidLength,
    }

    /// Find the position of SYNC_WORD in a slice.
    ///
    /// Returns the index of the first occurrence, or None if not found.
    pub fn find_sync_word(data: &[u8]) -> Option<usize> {
        if data.len() < SYNC_WORD.len() {
            return None;
        }
        for i in 0..=data.len() - SYNC_WORD.len() {
            if data[i..i + SYNC_WORD.len()] == SYNC_WORD {
                return Some(i);
            }
        }
        None
    }

    /// Try to parse one complete TLV from buffer.
    ///
    /// On success, returns `Ok(Some((tlv, bytes_consumed)))` where `bytes_consumed`
    /// is the total length including sync word, header, and value.
    ///
    /// Returns `Ok(None)` if the buffer doesn't contain a complete TLV yet.
    ///
    /// Returns `Err(ParseError)` if parsing fails due to invalid data.
    ///
    /// The buffer is not modified - the caller is responsible for removing
    /// consumed bytes.
    pub fn try_parse_from_buffer<T: TryFrom<u16>>(
        buffer: &[u8],
    ) -> Result<Option<(Tlv<T>, usize)>, ParseError> {
        // Find sync word
        let sync_pos = match find_sync_word(buffer) {
            Some(pos) => pos,
            None => return Ok(None),
        };

        // Check if we have enough data for header
        let header_start = sync_pos + SYNC_WORD.len();
        if buffer.len() < header_start + HEADER_SIZE {
            return Ok(None); // Need more data
        }

        // Parse header using shared function
        let header: [u8; HEADER_SIZE] = buffer[header_start..header_start + HEADER_SIZE]
            .try_into()
            .unwrap();
        let (tlv_type_raw, length) = super::decode_header_bytes(&header);

        // Sanity check length
        if length > MAX_VALUE_SIZE {
            return Err(ParseError::TooLong);
        }

        // Check if we have the complete value
        let value_start = header_start + HEADER_SIZE;
        let total_len = sync_pos + SYNC_WORD.len() + HEADER_SIZE + length;
        if buffer.len() < total_len {
            return Ok(None); // Need more data
        }

        // Parse type
        let tlv_type = T::try_from(tlv_type_raw).map_err(|_| ParseError::InvalidType(tlv_type_raw))?;

        // Extract value
        let value_bytes = &buffer[value_start..value_start + length];
        let value = heapless::Vec::try_from(value_bytes)
            .map_err(|_| ParseError::TooLong)?;

        Ok(Some((Tlv { tlv_type, value }, total_len)))
    }

    /// Parse a complete TLV from buffer (for tunneled TLVs).
    ///
    /// This expects the buffer to contain a complete TLV starting with SYNC_WORD at position 0.
    /// Returns `Err(ParseError)` if the data is invalid or incomplete.
    ///
    /// This is a thin wrapper around `try_parse_from_buffer()` with stricter requirements.
    pub fn parse_complete<T: TryFrom<u16>>(data: &[u8]) -> Result<Tlv<T>, ParseError> {
        // Check minimum length
        if data.len() < SYNC_WORD.len() + HEADER_SIZE {
            return Err(ParseError::Incomplete);
        }

        // Verify sync word at position 0
        if &data[0..SYNC_WORD.len()] != SYNC_WORD {
            return Err(ParseError::Incomplete);
        }

        // Use try_parse_from_buffer (which will find sync at position 0)
        match try_parse_from_buffer(data)? {
            Some((tlv, _)) => Ok(tlv),
            None => Err(ParseError::Incomplete),
        }
    }
}

// ============================================================================
// Tunneling utilities (for ctl module)
// ============================================================================

#[cfg(feature = "std")]
pub mod tunnel {
    //! Utilities for encoding/decoding nested TLVs.
    //!
    //! Used for tunneling TLVs through wrapper TLVs (e.g., UI/NET through MGMT).

    use super::*;

    /// Encode a TLV as nested payload (sync_word + header + value).
    ///
    /// Returns a Vec containing the complete nested TLV ready to be used
    /// as the value field of a wrapper TLV.
    pub fn encode_nested<T: Into<u16>>(inner_type: T, inner_value: &[u8]) -> std::vec::Vec<u8> {
        let type_val: u16 = inner_type.into();
        let header = super::encode_header_bytes(type_val, inner_value.len());

        let mut result = std::vec::Vec::new();
        result.extend_from_slice(&SYNC_WORD);
        result.extend_from_slice(&header);
        result.extend_from_slice(inner_value);
        result
    }

    /// Decode nested TLV from wrapper value field.
    ///
    /// The `wrapper_value` should contain a complete TLV (sync_word + header + value).
    pub fn decode_nested<T: TryFrom<u16>>(
        wrapper_value: &[u8],
    ) -> Result<Tlv<T>, buffer::ParseError> {
        buffer::parse_complete(wrapper_value)
    }
}

#[cfg(test)]
mod test {
    use super::async_tlv::ReadError;
    use super::*;
    use crate::shared::protocol::*;
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

        let original_type = NetToCtl::Pong;
        let original_value = b"async roundtrip";

        // Write (includes sync word automatically via WriteTlv blanket impl)
        writer
            .write_tlv(original_type, original_value)
            .await
            .unwrap();

        // Read (scans for sync word automatically via ReadTlv blanket impl)
        let tlv: Tlv<NetToCtl> = reader.read_tlv().await.unwrap().unwrap();

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
        writer.write_tlv(UiToCtl::Pong, b"found me").await.unwrap();

        // Reader should skip garbage and find the TLV
        let tlv: Tlv<UiToCtl> = reader.read_tlv().await.unwrap().unwrap();

        assert_eq!(tlv.tlv_type, UiToCtl::Pong);
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
                0x00, 0x00, 0x10, 0x00, // length: 4096 (exceeds MAX_VALUE_SIZE=640)
            ])
            .await
            .unwrap();
        writer.flush().await.unwrap();

        let result: Result<Option<Tlv<CtlToMgmt>>, _> = reader.read_tlv().await;

        assert!(matches!(result, Err(ReadError::TooLong)));
    }

    #[test]
    fn buffer_find_sync_word() {
        use buffer::find_sync_word;

        // Sync word at beginning
        let data = [0x4C, 0x49, 0x4E, 0x4B, 0x00, 0x01];
        assert_eq!(find_sync_word(&data), Some(0));

        // Sync word in middle
        let data = [0x00, 0x01, 0x4C, 0x49, 0x4E, 0x4B, 0x02];
        assert_eq!(find_sync_word(&data), Some(2));

        // No sync word
        let data = [0x00, 0x01, 0x02, 0x03];
        assert_eq!(find_sync_word(&data), None);

        // Partial sync word
        let data = [0x4C, 0x49, 0x4E];
        assert_eq!(find_sync_word(&data), None);

        // Empty buffer
        let data: [u8; 0] = [];
        assert_eq!(find_sync_word(&data), None);
    }

    #[test]
    fn buffer_try_parse_complete_tlv() {
        use buffer::try_parse_from_buffer;

        // Complete TLV
        let mut data = Vec::new();
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0x00, 0x00]); // type: CtlToMgmt::Ping
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // length: 2
        data.extend_from_slice(b"hi");

        let result = try_parse_from_buffer::<CtlToMgmt>(&data).unwrap();
        assert!(result.is_some());
        let (tlv, consumed) = result.unwrap();
        assert_eq!(tlv.tlv_type, CtlToMgmt::Ping);
        assert_eq!(tlv.value.as_slice(), b"hi");
        assert_eq!(consumed, data.len());

        // Incomplete TLV (missing value)
        let mut data = Vec::new();
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0x00, 0x00]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // length: 2
        data.push(b'h'); // Only 1 byte of value

        let result = try_parse_from_buffer::<CtlToMgmt>(&data).unwrap();
        assert!(result.is_none());

        // Invalid type
        let mut data = Vec::new();
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0xFF, 0xFF]); // invalid type
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.push(0x42);

        let result = try_parse_from_buffer::<CtlToMgmt>(&data);
        assert!(matches!(result, Err(buffer::ParseError::InvalidType(0xFFFF))));

        // TLV with garbage prefix
        let mut data = Vec::new();
        data.extend_from_slice(b"garbage");
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0x00, 0x00]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        data.extend_from_slice(b"hi");

        let result = try_parse_from_buffer::<CtlToMgmt>(&data).unwrap();
        assert!(result.is_some());
        let (tlv, consumed) = result.unwrap();
        assert_eq!(consumed, data.len()); // Should consume garbage too
    }

    #[test]
    fn buffer_parse_complete() {
        use buffer::parse_complete;

        // Valid complete TLV
        let mut data = Vec::new();
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0x00, 0x00]); // CtlToMgmt::Ping
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]);
        data.extend_from_slice(b"foo");

        let tlv = parse_complete::<CtlToMgmt>(&data).unwrap();
        assert_eq!(tlv.tlv_type, CtlToMgmt::Ping);
        assert_eq!(tlv.value.as_slice(), b"foo");

        // Missing sync word
        let data = [0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x42];
        let result = parse_complete::<CtlToMgmt>(&data);
        assert!(matches!(result, Err(buffer::ParseError::Incomplete)));

        // Incomplete data
        let mut data = Vec::new();
        data.extend_from_slice(&SYNC_WORD);
        data.extend_from_slice(&[0x00, 0x00]);
        // Missing length and value

        let result = parse_complete::<CtlToMgmt>(&data);
        assert!(matches!(result, Err(buffer::ParseError::Incomplete)));
    }

    #[test]
    fn tunnel_encode_decode_roundtrip() {
        use tunnel::{decode_nested, encode_nested};

        let original_type = CtlToUi::Ping;
        let original_value = b"test data";

        // Encode
        let nested = encode_nested(original_type, original_value);

        // Verify structure
        assert_eq!(&nested[0..4], &SYNC_WORD);

        // Decode
        let decoded = decode_nested::<CtlToUi>(&nested).unwrap();
        assert_eq!(decoded.tlv_type, original_type);
        assert_eq!(decoded.value.as_slice(), original_value);
    }

    #[test]
    fn tunnel_encode_empty_value() {
        use tunnel::encode_nested;

        let nested = encode_nested(CtlToNet::GetLoopback, &[]);

        // Should be: sync_word(4) + type(2) + length(4) = 10 bytes
        assert_eq!(nested.len(), 10);
        assert_eq!(&nested[0..4], &SYNC_WORD);
        assert_eq!(&nested[6..10], &[0x00, 0x00, 0x00, 0x00]); // length = 0
    }
}
