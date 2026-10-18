//! The SQL fixture `fixtures/subsonic.sql` and the literals pasted into it.

mod common;

use common::*;
use kima_core::cipher::Cipher;
use kima_db::PgPool;

/// Prints the literals `fixtures/subsonic.sql` embeds: two argon2 hashes, the token's SHA-256 and
/// its sealed copy. Run it only to regenerate them:
/// `cargo test -p kima-subsonic --test fixture print_fixture_literals -- --ignored --nocapture`
#[test]
#[ignore]
fn print_fixture_literals() {
    println!(
        "admin password_hash = {}",
        kima_core::auth::hash_password(ADMIN_PASSWORD)
    );
    println!(
        "alice password_hash = {}",
        kima_core::auth::hash_password(ALICE_PASSWORD)
    );
    println!(
        "key_hash = {}",
        hex::encode(kima_core::app_token::hash(FIXTURE_TOKEN))
    );
    let cipher = Cipher::from_secret(FIXTURE_CIPHER_SECRET);
    println!(
        "secret_enc = {}",
        hex::encode(kima_core::app_token::seal(
            &cipher,
            ALICE_TOKEN_ID,
            FIXTURE_TOKEN
        ))
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn fixture_credentials_match_their_literals(pool: PgPool) {
    let state = fixture_state(pool.clone());
    let admin = kima_db::users::find_by_username(&pool, "admin")
        .await
        .unwrap()
        .unwrap();
    assert!(kima_core::auth::verify_password(
        ADMIN_PASSWORD,
        &admin.password_hash
    ));
    assert_eq!(admin.role, "admin");
    let alice = kima_db::users::find_by_username(&pool, "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(kima_core::auth::verify_password(
        ALICE_PASSWORD,
        &alice.password_hash
    ));
    assert_eq!(alice.id, ALICE_ID);

    // The sealed copy opens to the token, and the lookup hash finds the row.
    let secrets = kima_db::api_keys::secrets_for_user(&pool, alice.id)
        .await
        .unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].0, ALICE_TOKEN_ID);
    let opened = kima_core::app_token::open(&state.cipher, secrets[0].0, &secrets[0].1).unwrap();
    assert_eq!(opened, FIXTURE_TOKEN);
    let found = kima_db::api_keys::find_by_hash(&pool, &kima_core::app_token::hash(FIXTURE_TOKEN))
        .await
        .unwrap();
    assert!(found.is_some());
}
