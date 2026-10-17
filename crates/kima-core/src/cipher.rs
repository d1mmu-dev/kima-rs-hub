//! Authenticated encryption for secrets kept in the database (app tokens, ...).
//!
//! Blob layout: `FORMAT_V1 ‖ nonce (24) ‖ ciphertext ‖ tag (16)`. The key is HKDF-SHA256 over
//! `SETTINGS_ENCRYPTION_KEY`. Callers bind each blob to its row with the AAD.

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;

pub const FORMAT_V1: u8 = 0x01;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CipherError {
    #[error("unknown cipher format version {0}")]
    UnknownVersion(u8),
    #[error("encrypted value is too short")]
    Malformed,
    #[error("decryption failed")]
    Decrypt,
}

#[derive(Clone)]
pub struct Cipher {
    key: [u8; 32],
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher { .. }")
    }
}

fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let salt = if salt.is_empty() { None } else { Some(salt) };
    let hk = Hkdf::<Sha256>::new(salt, ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

impl Cipher {
    pub fn from_secret(secret: &str) -> Cipher {
        Cipher {
            key: hkdf_sha256(secret.as_bytes(), &[], b"kima-rs cipher v1"),
        }
    }

    /// A throwaway key; blobs do not survive the process. The default for tests.
    pub fn random() -> Cipher {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        Cipher { key }
    }

    fn aead(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(&self.key))
    }

    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self
            .aead()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory input");
        let mut blob = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        blob.push(FORMAT_V1);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        blob
    }

    pub fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>, CipherError> {
        if blob.len() < 1 + NONCE_LEN + TAG_LEN {
            return Err(CipherError::Malformed);
        }
        if blob[0] != FORMAT_V1 {
            return Err(CipherError::UnknownVersion(blob[0]));
        }
        let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
        self.aead()
            .decrypt(
                nonce,
                Payload {
                    msg: &blob[1 + NONCE_LEN..],
                    aad,
                },
            )
            .map_err(|_| CipherError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let c = Cipher::random();
        let blob = c.encrypt(b"hello", b"aad");
        assert_eq!(c.decrypt(&blob, b"aad").unwrap(), b"hello");
    }

    #[test]
    fn wrong_aad_fails() {
        let c = Cipher::random();
        let blob = c.encrypt(b"hello", b"aad");
        assert_eq!(c.decrypt(&blob, b"other"), Err(CipherError::Decrypt));
    }

    #[test]
    fn wrong_key_fails() {
        let blob = Cipher::from_secret("a").encrypt(b"hello", b"aad");
        assert_eq!(
            Cipher::from_secret("b").decrypt(&blob, b"aad"),
            Err(CipherError::Decrypt)
        );
    }

    #[test]
    fn format_is_versioned() {
        let c = Cipher::random();
        let mut blob = c.encrypt(b"hello", b"aad");
        assert_eq!(blob[0], 0x01);
        assert_eq!(blob.len(), 1 + 24 + 5 + 16);
        blob[0] = 2;
        assert_eq!(
            c.decrypt(&blob, b"aad"),
            Err(CipherError::UnknownVersion(2))
        );
        assert_eq!(c.decrypt(&[1u8; 10], b"aad"), Err(CipherError::Malformed));
    }

    #[test]
    fn nonces_differ() {
        let c = Cipher::random();
        let a = c.encrypt(b"same", b"aad");
        let b = c.encrypt(b"same", b"aad");
        assert_ne!(a[1..25], b[1..25]);
    }

    #[test]
    fn from_secret_key_is_pinned() {
        let c = Cipher::from_secret("kima-test-secret");
        assert_eq!(
            hex::encode(c.key),
            "c94dc075476e5fa21324053b713341e300095c94ba1d49b01cb619be0369b5be"
        );
    }

    #[test]
    fn tampered_ciphertext_or_tag_fails() {
        let c = Cipher::random();
        let blob = c.encrypt(b"hello world", b"aad");
        let mut ct = blob.clone();
        ct[1 + 24] ^= 1;
        assert!(c.decrypt(&ct, b"aad").is_err());
        let mut tag = blob.clone();
        *tag.last_mut().unwrap() ^= 1;
        assert!(c.decrypt(&tag, b"aad").is_err());
    }

    #[test]
    fn hkdf_matches_rfc5869_case_3() {
        let okm = hkdf_sha256(&[0x0b; 22], &[], &[]);
        assert_eq!(
            hex::encode(okm),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d"
        );
    }
}
