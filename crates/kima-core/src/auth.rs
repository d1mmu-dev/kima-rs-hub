//! Password hashing and the JWTs the frontend carries (upstream's claims plus an optional
//! `profileId`, D-241).

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profile::{AccountId, ProfileId};

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
    /// The account (upstream's `userId`).
    pub user_id: Uuid,
    /// The profile the token acts as (M-PROF, D-241). Absent = the account's main profile:
    /// every token issued before profiles existed, and every main-profile token since (so a
    /// main-profile token carries exactly upstream's claims).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<Uuid>,
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
    pub fn account(&self) -> AccountId {
        AccountId::new(self.user_id)
    }

    /// The profile the token acts as: `profileId`, or the main profile when it is absent.
    /// Whether that profile belongs to the account is checked against the database, not here.
    pub fn profile(&self) -> ProfileId {
        self.profile_id
            .map_or_else(|| ProfileId::main_of(self.account()), ProfileId::new)
    }

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
        account: AccountId,
        profile: ProfileId,
        username: &str,
        role: &str,
        token_version: i32,
    ) -> String {
        self.sign(
            Claims {
                user_id: account.into_uuid(),
                profile_id: profile_claim(account, profile),
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

    pub fn issue_refresh(
        &self,
        account: AccountId,
        profile: ProfileId,
        token_version: i32,
    ) -> String {
        self.sign(
            Claims {
                user_id: account.into_uuid(),
                profile_id: profile_claim(account, profile),
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

    pub fn issue_stream(
        &self,
        account: AccountId,
        profile: ProfileId,
        token_version: i32,
    ) -> String {
        self.sign(
            Claims {
                user_id: account.into_uuid(),
                profile_id: profile_claim(account, profile),
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

/// The `profileId` claim for `profile`: none for the account's main profile.
fn profile_claim(account: AccountId, profile: ProfileId) -> Option<Uuid> {
    (profile != ProfileId::main_of(account)).then(|| profile.into_uuid())
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
        let account = AccountId::new(id);
        let claims = keys
            .decode(&keys.issue_access(account, ProfileId::main_of(account), "ana", "admin", 3))
            .unwrap();
        assert_eq!(claims.user_id, id);
        assert_eq!(claims.profile_id, None);
        assert_eq!(claims.profile(), ProfileId::main_of(account));
        assert_eq!(claims.username.as_deref(), Some("ana"));
        assert_eq!(claims.role.as_deref(), Some("admin"));
        assert_eq!(claims.token_version, 3);
        assert_eq!(claims.kind(), TokenKind::Access);
        assert_eq!(claims.exp - claims.iat, ACCESS_TTL_SECS);
    }

    #[test]
    fn refresh_and_stream_tokens_have_their_kind() {
        let keys = JwtKeys::new("secret");
        let account = AccountId::new(Uuid::now_v7());
        let main = ProfileId::main_of(account);
        let refresh = keys.decode(&keys.issue_refresh(account, main, 0)).unwrap();
        assert_eq!(refresh.kind(), TokenKind::Refresh);
        assert_eq!(refresh.exp - refresh.iat, REFRESH_TTL_SECS);
        let stream = keys.decode(&keys.issue_stream(account, main, 0)).unwrap();
        assert_eq!(stream.kind(), TokenKind::Stream);
        assert_eq!(stream.exp - stream.iat, STREAM_TTL_SECS);
    }

    #[test]
    fn wire_format_matches_upstream() {
        let keys = JwtKeys::new("secret");
        let account = AccountId::new(Uuid::nil());
        let token = keys.issue_refresh(account, ProfileId::main_of(account), 7);
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
        // A main-profile token carries exactly upstream's claims.
        assert!(json.get("profileId").is_none());
    }

    #[test]
    fn every_token_kind_carries_a_non_main_profile() {
        let keys = JwtKeys::new("secret");
        let account = AccountId::new(Uuid::now_v7());
        let kids = ProfileId::new(Uuid::now_v7());
        for token in [
            keys.issue_access(account, kids, "ana", "user", 1),
            keys.issue_refresh(account, kids, 1),
            keys.issue_stream(account, kids, 1),
        ] {
            let claims = keys.decode(&token).unwrap();
            assert_eq!(claims.account(), account);
            assert_eq!(claims.profile_id, Some(kids.into_uuid()));
            assert_eq!(claims.profile(), kids);
            let payload = token.split('.').nth(1).unwrap();
            use base64::Engine;
            let json: serde_json::Value = serde_json::from_slice(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(payload)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(json["profileId"], kids.to_string());
        }
    }

    #[test]
    fn claims_without_profile_id_mean_the_main_profile() {
        // The claim set of a token signed before profiles existed.
        let id = Uuid::now_v7();
        let claims: Claims = serde_json::from_value(serde_json::json!({
            "userId": id, "username": "ana", "role": "admin", "tokenVersion": 0,
            "iat": 1, "exp": 2,
        }))
        .unwrap();
        assert_eq!(claims.profile(), ProfileId::main_of(AccountId::new(id)));
    }

    #[test]
    fn other_secret_is_rejected() {
        let account = AccountId::new(Uuid::now_v7());
        let token =
            JwtKeys::new("a").issue_access(account, ProfileId::main_of(account), "u", "user", 0);
        assert_eq!(JwtKeys::new("b").decode(&token), Err(InvalidToken));
    }

    #[test]
    fn expired_token_is_rejected() {
        let keys = JwtKeys::new("secret");
        let token = keys.sign(
            Claims {
                user_id: Uuid::now_v7(),
                profile_id: None,
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
