//! M-PROF Task 3: every personal query is keyed by the profile, not the account. Each test
//! writes as a non-main profile, because on the main profile (whose id is the account id) a
//! query that confuses the two still gives the right answer.

use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use kima_db::{api_keys, playlists, podcasts};
use time::OffsetDateTime;
use uuid::Uuid;

async fn account(pool: &PgPool, name: &str) -> AccountId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    AccountId::new(id)
}

async fn kids(pool: &PgPool, account: AccountId, name: &str) -> ProfileId {
    profiles::create(
        pool,
        account,
        NewProfile {
            name: name.to_string(),
            kind: ProfileKind::Kids,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
    .id
}

fn names(list: &[playlists::PlaylistSummary]) -> Vec<&str> {
    let mut n: Vec<&str> = list.iter().map(|s| s.playlist.name.as_str()).collect();
    n.sort();
    n
}

async fn seen_by_kid_ids(pool: &PgPool, kid: ProfileId, name: &str) -> Uuid {
    playlists::list(pool, kid, 50, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.playlist.name == name)
        .unwrap()
        .playlist
        .id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playlists_list_shows_sibling_public_not_private(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    playlists::create(&pool, Uuid::now_v7(), kid, "kid public", true)
        .await
        .unwrap();
    playlists::create(&pool, Uuid::now_v7(), kid, "kid private", false)
        .await
        .unwrap();
    playlists::create(&pool, Uuid::now_v7(), main, "main private", false)
        .await
        .unwrap();

    let seen_by_main = playlists::list(&pool, main, 50, 0).await.unwrap();
    assert_eq!(names(&seen_by_main), vec!["kid public", "main private"]);
    let kid_public = seen_by_main
        .iter()
        .find(|s| s.playlist.name == "kid public")
        .unwrap();
    assert_eq!(kid_public.username, "ana", "owner name is the account's");
    assert_eq!(kid_public.profile_name, "Kids");
    assert_eq!(kid_public.playlist.profile_id, kid);
    assert_eq!(
        kid_public.playlist.account_id, a,
        "the wire userId stays the account"
    );
    let own = seen_by_main
        .iter()
        .find(|s| s.playlist.name == "main private")
        .unwrap();
    assert_eq!(
        (own.profile_name.as_str(), own.playlist.profile_id),
        ("ana", main)
    );
    // Detail reads follow the same rule as the list.
    let kid_private = seen_by_kid_ids(&pool, kid, "kid private").await;
    assert!(
        playlists::summary(&pool, kid_private, main)
            .await
            .unwrap()
            .is_none(),
        "a sibling's private playlist is not readable"
    );
    assert!(
        playlists::summary(&pool, kid_private, kid)
            .await
            .unwrap()
            .is_some()
    );
    let found = playlists::find(&pool, kid_private).await.unwrap().unwrap();
    assert_eq!((found.profile_id, found.account_id), (kid, a));

    let seen_by_kid = playlists::list(&pool, kid, 50, 0).await.unwrap();
    assert_eq!(names(&seen_by_kid), vec!["kid private", "kid public"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hidden_playlists_are_per_profile(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let b = account(&pool, "ben").await;
    let theirs = playlists::create(
        &pool,
        Uuid::now_v7(),
        ProfileId::main_of(b),
        "ben public",
        true,
    )
    .await
    .unwrap();

    assert!(playlists::hide(&pool, kid, theirs.id).await.unwrap());
    let hidden = |list: Vec<playlists::PlaylistSummary>| {
        list.into_iter()
            .find(|s| s.playlist.id == theirs.id)
            .map(|s| s.hidden)
    };
    assert_eq!(
        hidden(playlists::list(&pool, kid, 50, 0).await.unwrap()),
        Some(true)
    );
    assert_eq!(
        hidden(playlists::list(&pool, main, 50, 0).await.unwrap()),
        Some(false)
    );
    assert!(
        !playlists::unhide(&pool, main, theirs.id).await.unwrap(),
        "main never hid it"
    );
    assert!(playlists::unhide(&pool, kid, theirs.id).await.unwrap());
}

fn hash_of(n: u32) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&n.to_be_bytes());
    h
}

/// A token row written straight into the table (the test's own setup, any profile).
async fn raw_key(pool: &PgPool, profile: ProfileId, n: u32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO api_keys (id, profile_id, name, key_hash, secret_enc) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(profile.into_uuid())
    .bind(format!("k{n}"))
    .bind(hash_of(n).as_slice())
    .bind(format!("enc{n}").into_bytes())
    .execute(pool)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_keys_cap_is_per_account(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let half = (api_keys::MAX_PER_USER / 2) as u32;
    for n in 0..half {
        raw_key(&pool, main, n).await;
    }
    for n in half..2 * half {
        raw_key(&pool, kid, n).await;
    }
    for profile in [main, kid] {
        let refused = api_keys::create(
            &pool,
            Uuid::now_v7(),
            a,
            profile,
            0,
            "one too many",
            &hash_of(999),
            b"enc",
        )
        .await
        .unwrap();
        assert_eq!(refused, Err(api_keys::Refused::TooMany));
    }
    // Another account's cap is its own.
    let b = account(&pool, "ben").await;
    let ok = api_keys::create(
        &pool,
        Uuid::now_v7(),
        b,
        ProfileId::main_of(b),
        0,
        "ben's",
        &hash_of(1000),
        b"enc",
    )
    .await
    .unwrap();
    assert!(ok.is_ok());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_keys_belong_to_their_profile(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let b = account(&pool, "ben").await;
    let key = api_keys::create(
        &pool,
        Uuid::now_v7(),
        a,
        kid,
        0,
        "tablet",
        &hash_of(1),
        b"enc",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(key.profile_id, kid);
    // A profile of another account is never accepted for this session's account.
    assert_eq!(
        api_keys::create(&pool, Uuid::now_v7(), b, kid, 0, "x", &hash_of(2), b"enc")
            .await
            .unwrap(),
        Err(api_keys::Refused::StaleSession)
    );
    let ids = |keys: Vec<api_keys::ApiKey>| keys.into_iter().map(|k| k.id).collect::<Vec<_>>();
    assert_eq!(
        ids(api_keys::list_for_profile(&pool, kid).await.unwrap()),
        vec![key.id]
    );
    assert!(
        api_keys::list_for_profile(&pool, main)
            .await
            .unwrap()
            .is_empty()
    );
    let owner = api_keys::find_by_hash(&pool, &hash_of(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (owner.key_id, owner.profile, owner.user.id),
        (key.id, kid, a.into_uuid())
    );
    assert!(
        !api_keys::delete_owned(&pool, key.id, main).await.unwrap(),
        "sibling's key"
    );
    assert!(api_keys::delete_owned(&pool, key.id, kid).await.unwrap());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_change_revokes_every_profiles_keys(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let kid = kids(&pool, a, "Kids").await;
    raw_key(&pool, ProfileId::main_of(a), 1).await;
    raw_key(&pool, kid, 2).await;
    let b = account(&pool, "ben").await;
    raw_key(&pool, ProfileId::main_of(b), 3).await;
    let mut conn = pool.acquire().await.unwrap();
    kima_db::users::set_password(&mut conn, a.into_uuid(), "new", None)
        .await
        .unwrap()
        .unwrap();
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT profile_id FROM api_keys")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, vec![b.into_uuid()]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_with_secrets_spans_all_profiles(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let mut expected = vec![raw_key(&pool, main, 1).await];
    expected.push(raw_key(&pool, kid, 2).await);
    expected.push(raw_key(&pool, kid, 3).await);
    // Another account's token never shows up.
    let b = account(&pool, "ben").await;
    raw_key(&pool, ProfileId::main_of(b), 4).await;

    let (user, secrets) = api_keys::user_with_secrets(&pool, "ANA").await.unwrap();
    assert_eq!(user.unwrap().id, a.into_uuid());
    let ids: Vec<Uuid> = secrets.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, expected, "every profile's tokens, oldest first");
}

async fn show(pool: &PgPool, n: u32) -> Uuid {
    let feed = format!("https://feed{n}.example/rss");
    let title = format!("Show {n}");
    let itunes = format!("{n}");
    podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &podcasts::NewShow {
            feed_url: &feed,
            title: &title,
            author: None,
            description: None,
            image_url: None,
            itunes_id: Some(&itunes),
            category_ids: &[1310],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn followed_and_for_you_are_per_profile(pool: PgPool) {
    // `kima_podcast::Catalog::for_you` builds its rows from `followed`.
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let one = show(&pool, 1).await;
    let two = show(&pool, 2).await;
    assert!(podcasts::subscribe(&pool, kid, one).await.unwrap());
    assert!(podcasts::subscribe(&pool, main, two).await.unwrap());

    let titles = |f: Vec<podcasts::Followed>| f.into_iter().map(|s| s.title).collect::<Vec<_>>();
    assert_eq!(
        titles(podcasts::followed(&pool, kid).await.unwrap()),
        vec!["Show 1"]
    );
    assert_eq!(
        titles(podcasts::followed(&pool, main).await.unwrap()),
        vec!["Show 2"]
    );
    assert!(podcasts::is_subscribed(&pool, kid, one).await.unwrap());
    assert!(!podcasts::is_subscribed(&pool, main, one).await.unwrap());
    let shows = podcasts::followed_shows(&pool, kid).await.unwrap();
    assert_eq!(shows.iter().map(|s| s.id).collect::<Vec<_>>(), vec![one]);
    // Unfollowing as main leaves the kids profile's follow alone.
    assert!(!podcasts::unsubscribe(&pool, main, one).await.unwrap());
    assert!(podcasts::is_subscribed(&pool, kid, one).await.unwrap());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recent_in_progress_per_profile(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let kid = kids(&pool, a, "Kids").await;
    let one = show(&pool, 1).await;
    let ep = |guid: &str| podcasts::NewEpisode {
        guid: guid.into(),
        title: guid.into(),
        description: None,
        audio_url: format!("https://cdn.example/{guid}.mp3"),
        duration: 600,
        published_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        episode_number: None,
        season: None,
        image_url: None,
        file_size: None,
        mime_type: None,
    };
    podcasts::insert_episodes(&pool, one, &[ep("e1")])
        .await
        .unwrap();
    let episode: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'e1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    podcasts::save_progress(&pool, kid, episode, 120.0, 600.0, false)
        .await
        .unwrap();

    let (kid_rows, _) = podcasts::recent_in_progress(&pool, kid, 10, false)
        .await
        .unwrap();
    assert_eq!(
        kid_rows.iter().map(|r| r.episode_id).collect::<Vec<_>>(),
        vec![episode]
    );
    assert_eq!(kid_rows[0].progress, 20);
    let (main_rows, _) = podcasts::recent_in_progress(&pool, main, 10, false)
        .await
        .unwrap();
    assert!(main_rows.is_empty(), "main never played it");
    assert_eq!(
        podcasts::progress(&pool, main, episode).await.unwrap(),
        None
    );
    assert_eq!(
        podcasts::progress(&pool, kid, episode).await.unwrap(),
        Some((120.0, 600.0))
    );
}

/// Owner decision (2026-11-14): the main profile's name is a display name of its own. It starts
/// as the username, the main profile can rename it, and a later username change leaves it alone.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn main_profile_name_is_independent_of_the_username(pool: PgPool) {
    let a = account(&pool, "ana").await;
    let main = ProfileId::main_of(a);
    let name =
        |pool: PgPool| async move { profiles::find(&pool, a, main).await.unwrap().unwrap().name };
    assert_eq!(name(pool.clone()).await, "ana");
    profiles::update(
        &pool,
        a,
        main,
        profiles::ProfileUpdate {
            name: Some("Mum".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(name(pool.clone()).await, "Mum");
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(a.into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(username, "ana", "renaming the profile leaves the username");
    sqlx::query("UPDATE users SET username = 'anna' WHERE id = $1")
        .bind(a.into_uuid())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        name(pool.clone()).await,
        "Mum",
        "a username change renames no profile"
    );
}
