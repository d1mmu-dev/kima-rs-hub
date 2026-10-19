//! User management (spec §4.8 #52-#57): the `subsonicAuth2fa.test.ts` port and rulings R-A and
//! R-C (spec §10.2). App tokens never manage users or change passwords (D-098), and every
//! password change revokes the target's app tokens (D-097).

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_api::AppState;
use kima_core::auth::verify_password;
use kima_db::PgPool;
use kima_db::users::{self, ROLE_ADMIN, ROLE_USER, User};
use serde_json::{Value, json};

const ROOT_PASSWORD: &str = "rootpass1";
const BOB_PASSWORD: &str = "bobpass12";
const SECOND_ADMIN_PASSWORD: &str = "annpass12";
/// The advisory lock `kima_db::users` serialises role changes and deletions under.
const REGISTRATION_LOCK: i64 = 0x6b69_6d61_0000_0001;

const TOKEN_MESSAGE: &str = "This needs your account password, not an app key.";
const CHANGE_MESSAGE: &str = "Change your password on the web, not with an app key.";

/// How a request proves who it is.
#[derive(Clone)]
enum Auth {
    Password(String, String),
    ApiKey(String),
    TokenSalt(String, String),
    TokenAsP(String, String),
}

impl Auth {
    fn params(&self) -> Vec<(String, String)> {
        let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        match self {
            Self::Password(u, p) => vec![pair("u", u), pair("p", p)],
            Self::ApiKey(key) => vec![pair("apiKey", key)],
            Self::TokenSalt(u, token) => {
                let mut out = vec![pair("u", u)];
                out.extend(ts_params(token));
                out
            }
            Self::TokenAsP(u, token) => vec![pair("u", u), pair("p", token)],
        }
    }

    /// The three ways an app token can be sent.
    fn token_modes(user: &User, token: &str) -> [Auth; 3] {
        [
            Self::ApiKey(token.to_owned()),
            Self::TokenSalt(user.username.clone(), token.to_owned()),
            Self::TokenAsP(user.username.clone(), token.to_owned()),
        ]
    }
}

struct Setup {
    server: TestServer,
    state: AppState,
    root: User,
    bob: User,
}

impl Setup {
    async fn new(pool: PgPool) -> Self {
        let (server, state) = server(pool);
        let root = user(&state, "root", ROOT_PASSWORD, ROLE_ADMIN).await;
        let bob = user(&state, "bob", BOB_PASSWORD, ROLE_USER).await;
        Self {
            server,
            state,
            root,
            bob,
        }
    }

    fn root_password(&self) -> Auth {
        Auth::Password("root".into(), ROOT_PASSWORD.into())
    }

    fn bob_password(&self) -> Auth {
        Auth::Password("bob".into(), BOB_PASSWORD.into())
    }

    async fn call(&self, endpoint: &str, auth: &Auth, params: &[(&str, &str)]) -> Value {
        call(&self.server, endpoint, auth, params).await
    }

    async fn find(&self, name: &str) -> Option<User> {
        users::find_by_username(&self.state.pool, name)
            .await
            .unwrap()
    }

    async fn tokens_of(&self, user: &User) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
    }
}

async fn call(server: &TestServer, endpoint: &str, auth: &Auth, params: &[(&str, &str)]) -> Value {
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_param("f", "json")
        .add_query_params(auth.params())
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
}

#[track_caller]
fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

#[track_caller]
fn failed(res: &Value, code: u64, message: &str) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
    assert_eq!(res["error"]["message"], message, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_user_self_and_other_needs_admin_50(pool: PgPool) {
    let s = Setup::new(pool).await;
    let (_, bob_token) = app_token(&s.state, &s.bob, "Phone").await;
    let bob = Auth::ApiKey(bob_token);

    let res = s.call("getUser", &bob, &[]).await;
    ok(&res);
    assert_eq!(
        res["user"],
        json!({
            "username": "bob",
            "scrobblingEnabled": true,
            "adminRole": false,
            "settingsRole": true,
            "downloadRole": true,
            "uploadRole": false,
            "playlistRole": true,
            "coverArtRole": false,
            "commentRole": false,
            "podcastRole": false,
            "streamRole": true,
            "jukeboxRole": false,
            "shareRole": false,
            "folder": [1],
        })
    );
    // Naming yourself, in any case, is still yourself.
    let res = s.call("getUser", &bob, &[("username", "BOB")]).await;
    ok(&res);
    assert_eq!(res["user"]["username"], "bob");

    let res = s.call("getUser", &bob, &[("username", "root")]).await;
    failed(&res, 50, "Admin privileges required");

    let root = s.root_password();
    let res = s.call("getUser", &root, &[("username", "bob")]).await;
    ok(&res);
    assert_eq!(res["user"]["username"], "bob");
    let res = s.call("getUser", &root, &[]).await;
    assert_eq!(res["user"]["adminRole"], true, "{res}");
    let res = s.call("getUser", &root, &[("username", "nobody")]).await;
    failed(&res, 70, "User not found");

    let xml = rest_xml(&s.server, "getUser", &[("u", "bob"), ("p", BOB_PASSWORD)]).await;
    assert!(
        xml.contains(r#"<user username="bob" scrobblingEnabled="true" adminRole="false""#),
        "{xml}"
    );
    assert!(xml.contains("<folder>1</folder></user>"), "{xml}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_users_admin_only_ordered_by_username(pool: PgPool) {
    let s = Setup::new(pool).await;
    user(&s.state, "zed", "zedpass12", ROLE_USER).await;
    user(&s.state, "carol", "carolpass1", ROLE_USER).await;

    let res = s.call("getUsers", &s.bob_password(), &[]).await;
    failed(&res, 50, "Admin privileges required");

    // Reading is not managing: the admin's app token may list users.
    let (_, root_token) = app_token(&s.state, &s.root, "Phone").await;
    let res = s.call("getUsers", &Auth::ApiKey(root_token), &[]).await;
    ok(&res);
    let names: Vec<&str> = res["users"]["user"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bob", "carol", "root", "zed"]);
    assert_eq!(res["users"]["user"][2]["adminRole"], true);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn self_change_password_with_token_50_unchanged(pool: PgPool) {
    let s = Setup::new(pool).await;
    let (_, token) = app_token(&s.state, &s.bob, "Phone").await;
    for auth in Auth::token_modes(&s.bob, &token) {
        let res = s
            .call(
                "changePassword",
                &auth,
                &[("username", "bob"), ("password", "hijacked1")],
            )
            .await;
        failed(&res, 50, CHANGE_MESSAGE);
    }
    let after = s.find("bob").await.unwrap();
    assert_eq!(after.password_hash, s.bob.password_hash);
    assert_eq!(after.token_version, s.bob.token_version);
    assert_eq!(s.tokens_of(&s.bob).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn self_change_password_with_password_works(pool: PgPool) {
    let s = Setup::new(pool).await;
    let res = s
        .call(
            "changePassword",
            &s.bob_password(),
            &[("username", "bob"), ("password", "brandnew1")],
        )
        .await;
    ok(&res);
    let after = s.find("bob").await.unwrap();
    assert!(verify_password("brandnew1", &after.password_hash));
    assert_eq!(after.token_version, s.bob.token_version + 1);

    // The old password no longer signs in; the new one does.
    let res = s.call("ping", &s.bob_password(), &[]).await;
    failed(&res, 40, "Wrong username or password");
    let res = s
        .call(
            "ping",
            &Auth::Password("bob".into(), "brandnew1".into()),
            &[],
        )
        .await;
    ok(&res);

    // Too short is refused, like the web form.
    let res = s
        .call(
            "changePassword",
            &Auth::Password("bob".into(), "brandnew1".into()),
            &[("username", "bob"), ("password", "abc")],
        )
        .await;
    assert_eq!(res["error"]["code"], 10, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_token_cannot_manage_users(pool: PgPool) {
    let s = Setup::new(pool).await;
    let (_, token) = app_token(&s.state, &s.root, "Phone").await;
    for auth in Auth::token_modes(&s.root, &token) {
        let res = s
            .call(
                "createUser",
                &auth,
                &[
                    ("username", "eve"),
                    ("password", "evepass12"),
                    ("email", "eve@example.com"),
                ],
            )
            .await;
        failed(&res, 50, TOKEN_MESSAGE);
        let res = s
            .call(
                "updateUser",
                &auth,
                &[("username", "bob"), ("adminRole", "true")],
            )
            .await;
        failed(&res, 50, TOKEN_MESSAGE);
        let res = s
            .call(
                "updateUser",
                &auth,
                &[("username", "bob"), ("password", "hijacked1")],
            )
            .await;
        failed(&res, 50, TOKEN_MESSAGE);
        let res = s.call("deleteUser", &auth, &[("username", "bob")]).await;
        failed(&res, 50, TOKEN_MESSAGE);
        let res = s
            .call(
                "changePassword",
                &auth,
                &[("username", "bob"), ("password", "hijacked1")],
            )
            .await;
        failed(&res, 50, CHANGE_MESSAGE);
    }
    assert!(s.find("eve").await.is_none());
    let bob = s.find("bob").await.expect("bob still present");
    assert_eq!(
        bob, s.bob,
        "role, password hash and token version unchanged"
    );
    assert_eq!(s.find("root").await.unwrap(), s.root);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_password_auth_manages_users(pool: PgPool) {
    let s = Setup::new(pool).await;
    let root = s.root_password();

    let res = s
        .call(
            "createUser",
            &root,
            &[
                ("username", "eve"),
                ("password", "evepass12"),
                ("email", "eve@example.com"),
                ("streamRole", "false"),
            ],
        )
        .await;
    ok(&res);
    let eve = s.find("eve").await.unwrap();
    assert_eq!(eve.role, ROLE_USER);
    assert!(eve.onboarding_complete);
    assert!(verify_password("evepass12", &eve.password_hash));

    let res = s
        .call(
            "updateUser",
            &root,
            &[("username", "eve"), ("adminRole", "true")],
        )
        .await;
    ok(&res);
    assert_eq!(s.find("eve").await.unwrap().role, ROLE_ADMIN);

    let res = s
        .call(
            "updateUser",
            &root,
            &[
                ("username", "eve"),
                ("adminRole", "false"),
                ("password", "evenew123"),
            ],
        )
        .await;
    ok(&res);
    let after = s.find("eve").await.unwrap();
    assert_eq!(after.role, ROLE_USER);
    assert!(verify_password("evenew123", &after.password_hash));
    assert_eq!(after.token_version, eve.token_version + 1);

    let res = s
        .call(
            "changePassword",
            &root,
            &[("username", "bob"), ("password", "rootset12")],
        )
        .await;
    ok(&res);
    assert!(verify_password(
        "rootset12",
        &s.find("bob").await.unwrap().password_hash
    ));

    let res = s.call("deleteUser", &root, &[("username", "eve")]).await;
    ok(&res);
    assert!(s.find("eve").await.is_none());

    let res = s.call("deleteUser", &root, &[("username", "eve")]).await;
    failed(&res, 70, "User not found");
    let res = s
        .call(
            "updateUser",
            &root,
            &[("username", "eve"), ("adminRole", "true")],
        )
        .await;
    failed(&res, 70, "User not found");
    let res = s
        .call(
            "changePassword",
            &root,
            &[("username", "eve"), ("password", "whatever1")],
        )
        .await;
    failed(&res, 70, "User not found");

    // A non-admin with the password is still not an admin.
    let bob = Auth::Password("bob".into(), "rootset12".into());
    let res = s
        .call(
            "createUser",
            &bob,
            &[("username", "frank"), ("password", "frankpass1")],
        )
        .await;
    failed(&res, 50, "Admin privileges required");
    let res = s
        .call(
            "changePassword",
            &bob,
            &[("username", "root"), ("password", "hijacked1")],
        )
        .await;
    failed(&res, 50, "Admin privileges required");
    assert_eq!(s.find("root").await.unwrap(), s.root);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_user_admin_role_via_token_no_login(pool: PgPool) {
    let s = Setup::new(pool).await;
    let (_, token) = app_token(&s.state, &s.root, "Phone").await;
    let res = s
        .call(
            "createUser",
            &Auth::ApiKey(token),
            &[
                ("username", "mallory"),
                ("password", "mallorypw1"),
                ("adminRole", "true"),
            ],
        )
        .await;
    failed(&res, 50, TOKEN_MESSAGE);
    assert!(s.find("mallory").await.is_none());

    let peer: SocketAddr = "10.0.0.2:4000".parse().unwrap();
    let api =
        TestServer::new(kima_api::router(s.state.clone()).layer(MockConnectInfo(peer))).unwrap();
    let res = api
        .post("/api/auth/login")
        .json(&json!({ "username": "mallory", "password": "mallorypw1" }))
        .await;
    res.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

/// Every way the old token could be sent now fails: 44 for `apiKey`, 40 for the others.
async fn assert_token_dead(s: &Setup, owner: &User, token: &str) {
    let [api_key, ts, as_p] = Auth::token_modes(owner, token);
    failed(&s.call("ping", &api_key, &[]).await, 44, "Invalid API key");
    let res = s.call("ping", &ts, &[]).await;
    assert_eq!(res["error"]["code"], 40, "{res}");
    let res = s.call("ping", &as_p, &[]).await;
    assert_eq!(res["error"]["code"], 40, "{res}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_change_revokes_tokens(pool: PgPool) {
    let s = Setup::new(pool).await;
    let carol = user(&s.state, "carol", "carolpass1", ROLE_USER).await;
    let (_, carol_token) = app_token(&s.state, &carol, "Phone").await;

    // Subsonic changePassword, as bob with his password.
    let (_, phone) = app_token(&s.state, &s.bob, "Phone").await;
    let (_, tablet) = app_token(&s.state, &s.bob, "Tablet").await;
    assert_eq!(s.tokens_of(&s.bob).await, 2);
    let res = s
        .call(
            "changePassword",
            &s.bob_password(),
            &[("username", "bob"), ("password", "bobnew123")],
        )
        .await;
    ok(&res);
    assert_eq!(s.tokens_of(&s.bob).await, 0);
    assert_token_dead(&s, &s.bob, &phone).await;
    assert_token_dead(&s, &s.bob, &tablet).await;

    // updateUser password=…, as the admin with the password.
    let bob = s.find("bob").await.unwrap();
    let (_, laptop) = app_token(&s.state, &bob, "Laptop").await;
    let res = s
        .call(
            "updateUser",
            &s.root_password(),
            &[("username", "bob"), ("password", "bobnewer1")],
        )
        .await;
    ok(&res);
    assert_eq!(s.tokens_of(&bob).await, 0);
    assert_token_dead(&s, &bob, &laptop).await;

    // Another user's tokens are untouched.
    assert_eq!(s.tokens_of(&carol).await, 1);
    for auth in Auth::token_modes(&carol, &carol_token) {
        ok(&s.call("ping", &auth, &[]).await);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_user_cannot_remove_own_admin_50(pool: PgPool) {
    let s = Setup::new(pool).await;
    let res = s
        .call(
            "updateUser",
            &s.root_password(),
            &[("username", "root"), ("adminRole", "false")],
        )
        .await;
    failed(&res, 50, "Cannot remove your own admin role");
    assert_eq!(s.find("root").await.unwrap().role, ROLE_ADMIN);
}

/// Runs `first` and `second` while the test holds the users lock, and releases it only once both
/// have authenticated (as admins) and are waiting on it. Returns both answers.
async fn race_under_lock(
    s: &Setup,
    first: (&str, Auth, Vec<(&str, &str)>),
    second: (&str, Auth, Vec<(&str, &str)>),
) -> (Value, Value) {
    let mut holder = s.state.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK)
        .execute(&mut *holder)
        .await
        .unwrap();
    let release = async {
        for attempt in 0.. {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks l JOIN pg_database d ON d.oid = l.database
                 WHERE l.locktype = 'advisory' AND NOT l.granted
                   AND d.datname = current_database()",
            )
            .fetch_one(&s.state.pool)
            .await
            .unwrap();
            if waiting == 2 {
                break;
            }
            assert!(attempt < 1500, "both requests never reached the lock");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        holder.commit().await.unwrap();
    };
    let (a, b, ()) = tokio::join!(
        call(&s.server, first.0, &first.1, &first.2),
        call(&s.server, second.0, &second.1, &second.2),
        release
    );
    (a, b)
}

/// Exactly one of the two answers is ok and the other is 50 `message`.
#[track_caller]
fn one_ok_one_refused(a: &Value, b: &Value, message: &str) {
    let (refused, passed) = if a["status"] == "ok" { (b, a) } else { (a, b) };
    ok(passed);
    failed(refused, 50, message);
}

async fn admin_count(s: &Setup) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'admin'")
        .fetch_one(&s.state.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_user_last_admin_50(pool: PgPool) {
    let s = Setup::new(pool).await;
    user(&s.state, "ann", SECOND_ADMIN_PASSWORD, ROLE_ADMIN).await;
    let ann = Auth::Password("ann".into(), SECOND_ADMIN_PASSWORD.into());
    // Two admins demote each other at once: both pass the admin check, but only one demotion
    // can land, or the server would be left without an admin.
    let (a, b) = race_under_lock(
        &s,
        (
            "updateUser",
            s.root_password(),
            vec![("username", "ann"), ("adminRole", "false")],
        ),
        (
            "updateUser",
            ann,
            vec![("username", "root"), ("adminRole", "false")],
        ),
    )
    .await;
    one_ok_one_refused(&a, &b, "Cannot remove the last admin");
    assert_eq!(admin_count(&s).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_user_self_50_last_admin_50(pool: PgPool) {
    let s = Setup::new(pool).await;
    let res = s
        .call("deleteUser", &s.root_password(), &[("username", "root")])
        .await;
    failed(&res, 50, "Cannot delete your own account");
    assert!(s.find("root").await.is_some());

    user(&s.state, "ann", SECOND_ADMIN_PASSWORD, ROLE_ADMIN).await;
    let ann = Auth::Password("ann".into(), SECOND_ADMIN_PASSWORD.into());
    let (a, b) = race_under_lock(
        &s,
        ("deleteUser", s.root_password(), vec![("username", "ann")]),
        ("deleteUser", ann, vec![("username", "root")]),
    )
    .await;
    one_ok_one_refused(&a, &b, "Cannot delete the last admin");
    assert_eq!(admin_count(&s).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_user_enc_password_and_duplicate_0(pool: PgPool) {
    let s = Setup::new(pool).await;
    let root = s.root_password();
    let enc = format!("enc:{}", hex::encode("davepass1"));
    let res = s
        .call(
            "createUser",
            &root,
            &[("username", "dave"), ("password", &enc), ("adminRole", "1")],
        )
        .await;
    ok(&res);
    let dave = s.find("dave").await.unwrap();
    assert_eq!(dave.role, ROLE_ADMIN);
    ok(&s
        .call(
            "ping",
            &Auth::Password("dave".into(), "davepass1".into()),
            &[],
        )
        .await);

    let res = s
        .call(
            "createUser",
            &root,
            &[("username", "DAVE"), ("password", "otherpass1")],
        )
        .await;
    failed(&res, 0, "Username already exists");

    let res = s
        .call(
            "createUser",
            &root,
            &[("username", "erin"), ("password", "abc")],
        )
        .await;
    assert_eq!(res["error"]["code"], 10, "{res}");
    let res = s
        .call("createUser", &root, &[("password", "erinpass1")])
        .await;
    failed(&res, 10, "Required parameter is missing: username");
    let res = s.call("createUser", &root, &[("username", "erin")]).await;
    failed(&res, 10, "Required parameter is missing: password");
    assert!(s.find("erin").await.is_none());
}

/// Self is decided by user id after the database lookup, never by comparing names in Rust. Rust
/// lowercases `İ` (U+0130) to `i` + U+0307, the database (en_US) to plain `i`. So `İvy` folds to
/// the caller `i̇vy` in Rust, but the database resolves it to another user, `ivy`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn self_check_uses_the_resolved_user_not_name_folding(pool: PgPool) {
    let s = Setup::new(pool).await;
    let caller_name = "i\u{307}vy";
    user(&s.state, caller_name, "callerpass1", ROLE_USER).await;
    let victim = user(&s.state, "ivy", "victimpass1", ROLE_USER).await;
    let probe = "\u{130}vy";
    assert_eq!(probe.to_lowercase(), caller_name.to_lowercase());
    assert_eq!(
        s.find(probe).await.map(|u| u.id),
        Some(victim.id),
        "the database resolves the probe to the victim"
    );

    let caller = Auth::Password(caller_name.into(), "callerpass1".into());
    let res = s
        .call(
            "changePassword",
            &caller,
            &[("username", probe), ("password", "hijacked1")],
        )
        .await;
    failed(&res, 50, "Admin privileges required");
    let res = s.call("getUser", &caller, &[("username", probe)]).await;
    failed(&res, 50, "Admin privileges required");
    assert_eq!(s.find("ivy").await.unwrap(), victim);

    // Their own name still works.
    let res = s
        .call("getUser", &caller, &[("username", caller_name)])
        .await;
    ok(&res);
    assert_eq!(res["user"]["username"], caller_name);
}

/// A non-admin learns nothing about other names: unknown and known both give 50, and the
/// admin check comes before the password checks.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_admin_gets_50_for_any_other_name(pool: PgPool) {
    let s = Setup::new(pool).await;
    let bob = s.bob_password();
    for name in ["root", "nobody"] {
        let res = s.call("getUser", &bob, &[("username", name)]).await;
        failed(&res, 50, "Admin privileges required");
        for password in ["abc", "longenough1"] {
            let res = s
                .call(
                    "changePassword",
                    &bob,
                    &[("username", name), ("password", password)],
                )
                .await;
            failed(&res, 50, "Admin privileges required");
        }
        let res = s.call("changePassword", &bob, &[("username", name)]).await;
        failed(&res, 50, "Admin privileges required");
    }
    assert_eq!(s.find("root").await.unwrap(), s.root);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_admin_password_cannot_update_or_delete(pool: PgPool) {
    let s = Setup::new(pool).await;
    let carol = user(&s.state, "carol", "carolpass1", ROLE_USER).await;
    let bob = s.bob_password();
    for params in [
        vec![("username", "carol"), ("adminRole", "true")],
        vec![("username", "carol"), ("password", "hijacked1")],
        vec![("username", "bob"), ("adminRole", "true")],
    ] {
        let res = s.call("updateUser", &bob, &params).await;
        failed(&res, 50, "Admin privileges required");
    }
    let res = s.call("deleteUser", &bob, &[("username", "carol")]).await;
    failed(&res, 50, "Admin privileges required");
    assert_eq!(s.find("carol").await.unwrap(), carol);
    assert_eq!(s.find("bob").await.unwrap(), s.bob);
    assert_eq!(s.find("root").await.unwrap(), s.root);
}

/// A password change ends the target's web sessions too: the old access token stops working and
/// their event streams are told (`revoke_sessions`).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_change_revokes_web_sessions(pool: PgPool) {
    let s = Setup::new(pool).await;
    let peer: SocketAddr = "10.0.0.3:4000".parse().unwrap();
    let api =
        TestServer::new(kima_api::router(s.state.clone()).layer(MockConnectInfo(peer))).unwrap();
    let me = async |token: &str| {
        api.get("/api/auth/me")
            .authorization_bearer(token)
            .await
            .status_code()
    };

    for (endpoint, auth, password) in [
        ("changePassword", s.bob_password(), "bobnew123"),
        ("updateUser", s.root_password(), "bobnewer1"),
    ] {
        let bob = s.find("bob").await.unwrap();
        let token = s
            .state
            .jwt
            .issue_access(bob.id, &bob.username, &bob.role, bob.token_version);
        assert_eq!(me(&token).await, axum::http::StatusCode::OK);
        let mut events = s.state.events.subscribe();

        let res = s
            .call(
                endpoint,
                &auth,
                &[("username", "bob"), ("password", password)],
            )
            .await;
        ok(&res);

        assert_eq!(me(&token).await, axum::http::StatusCode::UNAUTHORIZED);
        let event = events.try_recv().expect("a revocation was published");
        assert!(event.is_revocation(), "{event:?}");
        assert_eq!(event.user_id, Some(bob.id));
    }
}

/// `createUser` and password sign-in share one cap (50 characters), so an admin can never
/// create an account that cannot sign in.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_user_caps_username_at_50_characters(pool: PgPool) {
    let s = Setup::new(pool).await;
    let root = s.root_password();
    let fifty = "é".repeat(50);
    ok(&s
        .call(
            "createUser",
            &root,
            &[("username", &fifty), ("password", "longpass1")],
        )
        .await);
    ok(&s
        .call(
            "ping",
            &Auth::Password(fifty.clone(), "longpass1".into()),
            &[],
        )
        .await);

    let fifty_one = "é".repeat(51);
    let res = s
        .call(
            "createUser",
            &root,
            &[("username", &fifty_one), ("password", "longpass1")],
        )
        .await;
    failed(&res, 10, "Username must be at most 50 characters");
    assert!(s.find(&fifty_one).await.is_none());
}
