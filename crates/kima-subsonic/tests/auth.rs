mod common;

use axum_test::TestServer;
use common::{app_token, rest, server, ts_params, user};
use kima_db::PgPool;
use kima_db::users::{ROLE_ADMIN, ROLE_USER};
use serde_json::{Value, json};

const HINT: &str = "Wrong username or password. Token authentication needs an app token from \
                    Settings > Native Apps; your account password cannot be used with it.";

async fn ping(server: &TestServer, params: &[(&str, &str)]) -> Value {
    rest(server, "ping", params).await
}

async fn ping_owned(server: &TestServer, params: &[(String, String)]) -> Value {
    let refs: Vec<(&str, &str)> = params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    ping(server, &refs).await
}

/// `u` plus the `t`/`s` pair for `secret`.
async fn ping_ts(server: &TestServer, u: &str, secret: &str) -> Value {
    let mut params = vec![("u".to_owned(), u.to_owned())];
    params.extend(ts_params(secret));
    ping_owned(server, &params).await
}

fn assert_ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

fn assert_error(res: &Value, code: u16, message: &str) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
    assert_eq!(res["error"]["message"], message, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_alone_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    assert_ok(&ping(&server, &[("apiKey", &token)]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_with_matching_u_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    assert_ok(&ping(&server, &[("apiKey", &token), ("u", "alice")]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_with_other_u_40(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    user(&state, "bob", "pw-bob", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    let res = ping(&server, &[("apiKey", &token), ("u", "bob")]).await;
    assert_error(&res, 40, "Wrong username or password");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_with_p_43(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    let res = ping(&server, &[("apiKey", &token), ("u", "alice"), ("p", "x")]).await;
    assert_error(
        &res,
        43,
        "Multiple conflicting authentication mechanisms provided",
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_with_t_43(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    let mut params = vec![
        ("apiKey".to_owned(), token.clone()),
        ("u".to_owned(), "alice".to_owned()),
    ];
    params.extend(ts_params(&token));
    let res = ping_owned(&server, &params).await;
    assert_eq!(res["error"]["code"], 43, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_key_unknown_44(pool: PgPool) {
    let (server, _) = server(pool);
    let res = ping(&server, &[("apiKey", "nope")]).await;
    assert_error(&res, 44, "Invalid API key");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_u_10(pool: PgPool) {
    let (server, _) = server(pool);
    let res = ping(&server, &[("p", "x")]).await;
    assert_error(&res, 10, "Required parameter is missing: u");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ts_with_app_token_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    app_token(&state, &alice, "other").await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    assert_ok(&ping_ts(&server, "alice", &token).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ts_wrong_40_with_hint_message(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    app_token(&state, &alice, "phone").await;
    assert_error(&ping_ts(&server, "alice", "not-a-token").await, 40, HINT);
    assert_error(&ping_ts(&server, "nobody", "not-a-token").await, 40, HINT);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ts_with_account_password_40(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    assert_error(&ping_ts(&server, "alice", "pw-alice").await, 40, HINT);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ts_missing_s_10(pool: PgPool) {
    let (server, _) = server(pool);
    let res = ping(
        &server,
        &[("u", "alice"), ("t", "26719a1196d2a940705a59634eb18eab")],
    )
    .await;
    assert_error(&res, 10, "Required parameter is missing: s");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ts_bad_t_40(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    let res = ping(&server, &[("u", "alice"), ("t", "zz"), ("s", "abc")]).await;
    assert_eq!(res["error"]["code"], 40, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn p_app_token_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    assert_ok(&ping(&server, &[("u", "alice"), ("p", &token)]).await);
    assert_eq!(state.password_permits_issued(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn p_enc_app_token_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    let enc = format!("enc:{}", hex::encode(&token));
    assert_ok(&ping(&server, &[("u", "alice"), ("p", &enc)]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn p_other_users_token_40(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    let bob = user(&state, "bob", "pw-bob", ROLE_USER).await;
    let (_, token) = app_token(&state, &bob, "phone").await;
    let res = ping(&server, &[("u", "alice"), ("p", &token)]).await;
    assert_error(&res, 40, "Wrong username or password");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn p_password_ok(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    assert_ok(&ping(&server, &[("u", "alice"), ("p", "pw-alice")]).await);
    let enc = format!("enc:{}", hex::encode("pw-alice"));
    assert_ok(&ping(&server, &[("u", "alice"), ("p", &enc)]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn p_neither_40(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    let wrong = "Wrong username or password";
    assert_error(
        &ping(&server, &[("u", "alice"), ("p", "x")]).await,
        40,
        wrong,
    );
    assert_error(
        &ping(&server, &[("u", "nobody"), ("p", "x")]).await,
        40,
        wrong,
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn nothing_usable_10(pool: PgPool) {
    let (server, _) = server(pool);
    let res = ping(&server, &[("u", "alice")]).await;
    assert_error(&res, 10, "Required parameter is missing: p, t or apiKey");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_cache_skips_argon2(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let params = [("u", "alice"), ("p", "pw-alice")];
    assert_ok(&ping(&server, &params).await);
    assert_eq!(state.password_permits_issued(), 1);
    assert_ok(&ping(&server, &params).await);
    assert_eq!(state.password_permits_issued(), 1, "cache hit adds none");
    kima_db::users::bump_token_version(&state.pool, alice.id)
        .await
        .unwrap();
    assert_ok(&ping(&server, &params).await);
    assert_eq!(state.password_permits_issued(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn limiter_blocks_sixth_p_with_41(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    for _ in 0..5 {
        let res = ping(&server, &[("u", "alice"), ("p", "wrong")]).await;
        assert_eq!(res["error"]["code"], 40, "{res}");
    }
    let permits = state.password_permits_issued();
    let res = ping(&server, &[("u", "alice"), ("p", "pw-alice")]).await;
    assert_eq!(res["error"]["code"], 41, "{res}");
    let message = res["error"]["message"].as_str().unwrap();
    let secs = message
        .strip_prefix("Too many failed sign-in attempts. Try again in ")
        .and_then(|rest| rest.strip_suffix(" seconds."))
        .and_then(|n| n.parse::<u32>().ok());
    assert!(secs.is_some_and(|n| n > 0), "{message}");
    assert_eq!(state.password_permits_issued(), permits, "no argon2 ran");
    // App tokens are never blocked.
    assert_ok(&ping(&server, &[("apiKey", &token)]).await);
    assert_ok(&ping_ts(&server, "alice", &token).await);
    assert_ok(&ping(&server, &[("u", "alice"), ("p", &token)]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn username_case_insensitive(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    assert_ok(&ping_ts(&server, "Alice", &token).await);
    assert_ok(&ping(&server, &[("u", "Alice"), ("p", "pw-alice")]).await);
    assert_ok(&ping(&server, &[("apiKey", &token), ("u", "ALICE")]).await);

    for name in ["Alice", "Alice", "Alice", "alice", "alice"] {
        let res = ping(&server, &[("u", name), ("p", "wrong")]).await;
        assert_eq!(res["error"]["code"], 40, "{res}");
    }
    for name in ["Alice", "alice", "ALICE"] {
        let res = ping(&server, &[("u", name), ("p", "pw-alice")]).await;
        assert_eq!(res["error"]["code"], 41, "{name}: {res}");
    }
}

/// Five failures as `noisy`, then one ping with a **wrong** password as the database's own
/// fold of `noisy`, against a fresh limiter. The whole `subsonic-response` is what a client sees.
async fn fold_probe(pool: &PgPool, noisy: &str) -> Value {
    let (server, _state) = server(pool.clone());
    for _ in 0..5 {
        let res = ping(&server, &[("u", noisy), ("p", "wrong")]).await;
        assert_eq!(res["error"]["code"], 40, "{res}");
    }
    let fold: String = sqlx::query_scalar("SELECT lower($1)")
        .bind(noisy)
        .fetch_one(pool)
        .await
        .unwrap();
    ping(&server, &[("u", &fold), ("p", "wrong")]).await
}

/// Spec §5.7: the limiter reveals nothing about whether an account exists. Rust lowercases `İ`
/// to `i` + U+0307, Postgres under `en_US` to plain `i` (under `C` it stays `İ`). Failures as
/// `İvy` must answer the database's fold of that name the same way whether or not `ivy` exists
/// (M2 final review I2).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fold_variant_failures_answer_the_same_for_known_and_unknown_names(pool: PgPool) {
    let unknown = fold_probe(&pool, "\u{130}vy").await;
    let (_, state) = server(pool.clone());
    user(&state, "ivy", "pw-ivy", ROLE_USER).await;
    let known = fold_probe(&pool, "\u{130}vy").await;
    assert_eq!(known, unknown, "the answer must not depend on existence");
    assert_eq!(known["error"]["code"], 41, "{known}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undecryptable_secret_fails_ts_only(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (id, token) = app_token(&state, &alice, "phone").await;
    let foreign = kima_core::app_token::seal(&kima_core::cipher::Cipher::random(), id, &token);
    sqlx::query("UPDATE api_keys SET secret_enc = $1 WHERE id = $2")
        .bind(foreign)
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_error(&ping_ts(&server, "alice", &token).await, 40, HINT);
    assert_ok(&ping(&server, &[("apiKey", &token)]).await);
    assert_ok(&ping(&server, &[("u", "alice"), ("p", &token)]).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn last_used_set_on_use(pool: PgPool) {
    let (server, state) = server(pool);
    let alice = user(&state, "alice", "pw-alice", ROLE_USER).await;
    let (_, token) = app_token(&state, &alice, "phone").await;
    let keys = kima_db::api_keys::list_for_user(&state.pool, alice.id)
        .await
        .unwrap();
    assert!(keys[0].last_used_at.is_none());
    assert_ok(&ping_ts(&server, "alice", &token).await);
    for _ in 0..50 {
        let keys = kima_db::api_keys::list_for_user(&state.pool, alice.id)
            .await
            .unwrap();
        if keys[0].last_used_at.is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("last_used_at was never set");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_endpoint_after_auth(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_USER).await;
    let res = rest(&server, "fooBar", &[("u", "alice"), ("p", "pw-alice")]).await;
    assert_error(&res, 0, "Not implemented: /fooBar.view");
    let res = rest(&server, "fooBar", &[]).await;
    assert_eq!(res["error"]["code"], 10, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_license_and_music_folders(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "alice", "pw-alice", ROLE_ADMIN).await;
    let auth = [("u", "alice"), ("p", "pw-alice")];
    let res = rest(&server, "getLicense", &auth).await;
    assert_eq!(
        res["license"],
        json!({"valid": true, "email": "kima@kima", "licenseExpires": "2099-12-31T23:59:59"})
    );
    let res = rest(&server, "getMusicFolders", &auth).await;
    assert_eq!(
        res["musicFolders"],
        json!({"musicFolder": [{"id": 1, "name": "Music"}]})
    );
    assert_eq!(rest(&server, "getLicense", &[]).await["error"]["code"], 10);
}
