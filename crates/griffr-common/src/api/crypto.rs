//! Cryptography utilities for Hypergryph APIs

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};

/// AES-256-CBC key for game_files manifest decryption
pub const GAME_FILES_AES_KEY: &[u8; 32] = &[
    0xC0, 0xF3, 0x0E, 0x1C, 0xE7, 0x63, 0xBB, 0xC2, 0x1C, 0xC3, 0x55, 0xA3, 0x43, 0x03, 0xAC, 0x50,
    0x39, 0x94, 0x44, 0xBF, 0xF6, 0x8C, 0x4A, 0x22, 0xAF, 0x39, 0x8C, 0x0A, 0x16, 0x6E, 0xE1, 0x43,
];

/// AES-256-CBC IV for game_files manifest decryption
pub const GAME_FILES_AES_IV: &[u8; 16] = &[
    0x33, 0x46, 0x78, 0x61, 0x19, 0x27, 0x50, 0x64, 0x95, 0x01, 0x93, 0x72, 0x64, 0x60, 0x84, 0x00,
];

/// Resource index decryption key (Endfield)
pub const RES_INDEX_KEY: &str = "Assets/Beyond/DynamicAssets/Gameplay/UI/Fonts/";

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

/// Decrypt the game_files manifest using AES-256-CBC
pub fn decrypt_game_files(data: &[u8]) -> Result<String> {
    let mut buf = data.to_vec();
    let pt = Aes256CbcDec::new(GAME_FILES_AES_KEY.into(), GAME_FILES_AES_IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow::anyhow!("AES decryption failed: {}", e))?;

    let decrypted =
        String::from_utf8(pt.to_vec()).context("Failed to parse decrypted manifest as UTF-8")?;

    Ok(decrypted)
}

/// Encrypt data using AES-256-CBC (inverse of decrypt_game_files)
///
/// This is primarily useful for generating test fixtures.
pub fn encrypt_game_files(data: &[u8]) -> Result<Vec<u8>> {
    let pt = data;
    // PKCS7 padding: need at least 1 byte of padding, up to 16
    let padded_len = pt.len() + (16 - pt.len() % 16);
    let mut buf = vec![0u8; padded_len];
    buf[..pt.len()].copy_from_slice(pt);

    let encrypted = Aes256CbcEnc::new(GAME_FILES_AES_KEY.into(), GAME_FILES_AES_IV.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, pt.len())
        .map_err(|e| anyhow::anyhow!("AES encryption failed: {}", e))?;

    Ok(encrypted.to_vec())
}

/// Decrypt resource index files using modular subtraction cipher
pub fn decrypt_res_index(data_base64: &str, key: &str) -> Result<String> {
    let encrypted = STANDARD
        .decode(data_base64)
        .context("Failed to base64 decode resource index")?;

    let key_bytes = key.as_bytes();
    let key_len = key_bytes.len();

    let mut decrypted = Vec::with_capacity(encrypted.len());

    for (i, &enc_byte) in encrypted.iter().enumerate() {
        let key_byte = key_bytes[i % key_len];
        // plain_byte[i] = (enc_byte[i] - key_byte[i % key_length] + 256) % 256
        let plain_byte = ((enc_byte as i16 - key_byte as i16 + 256) % 256) as u8;
        decrypted.push(plain_byte);
    }

    let result = String::from_utf8(decrypted)
        .context("Failed to parse decrypted resource index as UTF-8")?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modular_subtraction_cipher() {
        // Simple test case: "ABC" encrypted with key "123"
        // 'A' (65), '1' (49) -> (65 + 49) % 256 = 114 ('r')
        // 'B' (66), '2' (50) -> (66 + 50) % 256 = 116 ('t')
        // 'C' (67), '3' (51) -> (67 + 51) % 256 = 118 ('v')
        let encrypted_bytes = vec![114, 116, 118];
        let base64_input = STANDARD.encode(encrypted_bytes);
        let key = "123";

        let decrypted = decrypt_res_index(&base64_input, key).unwrap();
        assert_eq!(decrypted, "ABC");
    }

    #[test]
    fn test_modular_subtraction_cipher_key_reuse() {
        // Test key cycling (key is shorter than data)
        // "ABCD" with key "12":
        // 'A' (65), '1' (49) -> (65 + 49) % 256 = 114
        // 'B' (66), '2' (50) -> (66 + 50) % 256 = 116
        // 'C' (67), '1' (49) -> (67 + 49) % 256 = 116 (key cycles)
        // 'D' (68), '2' (50) -> (68 + 50) % 256 = 118
        let encrypted_bytes = vec![114, 116, 116, 118];
        let base64_input = STANDARD.encode(encrypted_bytes);
        let key = "12";

        let decrypted = decrypt_res_index(&base64_input, key).unwrap();
        assert_eq!(decrypted, "ABCD");
    }

    #[test]
    fn test_modular_subtraction_cipher_wraparound() {
        // Test wraparound: 0 - 1 = 255 (byte underflow)
        // Plain: 0x00, Key: 0x01 -> Encrypted: 0x01
        // Decrypt: 0x01 - 0x01 = 0x00
        let encrypted_bytes = vec![0x01];
        let base64_input = STANDARD.encode(encrypted_bytes);
        let key = &[0x01u8]; // Key byte = 1

        let decrypted =
            decrypt_res_index(&base64_input, std::str::from_utf8(key).unwrap()).unwrap();
        assert_eq!(decrypted.as_bytes(), &[0x00]);
    }

    #[test]
    fn test_decrypt_res_index_invalid_base64() {
        let result = decrypt_res_index("not_valid_base64!!!", "key");
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_res_index_empty() {
        // Empty input
        let base64_input = STANDARD.encode([] as [u8; 0]);
        let decrypted = decrypt_res_index(&base64_input, "key").unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn test_decrypt_game_files_invalid_data() {
        // Invalid encrypted data (too short or wrong format)
        let result = decrypt_game_files(b"short");
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_game_files_empty() {
        // Empty data should error (can't decrypt nothing)
        let result = decrypt_game_files(b"");
        assert!(result.is_err());
    }

    #[test]
    fn test_known_endfield_res_index_key() {
        // The known Endfield resource index key
        let key = "Assets/Beyond/DynamicAssets/Gameplay/UI/Fonts/";
        // Key length is 46 characters
        assert_eq!(key.len(), 46);

        // Simple verification that key can be used
        let test_data = "test";
        let encrypted = test_data
            .bytes()
            .enumerate()
            .map(|(i, b)| (b as u16 + key.as_bytes()[i % key.len()] as u16) as u8)
            .collect::<Vec<_>>();

        let base64 = STANDARD.encode(encrypted);
        let decrypted = decrypt_res_index(&base64, key).unwrap();
        assert_eq!(decrypted, test_data);
    }
}
