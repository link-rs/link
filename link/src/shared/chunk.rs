//! Chunk format for hactar-compatible message encoding.
//!
//! The chunk format wraps audio data with metadata before SFrame encryption.
//! This matches the format defined in hactar firmware's ui_net_link.hh.

// These will be used in Phase 2 when UI/NET adopt the chunk format
#![allow(dead_code)]

use super::protocol::MessageType;
use heapless::Vec;

/// Audio chunk header size for Media type: type(1) + last_chunk(1) + chunk_length(4)
pub const MEDIA_HEADER_SIZE: usize = 6;

/// Audio chunk header size for AIRequest type: type(1) + request_id(4) + last_chunk(1) + chunk_length(4)
pub const AI_REQUEST_HEADER_SIZE: usize = 10;

/// Audio chunk header size for AIResponse type: type(1) + request_id(4) + content_type(1) + last_chunk(1) + chunk_length(4)
pub const AI_RESPONSE_HEADER_SIZE: usize = 11;

/// Serialize a media chunk (for Ptt channel).
///
/// Returns the number of bytes written to `out`.
pub fn serialize_media_chunk(audio_data: &[u8], last_chunk: bool, out: &mut [u8]) -> usize {
    let audio_len = audio_data.len();
    out[0] = MessageType::Media as u8;
    out[1] = last_chunk as u8;
    out[2..6].copy_from_slice(&(audio_len as u32).to_le_bytes());
    out[6..6 + audio_len].copy_from_slice(audio_data);
    6 + audio_len
}

/// Serialize an AI request chunk (for PttAi channel).
///
/// Returns the number of bytes written to `out`.
pub fn serialize_ai_request_chunk(
    audio_data: &[u8],
    request_id: u32,
    last_chunk: bool,
    out: &mut [u8],
) -> usize {
    let audio_len = audio_data.len();
    out[0] = MessageType::AiRequest as u8;
    out[1..5].copy_from_slice(&request_id.to_le_bytes());
    out[5] = last_chunk as u8;
    out[6..10].copy_from_slice(&(audio_len as u32).to_le_bytes());
    out[10..10 + audio_len].copy_from_slice(audio_data);
    10 + audio_len
}

/// Prepend a media chunk header to a buffer containing audio data.
///
/// The buffer should already contain the audio data. This function shifts the
/// data to make room for the header and writes the header at the beginning.
/// Returns Ok(()) on success, or Err(()) if the buffer capacity is insufficient.
pub fn prepend_media_header<const N: usize>(
    buf: &mut Vec<u8, N>,
    last_chunk: bool,
) -> Result<(), ()> {
    let audio_len = buf.len();
    let new_len = MEDIA_HEADER_SIZE + audio_len;

    // Ensure capacity
    if new_len > N {
        return Err(());
    }

    // Extend buffer to new size
    buf.resize(new_len, 0).map_err(|_| ())?;

    // Shift audio data to make room for header
    buf.copy_within(0..audio_len, MEDIA_HEADER_SIZE);

    // Write header
    buf[0] = MessageType::Media as u8;
    buf[1] = last_chunk as u8;
    buf[2..6].copy_from_slice(&(audio_len as u32).to_le_bytes());

    Ok(())
}

/// Prepend an AI request chunk header to a buffer containing audio data.
///
/// The buffer should already contain the audio data. This function shifts the
/// data to make room for the header and writes the header at the beginning.
/// Returns Ok(()) on success, or Err(()) if the buffer capacity is insufficient.
pub fn prepend_ai_request_header<const N: usize>(
    buf: &mut Vec<u8, N>,
    request_id: u32,
    last_chunk: bool,
) -> Result<(), ()> {
    let audio_len = buf.len();
    let new_len = AI_REQUEST_HEADER_SIZE + audio_len;

    // Ensure capacity
    if new_len > N {
        return Err(());
    }

    // Extend buffer to new size
    buf.resize(new_len, 0).map_err(|_| ())?;

    // Shift audio data to make room for header
    buf.copy_within(0..audio_len, AI_REQUEST_HEADER_SIZE);

    // Write header
    buf[0] = MessageType::AiRequest as u8;
    buf[1..5].copy_from_slice(&request_id.to_le_bytes());
    buf[5] = last_chunk as u8;
    buf[6..10].copy_from_slice(&(audio_len as u32).to_le_bytes());

    Ok(())
}

/// Parsed chunk information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChunk {
    /// The message type
    pub message_type: MessageType,
    /// Whether this is the last chunk in a sequence
    pub last_chunk: bool,
    /// Request ID (only valid for AiRequest/AiResponse)
    pub request_id: u32,
    /// Offset to audio data within the original buffer
    pub audio_offset: usize,
    /// Length of audio data
    pub audio_length: usize,
}

/// Parse a received chunk.
///
/// Returns parsed chunk information, or None if the data is invalid.
pub fn parse_chunk(data: &[u8]) -> Option<ParsedChunk> {
    if data.is_empty() {
        return None;
    }

    let msg_type = MessageType::try_from(data[0]).ok()?;

    match msg_type {
        MessageType::Media => {
            if data.len() < MEDIA_HEADER_SIZE {
                return None;
            }
            let last_chunk = data[1] != 0;
            let chunk_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
            if data.len() < MEDIA_HEADER_SIZE + chunk_len {
                return None;
            }
            Some(ParsedChunk {
                message_type: msg_type,
                last_chunk,
                request_id: 0,
                audio_offset: MEDIA_HEADER_SIZE,
                audio_length: chunk_len,
            })
        }
        MessageType::AiRequest => {
            if data.len() < AI_REQUEST_HEADER_SIZE {
                return None;
            }
            let request_id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            let last_chunk = data[5] != 0;
            let chunk_len = u32::from_le_bytes([data[6], data[7], data[8], data[9]]) as usize;
            if data.len() < AI_REQUEST_HEADER_SIZE + chunk_len {
                return None;
            }
            Some(ParsedChunk {
                message_type: msg_type,
                last_chunk,
                request_id,
                audio_offset: AI_REQUEST_HEADER_SIZE,
                audio_length: chunk_len,
            })
        }
        MessageType::AiResponse => {
            // AI response: type(1) + request_id(4) + content_type(1) + last_chunk(1) + chunk_length(4)
            if data.len() < AI_RESPONSE_HEADER_SIZE {
                return None;
            }
            let request_id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            // content_type at data[5] - ignored for now
            let last_chunk = data[6] != 0;
            let chunk_len = u32::from_le_bytes([data[7], data[8], data[9], data[10]]) as usize;
            if data.len() < AI_RESPONSE_HEADER_SIZE + chunk_len {
                return None;
            }
            Some(ParsedChunk {
                message_type: msg_type,
                last_chunk,
                request_id,
                audio_offset: AI_RESPONSE_HEADER_SIZE,
                audio_length: chunk_len,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_media_chunk() {
        let audio = [0x01, 0x02, 0x03, 0x04];
        let mut out = [0u8; 20];
        let len = serialize_media_chunk(&audio, false, &mut out);

        assert_eq!(len, 10); // 6 header + 4 audio
        assert_eq!(out[0], MessageType::Media as u8);
        assert_eq!(out[1], 0); // last_chunk = false
        assert_eq!(&out[2..6], &4u32.to_le_bytes()); // chunk_length = 4
        assert_eq!(&out[6..10], &audio);
    }

    #[test]
    fn test_serialize_media_chunk_last() {
        let audio = [0xAA; 160];
        let mut out = [0u8; 200];
        let len = serialize_media_chunk(&audio, true, &mut out);

        assert_eq!(len, 166); // 6 header + 160 audio
        assert_eq!(out[1], 1); // last_chunk = true
    }

    #[test]
    fn test_serialize_ai_request_chunk() {
        let audio = [0x55; 160];
        let mut out = [0u8; 200];
        let len = serialize_ai_request_chunk(&audio, 0x12345678, true, &mut out);

        assert_eq!(len, 170); // 10 header + 160 audio
        assert_eq!(out[0], MessageType::AiRequest as u8);
        assert_eq!(&out[1..5], &0x12345678u32.to_le_bytes());
        assert_eq!(out[5], 1); // last_chunk = true
        assert_eq!(&out[6..10], &160u32.to_le_bytes());
    }

    #[test]
    fn test_parse_media_chunk() {
        let audio = [0x01, 0x02, 0x03, 0x04];
        let mut buf = [0u8; 20];
        let len = serialize_media_chunk(&audio, false, &mut buf);

        let parsed = parse_chunk(&buf[..len]).unwrap();
        assert_eq!(parsed.message_type, MessageType::Media);
        assert!(!parsed.last_chunk);
        assert_eq!(parsed.audio_offset, 6);
        assert_eq!(parsed.audio_length, 4);
        assert_eq!(&buf[parsed.audio_offset..parsed.audio_offset + parsed.audio_length], &audio);
    }

    #[test]
    fn test_parse_ai_request_chunk() {
        let audio = [0xBB; 160];
        let mut buf = [0u8; 200];
        let len = serialize_ai_request_chunk(&audio, 0xDEADBEEF, true, &mut buf);

        let parsed = parse_chunk(&buf[..len]).unwrap();
        assert_eq!(parsed.message_type, MessageType::AiRequest);
        assert!(parsed.last_chunk);
        assert_eq!(parsed.request_id, 0xDEADBEEF);
        assert_eq!(parsed.audio_offset, 10);
        assert_eq!(parsed.audio_length, 160);
    }

    #[test]
    fn test_parse_invalid_empty() {
        assert!(parse_chunk(&[]).is_none());
    }

    #[test]
    fn test_parse_invalid_type() {
        assert!(parse_chunk(&[0xFF]).is_none());
    }

    #[test]
    fn test_parse_truncated_media() {
        // Only header, no audio data
        let buf = [MessageType::Media as u8, 0, 10, 0, 0, 0];
        assert!(parse_chunk(&buf).is_none());
    }

    #[test]
    fn test_prepend_media_header() {
        let audio = [0x01, 0x02, 0x03, 0x04];
        let mut buf: Vec<u8, 20> = Vec::new();
        buf.extend_from_slice(&audio).unwrap();

        prepend_media_header(&mut buf, false).unwrap();

        assert_eq!(buf.len(), 10); // 6 header + 4 audio
        assert_eq!(buf[0], MessageType::Media as u8);
        assert_eq!(buf[1], 0); // last_chunk = false
        assert_eq!(&buf[2..6], &4u32.to_le_bytes());
        assert_eq!(&buf[6..10], &audio);
    }

    #[test]
    fn test_prepend_media_header_last() {
        let audio = [0xAA; 160];
        let mut buf: Vec<u8, 200> = Vec::new();
        buf.extend_from_slice(&audio).unwrap();

        prepend_media_header(&mut buf, true).unwrap();

        assert_eq!(buf.len(), 166); // 6 header + 160 audio
        assert_eq!(buf[1], 1); // last_chunk = true
    }

    #[test]
    fn test_prepend_ai_request_header() {
        let audio = [0x55; 160];
        let mut buf: Vec<u8, 200> = Vec::new();
        buf.extend_from_slice(&audio).unwrap();

        prepend_ai_request_header(&mut buf, 0x12345678, true).unwrap();

        assert_eq!(buf.len(), 170); // 10 header + 160 audio
        assert_eq!(buf[0], MessageType::AiRequest as u8);
        assert_eq!(&buf[1..5], &0x12345678u32.to_le_bytes());
        assert_eq!(buf[5], 1); // last_chunk = true
        assert_eq!(&buf[6..10], &160u32.to_le_bytes());
    }

    #[test]
    fn test_prepend_media_header_roundtrip() {
        let audio = [0x01, 0x02, 0x03, 0x04];
        let mut buf: Vec<u8, 20> = Vec::new();
        buf.extend_from_slice(&audio).unwrap();

        prepend_media_header(&mut buf, false).unwrap();

        let parsed = parse_chunk(&buf).unwrap();
        assert_eq!(parsed.message_type, MessageType::Media);
        assert!(!parsed.last_chunk);
        assert_eq!(parsed.audio_offset, 6);
        assert_eq!(parsed.audio_length, 4);
        assert_eq!(
            &buf[parsed.audio_offset..parsed.audio_offset + parsed.audio_length],
            &audio
        );
    }

    #[test]
    fn test_prepend_ai_request_header_roundtrip() {
        let audio = [0xBB; 160];
        let mut buf: Vec<u8, 200> = Vec::new();
        buf.extend_from_slice(&audio).unwrap();

        prepend_ai_request_header(&mut buf, 0xDEADBEEF, true).unwrap();

        let parsed = parse_chunk(&buf).unwrap();
        assert_eq!(parsed.message_type, MessageType::AiRequest);
        assert!(parsed.last_chunk);
        assert_eq!(parsed.request_id, 0xDEADBEEF);
        assert_eq!(parsed.audio_offset, 10);
        assert_eq!(parsed.audio_length, 160);
    }

    #[test]
    fn test_prepend_media_header_capacity_error() {
        let audio = [0x01; 10];
        let mut buf: Vec<u8, 10> = Vec::new(); // Too small for header + audio
        buf.extend_from_slice(&audio).unwrap();

        assert!(prepend_media_header(&mut buf, false).is_err());
    }
}
