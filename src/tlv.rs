use embedded_io_async::{Read, ReadExactError, Write};
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};

const HEADER_SIZE: usize = 6;
pub const MAX_VALUE_SIZE: usize = 512;

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum Type {
    Ping,
    Pong,
}

pub type Length = u32;
pub type Value = Vec<u8, MAX_VALUE_SIZE>;

fn decode_header(header: &[u8; HEADER_SIZE]) -> Result<(Type, usize), TryFromPrimitiveError<Type>> {
    let tlv_type = Type::try_from(u16::from_be_bytes([header[0], header[1]]))?;
    let length = Length::from_be_bytes([header[2], header[3], header[4], header[5]]);
    Ok((tlv_type, length as usize))
}

fn encode_header(tlv_type: Type, length: usize) -> [u8; HEADER_SIZE] {
    let mut header = [0u8; HEADER_SIZE];
    let type_val: u16 = tlv_type.into();
    let length_val: Length = length as Length;
    header[0..2].copy_from_slice(&type_val.to_be_bytes());
    header[2..6].copy_from_slice(&length_val.to_be_bytes());
    header
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv {
    pub tlv_type: Type,
    pub value: Value,
}

impl Tlv {
    pub const fn new(tlv_type: Type, value: Value) -> Self {
        Self { tlv_type, value }
    }
}

#[derive(Debug)]
pub enum ReadError<E> {
    Io(E),
    InvalidType(TryFromPrimitiveError<Type>),
    TooLong(usize),
}

pub trait ReadTlv {
    type Error;
    async fn read_tlv(&mut self) -> Result<Option<Tlv>, Self::Error>;
}

impl<R> ReadTlv for R
where
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv(&mut self) -> Result<Option<Tlv>, Self::Error> {
        let mut header = [0u8; HEADER_SIZE];
        match self.read_exact(&mut header).await {
            Ok(()) => {}
            Err(ReadExactError::UnexpectedEof) => return Ok(None),
            Err(ReadExactError::Other(e)) => return Err(ReadError::Io(e)),
        }

        let (tlv_type, length) = decode_header(&header).map_err(ReadError::InvalidType)?;

        let mut value = Value::new();
        value
            .resize(length, 0)
            .map_err(|_| ReadError::TooLong(length))?;

        match self.read_exact(&mut value).await {
            Ok(()) => {}
            Err(ReadExactError::UnexpectedEof) => return Ok(None),
            Err(ReadExactError::Other(e)) => return Err(ReadError::Io(e)),
        }

        Ok(Some(Tlv { tlv_type, value }))
    }
}

pub trait WriteTlv {
    type Error;
    async fn write_tlv(&mut self, tlv_type: Type, value: &[u8]) -> Result<(), Self::Error>;
}

impl<W> WriteTlv for W
where
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv(&mut self, tlv_type: Type, value: &[u8]) -> Result<(), W::Error> {
        let header = encode_header(tlv_type, value.len());
        self.write_all(&header).await?;
        self.write_all(&value).await?;
        Ok(())
    }
}
