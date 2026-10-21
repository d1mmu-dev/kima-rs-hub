//! Subsonic authentication (spec §5.3): `apiKey`, token+salt against app tokens, and the plain
//! or `enc:` password, with the failed-login limiter and the verified-password cache.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use hmac::{Hmac, KeyInit, Mac};
use kima_api::auth::{AuthUser, ClientIp};
use kima_core::cipher::Cipher;
use kima_db::users::{self, MAX_USERNAME_CHARS, ROLE_ADMIN, User};
use md5::{Digest, Md5};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;
use uuid::Uuid;

use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::params::{Params, decode_password};

/// Decrypt-and-md5 rounds of every token+salt check, whatever the user holds.
pub const TS_ROUNDS: usize = 50;
pub const PASSWORD_CACHE_TTL: Duration = Duration::from_secs(600);
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(60);
pub const DECRYPT_WARN_INTERVAL: Duration = Duration::from_secs(3600);
const DUMMY_AAD: &[u8] = b"api_keys:dummy";

const TS_HINT: &str = "Wrong username or password. Token authentication needs an app token from \
                       Settings > Native Apps; your account password cannot be used with it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    /// An app token: `apiKey`, token+salt, or a token sent as `p=`.
    AppToken(Uuid),
}

/// The signed-in user of a `/rest` request, and how they proved it.
#[derive(Debug, Clone, PartialEq)]
pub struct SubsonicUser {
    pub user: AuthUser,
    pub method: AuthMethod,
}

impl SubsonicUser {
    pub fn require_admin(&self) -> Result<(), SubsonicError> {
        if self.user.role == ROLE_ADMIN {
            Ok(())
        } else {
            Err(SubsonicError::not_authorized("Admin privileges required"))
        }
    }

    /// User-management writes refuse an app token, however it was presented (D-098).
    pub fn require_password_auth(&self) -> Result<(), SubsonicError> {
        match self.method {
            AuthMethod::Password => Ok(()),
            AuthMethod::AppToken(_) => Err(SubsonicError::not_authorized(
                "This needs your account password, not an app key.",
            )),
        }
    }
}

fn auth_user(user: &User) -> AuthUser {
    AuthUser {
        id: user.id,
        username: user.username.clone(),
        role: user.role.clone(),
        token_version: user.token_version,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_md5_hex(t: &str) -> bool {
    t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `t` is `md5(secret + salt)` as hex. The comparison is constant-time and ignores case; `t`
/// that is not 32 hex characters never matches.
pub fn token_matches(secret: &str, salt: &str, t: &str) -> bool {
    let mut md5 = Md5::new();
    md5.update(secret.as_bytes());
    md5.update(salt.as_bytes());
    let expected = hex::encode(md5.finalize());
    let given = t.to_ascii_lowercase();
    constant_time_eq(expected.as_bytes(), given.as_bytes()) & is_md5_hex(t)
}

#[derive(Debug, PartialEq, Eq)]
pub struct TsOutcome {
    pub matched: Option<Uuid>,
    pub rounds: usize,
    /// Tokens whose secret did not decrypt (a changed or lost `SETTINGS_ENCRYPTION_KEY`).
    pub undecryptable: Vec<Uuid>,
}

/// The ciphertext every unknown-user and spare round decrypts.
pub fn make_dummy(cipher: &Cipher) -> Vec<u8> {
    cipher.encrypt(kima_core::app_token::generate().as_bytes(), DUMMY_AAD)
}

/// Always `TS_ROUNDS` decrypt-and-md5 rounds: the real secrets first, then the dummy. A match
/// does not end the loop, so the work does not depend on what the user holds.
pub fn ts_rounds(
    cipher: &Cipher,
    secrets: &[(Uuid, Vec<u8>)],
    dummy: &[u8],
    salt: &str,
    t: &str,
) -> TsOutcome {
    let mut matched = None;
    let mut undecryptable = Vec::new();
    for round in 0..TS_ROUNDS {
        match secrets.get(round) {
            Some((id, blob)) => match kima_core::app_token::open(cipher, *id, blob) {
                Ok(secret) => {
                    if token_matches(&secret, salt, t) {
                        matched.get_or_insert(*id);
                    }
                }
                Err(_) => {
                    undecryptable.push(*id);
                    std::hint::black_box(token_matches("", salt, t));
                }
            },
            None => {
                let secret = cipher
                    .decrypt(dummy, DUMMY_AAD)
                    .map(|plain| String::from_utf8_lossy(&plain).into_owned())
                    .unwrap_or_default();
                // A dummy never authenticates.
                std::hint::black_box(token_matches(&secret, salt, t));
            }
        }
    }
    TsOutcome {
        matched,
        rounds: TS_ROUNDS,
        undecryptable,
    }
}

struct CacheEntry {
    mac: [u8; 32],
    token_version: i32,
    expires_at: Instant,
}

/// Passwords that verified lately, so a client sending `p=` on every request does not cost an
/// argon2 verify each time. One entry per user, holding only an HMAC under a per-process key.
pub struct PasswordCache {
    ttl: Duration,
    key: [u8; 32],
    entries: Mutex<HashMap<Uuid, CacheEntry>>,
}

impl PasswordCache {
    pub fn new(ttl: Duration) -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        Self {
            ttl,
            key,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn mac(&self, password: &str) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key)
            .expect("HMAC accepts any key length");
        mac.update(password.as_bytes());
        mac
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, CacheEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn check(&self, user: Uuid, password: &str, token_version: i32, now: Instant) -> bool {
        let entries = self.lock();
        let Some(entry) = entries.get(&user) else {
            return false;
        };
        entry.token_version == token_version
            && entry.expires_at > now
            && self.mac(password).verify_slice(&entry.mac).is_ok()
    }

    pub fn store(&self, user: Uuid, password: &str, token_version: i32, now: Instant) {
        let mac: [u8; 32] = self.mac(password).finalize().into_bytes().into();
        let mut entries = self.lock();
        entries.retain(|_, entry| entry.expires_at > now);
        entries.insert(
            user,
            CacheEntry {
                mac,
                token_version,
                expires_at: now + self.ttl,
            },
        );
    }
}

/// Lets something through once per interval per key.
pub struct Throttle {
    interval: Duration,
    last: Mutex<HashMap<Uuid, Instant>>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Mutex::new(HashMap::new()),
        }
    }

    pub fn allow(&self, key: Uuid, now: Instant) -> bool {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = last.get(&key)
            && now.saturating_duration_since(*at) < self.interval
        {
            return false;
        }
        if last.len() >= 10_000 {
            let interval = self.interval;
            last.retain(|_, at| now.saturating_duration_since(*at) < interval);
        }
        last.insert(key, now);
        true
    }
}

fn blocked(blocked: kima_api::login_limit::Blocked) -> SubsonicError {
    SubsonicError::new(
        41,
        format!(
            "Too many failed sign-in attempts. Try again in {} seconds.",
            blocked.retry_after_secs()
        ),
    )
}

/// A non-empty parameter. An empty `apiKey`, `p`, `t` or `s` counts as absent (D-074).
fn given<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params.get(key).filter(|value| !value.is_empty())
}

impl FromRequestParts<SubsonicCtx> for SubsonicUser {
    type Rejection = SubsonicError;

    async fn from_request_parts(
        parts: &mut Parts,
        ctx: &SubsonicCtx,
    ) -> Result<Self, SubsonicError> {
        let params = Params::from_request_parts(parts, ctx).await?;
        let Ok(ClientIp(ip)) = ClientIp::from_request_parts(parts, ctx).await;
        let now = Instant::now();
        let pool = &ctx.app.pool;

        // 1. apiKey.
        if let Some(api_key) = given(&params, "apiKey") {
            if ["p", "t", "s"].iter().any(|k| given(&params, k).is_some()) {
                return Err(SubsonicError::new(
                    43,
                    "Multiple conflicting authentication mechanisms provided",
                ));
            }
            let owner = kima_db::api_keys::find_by_hash(pool, &kima_core::app_token::hash(api_key))
                .await?
                .ok_or_else(SubsonicError::invalid_api_key)?;
            if let Some(u) = given(&params, "u")
                && u.to_lowercase() != owner.user.username.to_lowercase()
            {
                return Err(SubsonicError::wrong_credentials());
            }
            ctx.token_used(owner.key_id, now);
            return Ok(Self {
                user: auth_user(&owner.user),
                method: AuthMethod::AppToken(owner.key_id),
            });
        }

        // 2. u from here on.
        let u = given(&params, "u").ok_or_else(|| SubsonicError::missing("u"))?;

        // 3. Token + salt.
        if let Some(t) = given(&params, "t") {
            let salt = given(&params, "s").ok_or_else(|| SubsonicError::missing("s"))?;
            if !is_md5_hex(t) {
                return Err(SubsonicError::new(40, TS_HINT));
            }
            // One query whatever the user: no extra round trip for an existing user, so the
            // timing does not reveal which usernames exist (spec §5.3 step 3). Keep it so.
            let (found, secrets) = kima_db::api_keys::user_with_secrets(pool, u).await?;
            let outcome = ts_rounds(&ctx.app.cipher, &secrets, &ctx.dummy_secret, salt, t);
            for id in &outcome.undecryptable {
                if ctx.decrypt_warn.allow(*id, now) {
                    tracing::warn!(key_id = %id, "app token secret cannot be decrypted");
                }
            }
            return match (found, outcome.matched) {
                (Some(user), Some(key_id)) => {
                    ctx.token_used(key_id, now);
                    Ok(Self {
                        user: auth_user(&user),
                        method: AuthMethod::AppToken(key_id),
                    })
                }
                _ => Err(SubsonicError::new(40, TS_HINT)),
            };
        }

        // 4. p=, plain or enc:.
        if let Some(raw) = given(&params, "p") {
            let password = decode_password(raw);
            if let Some(owner) =
                kima_db::api_keys::find_by_hash(pool, &kima_core::app_token::hash(&password))
                    .await?
                && owner.user.username.to_lowercase() == u.to_lowercase()
            {
                ctx.token_used(owner.key_id, now);
                return Ok(Self {
                    user: auth_user(&owner.user),
                    method: AuthMethod::AppToken(owner.key_id),
                });
            }
            return password_auth(ctx, ip, u, password, now).await;
        }

        // 5.
        Err(SubsonicError::missing("p, t or apiKey"))
    }
}

async fn password_auth(
    ctx: &SubsonicCtx,
    ip: IpAddr,
    u: &str,
    password: String,
    now: Instant,
) -> Result<SubsonicUser, SubsonicError> {
    // No account can have a name this long, so refuse it before it becomes a limiter key.
    if u.chars().count() > MAX_USERNAME_CHARS {
        return Err(SubsonicError::wrong_credentials());
    }
    let state = &ctx.app;
    // A blocked typed name is refused before any database work or hashing (no charge here).
    state.login_limit.check(ip, u, now).map_err(blocked)?;
    // The database folds case its own way, Rust another: also count the attempt on the key of
    // the database's fold of the typed name, so no spelling gets a separate budget (as
    // `POST /api/auth/login`). Charged whether or not the account exists (spec §5.7). Counted
    // as failures now, cleared on success; if any key is blocked, none is charged.
    let lookup = users::sign_in_lookup(&state.pool, u).await?;
    let attempts = state
        .login_limit
        .begin_all(ip, &[u, &lookup.fold], now)
        .map_err(blocked)?;
    let found = lookup.user;

    let cached = found.as_ref().is_some_and(|user| {
        ctx.password_cache
            .check(user.id, &password, user.token_version, now)
    });
    let (found, valid) = if cached {
        (found, true)
    } else {
        let permit = state
            .password_permit()
            .await
            .map_err(SubsonicError::internal)?;
        let to_verify = password.clone();
        tokio::task::spawn_blocking(move || {
            // Released when the verify ends, even if the client hung up.
            let _permit = permit;
            let valid = match &found {
                Some(user) => kima_core::auth::verify_password(&to_verify, &user.password_hash),
                None => {
                    kima_core::auth::verify_dummy(&to_verify);
                    false
                }
            };
            (found, valid)
        })
        .await
        .map_err(SubsonicError::internal)?
    };

    let Some(user) = found.filter(|_| valid) else {
        return Err(SubsonicError::wrong_credentials());
    };
    for attempt in attempts {
        attempt.success();
    }
    if !cached {
        ctx.password_cache
            .store(user.id, &password, user.token_version, Instant::now());
    }
    Ok(SubsonicUser {
        user: auth_user(&user),
        method: AuthMethod::Password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_subsonic_doc_vector() {
        let t = "26719a1196d2a940705a59634eb18eab";
        assert!(token_matches("sesame", "c19b2d", t));
        assert!(token_matches("sesame", "c19b2d", &t.to_uppercase()));
        assert!(!token_matches("sesame", "c19b2d", &t[..31]));
        assert!(!token_matches("sesame", "c19b2e", t));
        assert!(!token_matches("sesame", "c19b2d", ""));
    }

    fn sealed(cipher: &Cipher, token: &str) -> (Uuid, Vec<u8>) {
        let id = Uuid::now_v7();
        (id, kima_core::app_token::seal(cipher, id, token))
    }

    fn t_for(secret: &str, salt: &str) -> String {
        hex::encode(Md5::digest(format!("{secret}{salt}")))
    }

    #[test]
    fn ts_rounds_constant() {
        let cipher = Cipher::random();
        let dummy = make_dummy(&cipher);
        let t = t_for("tok-0", "salt");
        for n in [0, 1, 50] {
            let secrets: Vec<_> = (0..n)
                .map(|i| sealed(&cipher, &format!("tok-{i}")))
                .collect();
            let out = ts_rounds(&cipher, &secrets, &dummy, "salt", &t);
            assert_eq!(out.rounds, 50, "n={n}");
            assert_eq!(out.matched, secrets.first().map(|(id, _)| *id), "n={n}");
            assert!(out.undecryptable.is_empty());
        }
        let secrets: Vec<_> = (0..50)
            .map(|i| sealed(&cipher, &format!("tok-{i}")))
            .collect();
        let last = t_for("tok-49", "salt");
        let out = ts_rounds(&cipher, &secrets, &dummy, "salt", &last);
        assert_eq!(out.matched, Some(secrets[49].0));
        let out = ts_rounds(&cipher, &secrets, &dummy, "salt", &t_for("nope", "salt"));
        assert_eq!((out.matched, out.rounds), (None, 50));

        let other = Cipher::random();
        let foreign = sealed(&other, "tok-0");
        let out = ts_rounds(&cipher, std::slice::from_ref(&foreign), &dummy, "salt", &t);
        assert_eq!(out.undecryptable, vec![foreign.0]);
        assert_eq!((out.matched, out.rounds), (None, 50));
    }

    #[test]
    fn password_cache_hit_expiry_and_version() {
        let cache = PasswordCache::new(Duration::from_secs(600));
        let (user, now) = (Uuid::now_v7(), Instant::now());
        assert!(!cache.check(user, "pw", 1, now));
        cache.store(user, "pw", 1, now);
        assert!(cache.check(user, "pw", 1, now + Duration::from_secs(599)));
        assert!(!cache.check(user, "other", 1, now));
        assert!(!cache.check(user, "pw", 2, now), "version bump invalidates");
        assert!(!cache.check(user, "pw", 1, now + Duration::from_secs(600)));
        assert!(!cache.check(Uuid::now_v7(), "pw", 1, now), "per user");
        cache.store(user, "new", 2, now);
        assert!(!cache.check(user, "pw", 1, now), "one entry per user");
        assert!(cache.check(user, "new", 2, now));
    }

    #[test]
    fn throttle_once_per_interval() {
        let throttle = Throttle::new(Duration::from_secs(60));
        let (key, now) = (Uuid::now_v7(), Instant::now());
        assert!(throttle.allow(key, now));
        assert!(!throttle.allow(key, now + Duration::from_secs(59)));
        assert!(throttle.allow(Uuid::now_v7(), now));
        assert!(throttle.allow(key, now + Duration::from_secs(60)));
    }
}
