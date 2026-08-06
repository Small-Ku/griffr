//! Cryptography utilities for Hypergryph APIs

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD, Engine};

use crate::error::{Error, Result};

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

/// Decrypt an owned game_files manifest buffer using AES-256-CBC.
///
/// The plaintext replaces the ciphertext in the same allocation.
pub fn decrypt_game_files_owned(mut data: Vec<u8>) -> Result<String> {
    let plaintext_len = Aes256CbcDec::new(GAME_FILES_AES_KEY.into(), GAME_FILES_AES_IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut data)
        .map_err(|e| Error::Message {
            context: "Crypto error: ",
            detail: format!("AES decryption failed: {e}"),
        })?
        .len();
    data.truncate(plaintext_len);
    Ok(String::from_utf8(data)?)
}

/// Decrypt a borrowed game_files manifest using AES-256-CBC.
pub fn decrypt_game_files(data: &[u8]) -> Result<String> {
    decrypt_game_files_owned(data.to_vec())
}

/// Encrypt data using AES-256-CBC (inverse of decrypt_game_files)
///
/// This is primarily useful for generating test samples.
pub fn encrypt_game_files(data: &[u8]) -> Result<Vec<u8>> {
    let pt = data;
    // PKCS7 padding: need at least 1 byte of padding, up to 16
    let padded_len = pt.len() + (16 - pt.len() % 16);
    let mut buf = vec![0u8; padded_len];
    buf[..pt.len()].copy_from_slice(pt);

    let encrypted_len = Aes256CbcEnc::new(GAME_FILES_AES_KEY.into(), GAME_FILES_AES_IV.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, pt.len())
        .map_err(|e| Error::Message {
            context: "Crypto error: ",
            detail: format!("AES encryption failed: {e}"),
        })?
        .len();
    buf.truncate(encrypted_len);
    Ok(buf)
}

/// Decrypt resource index files using modular subtraction cipher
pub fn decrypt_res_index(data_base64: &str, key: &str) -> Result<String> {
    let mut decrypted = STANDARD.decode(data_base64)?;
    let key_bytes = key.as_bytes();
    if key_bytes.is_empty() {
        return Err(Error::Message {
            context: "Crypto error: ",
            detail: "Resource index key cannot be empty".to_string(),
        });
    }

    for (index, byte) in decrypted.iter_mut().enumerate() {
        *byte = byte.wrapping_sub(key_bytes[index % key_bytes.len()]);
    }

    Ok(String::from_utf8(decrypted)?)
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
    fn test_decrypt_res_index_rejects_empty_key() {
        let base64_input = STANDARD.encode([1u8]);
        let error = decrypt_res_index(&base64_input, "").unwrap_err();
        assert!(error.to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_game_files_round_trip() {
        let plaintext = b"version=1.2.3\nentry=Endfield.exe\n";
        let encrypted = encrypt_game_files(plaintext).unwrap();
        assert_eq!(
            decrypt_game_files(&encrypted).unwrap().as_bytes(),
            plaintext
        );
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
