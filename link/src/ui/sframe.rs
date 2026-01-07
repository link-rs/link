//! SFrame (RFC 9605) implementation for AES-128-GCM cipher suite.
//!
//! This module provides a minimal implementation of the SFrame encryption format
//! as defined in RFC 9605, supporting only the AES_128_GCM_SHA256_128 cipher suite
//! (ID 0x0004).

use aes_gcm::{
    Aes128Gcm, Nonce,
    aead::{AeadInPlace, Buffer, KeyInit},
};
use heapless::Vec;
use hkdf::Hkdf;
use sha2::Sha256;

/// Buffer wrapper for heapless::Vec to implement aead::Buffer trait
struct HeaplessBuffer<'a, const N: usize>(&'a mut Vec<u8, N>);

impl<const N: usize> AsRef<[u8]> for HeaplessBuffer<'_, N> {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl<const N: usize> AsMut<[u8]> for HeaplessBuffer<'_, N> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut_slice()
    }
}

impl<const N: usize> Buffer for HeaplessBuffer<'_, N> {
    fn extend_from_slice(&mut self, other: &[u8]) -> aes_gcm::aead::Result<()> {
        self.0
            .extend_from_slice(other)
            .map_err(|_| aes_gcm::aead::Error)
    }

    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }
}

/// AES-128-GCM cipher suite identifier (RFC 9605 Section 4.4)
pub const CIPHER_SUITE_AES_128_GCM: u16 = 0x0004;

/// Key size for AES-128-GCM (Nk = 16 bytes)
pub const KEY_SIZE: usize = 16;

/// Nonce size for AES-128-GCM (Nn = 12 bytes)
pub const NONCE_SIZE: usize = 12;

/// Authentication tag size for AES-128-GCM (Nt = 16 bytes)
pub const TAG_SIZE: usize = 16;

/// Maximum header size (1 config byte + 8 bytes KID + 8 bytes CTR)
pub const MAX_HEADER_SIZE: usize = 17;

/// Error type for SFrame operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Buffer too small for output
    BufferTooSmall,
    /// Invalid header format
    InvalidHeader,
    /// Authentication failed during decryption
    AuthenticationFailed,
    /// Invalid key material
    InvalidKey,
    /// Key ID in ciphertext does not match KeyMaterial
    KeyIdMismatch,
}

/// Derived key material for SFrame encryption/decryption
pub struct KeyMaterial {
    /// The key identifier
    kid: u64,
    /// The derived AEAD key (16 bytes for AES-128-GCM)
    key: [u8; KEY_SIZE],
    /// The derived salt for nonce formation (12 bytes)
    salt: [u8; NONCE_SIZE],
}

impl KeyMaterial {
    /// Derive SFrame key material from a base key.
    ///
    /// Uses HKDF-SHA256 to derive the encryption key and salt as specified
    /// in RFC 9605 Section 4.3.1.
    ///
    /// # Arguments
    /// * `base_key` - The 16-byte base key
    /// * `kid` - The key identifier
    pub fn derive(base_key: &[u8; KEY_SIZE], kid: u64) -> Self {
        // sframe_secret = HKDF-Extract("", base_key)
        let hk = Hkdf::<Sha256>::new(None, base_key);

        // Build the info for key derivation:
        // "SFrame 1.0 Secret key " + KID (8 bytes BE) + cipher_suite (2 bytes BE)
        let mut key_info = [0u8; 32];
        key_info[..22].copy_from_slice(b"SFrame 1.0 Secret key ");
        key_info[22..30].copy_from_slice(&kid.to_be_bytes());
        key_info[30..32].copy_from_slice(&CIPHER_SUITE_AES_128_GCM.to_be_bytes());

        // Build the info for salt derivation:
        // "SFrame 1.0 Secret salt " + KID (8 bytes BE) + cipher_suite (2 bytes BE)
        let mut salt_info = [0u8; 33];
        salt_info[..23].copy_from_slice(b"SFrame 1.0 Secret salt ");
        salt_info[23..31].copy_from_slice(&kid.to_be_bytes());
        salt_info[31..33].copy_from_slice(&CIPHER_SUITE_AES_128_GCM.to_be_bytes());

        let mut key = [0u8; KEY_SIZE];
        let mut salt = [0u8; NONCE_SIZE];

        // sframe_key = HKDF-Expand(sframe_secret, key_info, Nk)
        hk.expand(&key_info, &mut key).expect("valid key length");

        // sframe_salt = HKDF-Expand(sframe_secret, salt_info, Nn)
        hk.expand(&salt_info, &mut salt).expect("valid salt length");

        Self { kid, key, salt }
    }

    /// Protect a plaintext in place using SFrame.
    ///
    /// The buffer should contain the plaintext on input. On success, the buffer
    /// will contain the SFrame ciphertext (header + encrypted data + tag).
    ///
    /// # Arguments
    /// * `ctr` - Counter value (must be unique for each message with this key)
    /// * `metadata` - Additional metadata to authenticate (not encrypted)
    /// * `buf` - Buffer containing plaintext on input, ciphertext on output
    pub fn protect<const N: usize>(
        &self,
        ctr: u64,
        metadata: &[u8],
        buf: &mut Vec<u8, N>,
    ) -> Result<(), Error> {
        // Encode the header into a temporary buffer
        let mut header: Vec<u8, MAX_HEADER_SIZE> = Vec::new();
        encode_header(self.kid, ctr, &mut header)?;

        // Build AAD = header || metadata
        let mut aad: Vec<u8, 128> = Vec::new();
        aad.extend_from_slice(&header)
            .map_err(|_| Error::BufferTooSmall)?;
        aad.extend_from_slice(metadata)
            .map_err(|_| Error::BufferTooSmall)?;

        // Form nonce
        let nonce = form_nonce(&self.salt, ctr);

        // Create cipher and encrypt in place (appends tag)
        let cipher = Aes128Gcm::new_from_slice(&self.key).map_err(|_| Error::InvalidKey)?;
        cipher
            .encrypt_in_place(Nonce::from_slice(&nonce), &aad, &mut HeaplessBuffer(buf))
            .map_err(|_| Error::BufferTooSmall)?;

        // Prepend header by rotating: save ciphertext, clear, write header, append ciphertext
        let ct_len = buf.len();
        let mut temp: Vec<u8, N> = Vec::new();
        temp.extend_from_slice(buf)
            .map_err(|_| Error::BufferTooSmall)?;
        buf.clear();
        buf.extend_from_slice(&header)
            .map_err(|_| Error::BufferTooSmall)?;
        buf.extend_from_slice(&temp[..ct_len])
            .map_err(|_| Error::BufferTooSmall)?;

        Ok(())
    }

    /// Unprotect an SFrame ciphertext in place.
    ///
    /// The buffer should contain the full SFrame ciphertext (header + encrypted data + tag)
    /// on input. On success, the buffer will contain only the plaintext.
    ///
    /// # Arguments
    /// * `metadata` - The metadata that was used during protection
    /// * `buf` - Buffer containing ciphertext on input, plaintext on output
    pub fn unprotect<const N: usize>(
        &self,
        metadata: &[u8],
        buf: &mut Vec<u8, N>,
    ) -> Result<(), Error> {
        // Parse header
        let header = decode_header(buf)?;

        // Verify KID matches
        if header.kid != self.kid {
            return Err(Error::KeyIdMismatch);
        }

        // Check minimum size
        if buf.len() < header.len + TAG_SIZE {
            return Err(Error::InvalidHeader);
        }

        // Build AAD = header || metadata
        let mut aad: Vec<u8, 128> = Vec::new();
        aad.extend_from_slice(&buf[..header.len])
            .map_err(|_| Error::BufferTooSmall)?;
        aad.extend_from_slice(metadata)
            .map_err(|_| Error::BufferTooSmall)?;

        // Form nonce
        let nonce = form_nonce(&self.salt, header.ctr);

        // Remove header, keeping only ciphertext+tag
        let header_len = header.len;
        let ct_len = buf.len() - header_len;
        for i in 0..ct_len {
            buf[i] = buf[i + header_len];
        }
        buf.truncate(ct_len);

        // Create cipher and decrypt in place
        let cipher = Aes128Gcm::new_from_slice(&self.key).map_err(|_| Error::InvalidKey)?;
        cipher
            .decrypt_in_place(Nonce::from_slice(&nonce), &aad, &mut HeaplessBuffer(buf))
            .map_err(|_| Error::AuthenticationFailed)?;

        Ok(())
    }
}

/// Compute the minimum number of bytes needed to represent a value.
fn min_bytes_for_value(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        ((64 - value.leading_zeros()) as usize).div_ceil(8)
    }
}

/// Encode an SFrame header.
fn encode_header<const N: usize>(kid: u64, ctr: u64, out: &mut Vec<u8, N>) -> Result<usize, Error> {
    let start_len = out.len();

    let kid_be = kid.to_be_bytes();
    let ctr_be = ctr.to_be_bytes();

    // Determine encoding for KID
    let (x, k, kid_ext_bytes): (u8, u8, usize) = if kid < 8 {
        (0, kid as u8, 0)
    } else {
        let num_bytes = min_bytes_for_value(kid);
        (1, (num_bytes - 1) as u8, num_bytes)
    };

    // Determine encoding for CTR
    let (y, c, ctr_ext_bytes): (u8, u8, usize) = if ctr < 8 {
        (0, ctr as u8, 0)
    } else {
        let num_bytes = min_bytes_for_value(ctr);
        (1, (num_bytes - 1) as u8, num_bytes)
    };

    // Build config byte: X(1) K(3) Y(1) C(3)
    let config = (x << 7) | (k << 4) | (y << 3) | c;
    out.push(config).map_err(|_| Error::BufferTooSmall)?;

    // Append KID bytes if extended
    if kid_ext_bytes > 0 {
        out.extend_from_slice(&kid_be[8 - kid_ext_bytes..])
            .map_err(|_| Error::BufferTooSmall)?;
    }

    // Append CTR bytes if extended
    if ctr_ext_bytes > 0 {
        out.extend_from_slice(&ctr_be[8 - ctr_ext_bytes..])
            .map_err(|_| Error::BufferTooSmall)?;
    }

    Ok(out.len() - start_len)
}

/// Parsed SFrame header
#[derive(Debug, Clone, Copy)]
struct Header {
    /// Key identifier
    kid: u64,
    /// Counter value
    ctr: u64,
    /// Length of the encoded header in bytes
    len: usize,
}

/// Decode an SFrame header from the input buffer.
fn decode_header(input: &[u8]) -> Result<Header, Error> {
    if input.is_empty() {
        return Err(Error::InvalidHeader);
    }

    let config = input[0];
    let x = (config >> 7) & 1;
    let k = (config >> 4) & 0x07;
    let y = (config >> 3) & 1;
    let c = config & 0x07;

    let mut offset = 1;

    // Parse KID
    let kid = if x == 0 {
        k as u64
    } else {
        let num_bytes = (k + 1) as usize;
        if input.len() < offset + num_bytes {
            return Err(Error::InvalidHeader);
        }
        let mut kid_bytes = [0u8; 8];
        kid_bytes[8 - num_bytes..].copy_from_slice(&input[offset..offset + num_bytes]);
        offset += num_bytes;
        u64::from_be_bytes(kid_bytes)
    };

    // Parse CTR
    let ctr = if y == 0 {
        c as u64
    } else {
        let num_bytes = (c + 1) as usize;
        if input.len() < offset + num_bytes {
            return Err(Error::InvalidHeader);
        }
        let mut ctr_bytes = [0u8; 8];
        ctr_bytes[8 - num_bytes..].copy_from_slice(&input[offset..offset + num_bytes]);
        offset += num_bytes;
        u64::from_be_bytes(ctr_bytes)
    };

    Ok(Header {
        kid,
        ctr,
        len: offset,
    })
}

/// Form the nonce by XORing the salt with the counter.
fn form_nonce(salt: &[u8; NONCE_SIZE], ctr: u64) -> [u8; NONCE_SIZE] {
    let mut ctr_bytes = [0u8; NONCE_SIZE];
    ctr_bytes[4..].copy_from_slice(&ctr.to_be_bytes());

    let mut nonce = [0u8; NONCE_SIZE];
    for i in 0..NONCE_SIZE {
        nonce[i] = salt[i] ^ ctr_bytes[i];
    }
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from RFC 9605 (via sframe-wg/sframe test-vectors.json)
    // cipher_suite: 4 (AES_128_GCM_SHA256_128)
    const TEST_KID: u64 = 291; // 0x123
    const TEST_CTR: u64 = 17767; // 0x4567
    const TEST_BASE_KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const TEST_SFRAME_KEY: [u8; 16] = [
        0xd3, 0x4f, 0x54, 0x7f, 0x4c, 0xa4, 0xf9, 0xa7, 0x44, 0x70, 0x06, 0xfe, 0x7f, 0xcb, 0xf7,
        0x68,
    ];
    const TEST_SFRAME_SALT: [u8; 12] = [
        0x75, 0x23, 0x4e, 0xde, 0xfe, 0x07, 0x81, 0x90, 0x26, 0x75, 0x18, 0x16,
    ];
    const TEST_METADATA: &[u8] = &[
        0x49, 0x45, 0x54, 0x46, 0x20, 0x53, 0x46, 0x72, 0x61, 0x6d, 0x65, 0x20, 0x57, 0x47,
    ];
    const TEST_NONCE: [u8; 12] = [
        0x75, 0x23, 0x4e, 0xde, 0xfe, 0x07, 0x81, 0x90, 0x26, 0x75, 0x5d, 0x71,
    ];
    const TEST_PLAINTEXT: &[u8] = &[
        0x64, 0x72, 0x61, 0x66, 0x74, 0x2d, 0x69, 0x65, 0x74, 0x66, 0x2d, 0x73, 0x66, 0x72, 0x61,
        0x6d, 0x65, 0x2d, 0x65, 0x6e, 0x63,
    ];
    const TEST_CIPHERTEXT: &[u8] = &[
        0x99, 0x01, 0x23, 0x45, 0x67, 0xb7, 0x41, 0x2c, 0x25, 0x13, 0xa1, 0xb6, 0x6d, 0xbb, 0x48,
        0x84, 0x1b, 0xba, 0xf1, 0x7f, 0x59, 0x87, 0x51, 0x17, 0x6a, 0xd8, 0x47, 0x68, 0x1a, 0x69,
        0xc6, 0xd0, 0xb0, 0x91, 0xc0, 0x70, 0x18, 0xce, 0x4a, 0xdb, 0x34, 0xeb,
    ];

    #[test]
    fn test_key_derivation() {
        let km = KeyMaterial::derive(&TEST_BASE_KEY, TEST_KID);
        assert_eq!(km.key, TEST_SFRAME_KEY);
        assert_eq!(km.salt, TEST_SFRAME_SALT);
        assert_eq!(km.kid, TEST_KID);
    }

    #[test]
    fn test_header_encoding() {
        // Test vector: kid=291 (0x123), ctr=17767 (0x4567)
        // Expected header: 0x99 0x01 0x23 0x45 0x67
        // Config byte: X=1 K=1 (2 bytes for KID) Y=1 C=1 (2 bytes for CTR)
        // 0x99 = 1_001_1_001 = X=1, K=1, Y=1, C=1
        let mut header: Vec<u8, 32> = Vec::new();
        encode_header(TEST_KID, TEST_CTR, &mut header).unwrap();
        assert_eq!(&header[..], &[0x99, 0x01, 0x23, 0x45, 0x67]);
    }

    #[test]
    fn test_header_decoding() {
        let header_bytes = [0x99, 0x01, 0x23, 0x45, 0x67];
        let header = decode_header(&header_bytes).unwrap();
        assert_eq!(header.kid, TEST_KID);
        assert_eq!(header.ctr, TEST_CTR);
        assert_eq!(header.len, 5);
    }

    #[test]
    fn test_nonce_formation() {
        let nonce = form_nonce(&TEST_SFRAME_SALT, TEST_CTR);
        assert_eq!(nonce, TEST_NONCE);
    }

    #[test]
    fn test_header_encoding_small_values() {
        // kid=0, ctr=0 -> config=0x00
        let mut header: Vec<u8, 32> = Vec::new();
        encode_header(0, 0, &mut header).unwrap();
        assert_eq!(&header[..], &[0x00]);

        // kid=7, ctr=7 -> config=0x77
        let mut header: Vec<u8, 32> = Vec::new();
        encode_header(7, 7, &mut header).unwrap();
        assert_eq!(&header[..], &[0x77]);

        // kid=0, ctr=0xff -> Y=1, C=0 (1 byte), config=0x08 0xff
        let mut header: Vec<u8, 32> = Vec::new();
        encode_header(0, 0xff, &mut header).unwrap();
        assert_eq!(&header[..], &[0x08, 0xff]);
    }

    #[test]
    fn test_protect() {
        let km = KeyMaterial::derive(&TEST_BASE_KEY, TEST_KID);

        let mut buf: Vec<u8, 128> = Vec::new();
        buf.extend_from_slice(TEST_PLAINTEXT).unwrap();
        km.protect(TEST_CTR, TEST_METADATA, &mut buf).unwrap();

        assert_eq!(&buf[..], TEST_CIPHERTEXT);
    }

    #[test]
    fn test_unprotect() {
        let km = KeyMaterial::derive(&TEST_BASE_KEY, TEST_KID);

        let mut buf: Vec<u8, 128> = Vec::new();
        buf.extend_from_slice(TEST_CIPHERTEXT).unwrap();
        km.unprotect(TEST_METADATA, &mut buf).unwrap();

        assert_eq!(&buf[..], TEST_PLAINTEXT);
    }

    #[test]
    fn test_unprotect_wrong_kid() {
        let km = KeyMaterial::derive(&TEST_BASE_KEY, 999); // Wrong KID

        let mut buf: Vec<u8, 128> = Vec::new();
        buf.extend_from_slice(TEST_CIPHERTEXT).unwrap();
        let result = km.unprotect(TEST_METADATA, &mut buf);

        assert_eq!(result, Err(Error::KeyIdMismatch));
    }

    #[test]
    fn test_round_trip() {
        let base_key = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        let kid = 42;
        let ctr = 12345;
        let plaintext = b"Hello, SFrame!";
        let metadata = b"test metadata";

        let km = KeyMaterial::derive(&base_key, kid);

        let mut buf: Vec<u8, 128> = Vec::new();
        buf.extend_from_slice(plaintext).unwrap();
        km.protect(ctr, metadata, &mut buf).unwrap();

        km.unprotect(metadata, &mut buf).unwrap();

        assert_eq!(&buf[..], plaintext);
    }
}
