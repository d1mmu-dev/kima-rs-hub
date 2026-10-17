//! App tokens: per-client secrets for the Subsonic API (spec §5.1, §5.2).
//!
//! A token is stored twice: its SHA-256 (the lookup key) and an authenticated encryption of it
//! (what token+salt auth needs). Neither is the token itself.

use chacha20poly1305::aead::OsRng;
use chacha20poly1305::aead::rand_core::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::cipher::{Cipher, CipherError};

/// 32 bytes from the OS RNG as 64 lowercase hex characters.
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The lookup hash: SHA-256 of the token's UTF-8 bytes.
pub fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Associated data binding a ciphertext to its `api_keys` row: `"api_keys:" ‖ uuid bytes`.
pub fn aad(key_id: Uuid) -> Vec<u8> {
    let mut aad = b"api_keys:".to_vec();
    aad.extend_from_slice(key_id.as_bytes());
    aad
}

pub fn seal(cipher: &Cipher, key_id: Uuid, token: &str) -> Vec<u8> {
    cipher.encrypt(token.as_bytes(), &aad(key_id))
}

pub fn open(cipher: &Cipher, key_id: Uuid, blob: &[u8]) -> Result<String, CipherError> {
    let plain = cipher.decrypt(blob, &aad(key_id))?;
    String::from_utf8(plain).map_err(|_| CipherError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_64_lowercase_hex_and_distinct() {
        let a = generate();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_ne!(a, generate());
    }

    #[test]
    fn hash_is_sha256() {
        assert_eq!(
            hex::encode(hash("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn aad_is_prefix_then_raw_uuid() {
        let id = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
        let aad = aad(id);
        assert_eq!(&aad[..9], b"api_keys:");
        assert_eq!(&aad[9..], &(1u8..=16).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn seal_open_round_trip_and_row_binding() {
        let cipher = Cipher::random();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let blob = seal(&cipher, a, "secret");
        assert_eq!(open(&cipher, a, &blob).unwrap(), "secret");
        assert_eq!(open(&cipher, b, &blob), Err(CipherError::Decrypt));
    }
}
