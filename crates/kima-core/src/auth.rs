//! Password hashing and the JWTs the frontend carries (same claims as upstream).

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ACCESS_TTL_SECS: i64 = 24 * 3600;
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;
pub const STREAM_TTL_SECS: i64 = 4 * 3600;

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 with default parameters cannot fail")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Spends the time of a real verification, for logins that name no user.
pub fn verify_dummy(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let hash = DUMMY.get_or_init(|| hash_password("kima-dummy-password"));
    let _ = verify_password(password, hash);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claims {
    pub user_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub token_version: i32,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
    Stream,
}

impl Claims {
    pub fn kind(&self) -> TokenKind {
        if self.token_type.as_deref() == Some("refresh") {
            TokenKind::Refresh
        } else if self.scope.as_deref() == Some("stream") {
            TokenKind::Stream
        } else {
            TokenKind::Access
        }
    }
}

#[derive(Debug, PartialEq, thiserror::Error)]
#[error("invalid or expired token")]
pub struct InvalidToken;

pub struct JwtKeys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
}

impl JwtKeys {
    pub fn new(secret: &str) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            validation,
        }
    }

    pub fn issue_access(
        &self,
        user_id: Uuid,
        username: &str,
        role: &str,
        token_version: i32,
    ) -> String {
        self.sign(
            Claims {
                user_id,
                username: Some(username.to_string()),
                role: Some(role.to_string()),
                token_version,
                token_type: None,
                scope: None,
                iat: 0,
                exp: 0,
            },
            ACCESS_TTL_SECS,
        )
    }

    pub fn issue_refresh(&self, user_id: Uuid, token_version: i32) -> String {
        self.sign(
            Claims {
                user_id,
                username: None,
                role: None,
                token_version,
                token_type: Some("refresh".into()),
                scope: None,
                iat: 0,
                exp: 0,
            },
            REFRESH_TTL_SECS,
        )
    }

    pub fn issue_stream(&self, user_id: Uuid, token_version: i32) -> String {
        self.sign(
            Claims {
                user_id,
                username: None,
                role: None,
                token_version,
                token_type: None,
                scope: Some("stream".into()),
                iat: 0,
                exp: 0,
            },
            STREAM_TTL_SECS,
        )
    }

    pub fn decode(&self, token: &str) -> Result<Claims, InvalidToken> {
        decode::<Claims>(token, &self.decoding, &self.validation)
            .map(|data| data.claims)
            .map_err(|_| InvalidToken)
    }

    /// Stamps `iat`/`exp` (now, now + `ttl_secs`) and signs.
    fn sign(&self, mut claims: Claims, ttl_secs: i64) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        claims.iat = now;
        claims.exp = now + ttl_secs;
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .expect("HS256 signing cannot fail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let hash = hash_password("hunter22");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("hunter22", &hash));
        assert!(!verify_password("hunter23", &hash));
    }

    #[test]
    fn garbage_hash_never_verifies() {
        assert!(!verify_password("x", "not-a-hash"));
        assert!(!verify_password("x", ""));
    }

    #[test]
    fn dummy_verification_does_not_panic() {
        verify_dummy("anything");
    }

    #[test]
    fn access_token_round_trip() {
        let keys = JwtKeys::new("secret");
        let id = Uuid::now_v7();
        let claims = keys
            .decode(&keys.issue_access(id, "ana", "admin", 3))
            .unwrap();
        assert_eq!(claims.user_id, id);
        assert_eq!(claims.username.as_deref(), Some("ana"));
        assert_eq!(claims.role.as_deref(), Some("admin"));
        assert_eq!(claims.token_version, 3);
        assert_eq!(claims.kind(), TokenKind::Access);
        assert_eq!(claims.exp - claims.iat, ACCESS_TTL_SECS);
    }

    #[test]
    fn refresh_and_stream_tokens_have_their_kind() {
        let keys = JwtKeys::new("secret");
        let id = Uuid::now_v7();
        let refresh = keys.decode(&keys.issue_refresh(id, 0)).unwrap();
        assert_eq!(refresh.kind(), TokenKind::Refresh);
        assert_eq!(refresh.exp - refresh.iat, REFRESH_TTL_SECS);
        let stream = keys.decode(&keys.issue_stream(id, 0)).unwrap();
        assert_eq!(stream.kind(), TokenKind::Stream);
        assert_eq!(stream.exp - stream.iat, STREAM_TTL_SECS);
    }

    #[test]
    fn wire_format_matches_upstream() {
        let keys = JwtKeys::new("secret");
        let token = keys.issue_refresh(Uuid::nil(), 7);
        let payload = token.split('.').nth(1).unwrap();
        use base64::Engine;
        let json: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["userId"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["tokenVersion"], 7);
        assert_eq!(json["type"], "refresh");
        assert!(json.get("username").is_none());
    }

    #[test]
    fn other_secret_is_rejected() {
        let token = JwtKeys::new("a").issue_access(Uuid::now_v7(), "u", "user", 0);
        assert_eq!(JwtKeys::new("b").decode(&token), Err(InvalidToken));
    }

    #[test]
    fn expired_token_is_rejected() {
        let keys = JwtKeys::new("secret");
        let token = keys.sign(
            Claims {
                user_id: Uuid::now_v7(),
                username: None,
                role: None,
                token_version: 0,
                token_type: None,
                scope: None,
                iat: 0,
                exp: 0,
            },
            -10,
        );
        assert_eq!(keys.decode(&token), Err(InvalidToken));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(JwtKeys::new("s").decode("not.a.jwt"), Err(InvalidToken));
    }
}
