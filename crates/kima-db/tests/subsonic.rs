//! Subsonic state and read models, on the SQL fixture shared with `kima-subsonic`
//! (see its header for what is in it).

use kima_db::PgPool;
use kima_db::stars::StarKind;
use kima_db::subsonic::{self, AlbumListKind, SearchPage};
use kima_db::{bookmarks, ratings, stars};
use uuid::Uuid;

/// `n` written out in decimal digits, one per nibble: 20 becomes 0x20, as in the fixture's ids.
const fn dec(mut n: u128) -> u128 {
    let (mut out, mut shift) = (0, 0);
    while n > 0 {
        out |= (n % 10) << shift;
        shift += 4;
        n /= 10;
    }
    out
}

const fn id(kind: u128, n: u128) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0000 | (kind << 44) | dec(n))
}
const ADMIN: Uuid = id(4, 1);
const ALICE: Uuid = id(4, 2);
const fn artist(n: u128) -> Uuid {
    id(1, n)
}
const fn album(n: u128) -> Uuid {
    id(2, n)
}
const fn track(n: u128) -> Uuid {
    id(3, n)
}

macro_rules! fixture_test {
    ($name:ident, |$pool:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "../../kima-subsonic/tests/fixtures", scripts("subsonic"))
        )]
        async fn $name($pool: PgPool) $body
    };
}

fn ids<T>(items: &[T], f: impl Fn(&T) -> Uuid) -> Vec<Uuid> {
    items.iter().map(f).collect()
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sort_name_strips_articles(pool: PgPool) {
    for (name, key) in [
        ("The Beatles", "a"),
        ("La Roux", "b"),
        ("Theatre", "c"),
        ("Die Antwoord", "d"),
        ("A", "e"),
        ("THE  Who", "f"),
    ] {
        sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $3)")
            .bind(Uuid::now_v7())
            .bind(name)
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sort_name FROM artists ORDER BY name_key")
            .fetch_all(&pool)
            .await
            .unwrap();
    let got: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(
        got,
        [
            ("The Beatles", "beatles"),
            ("La Roux", "roux"),
            ("Theatre", "theatre"),
            ("Die Antwoord", "antwoord"),
            ("A", "a"), // an article alone, with nothing after it, stays
            ("THE  Who", "who"),
        ]
    );
}

fixture_test!(artists_sorted_hide_artists_without_present_tracks, |pool| {
    let artists = subsonic::artists_sorted(&pool, ALICE).await.unwrap();
    // Theatre's only track is missing. The Beatles file under B and La Roux under R.
    assert_eq!(ids(&artists, |a| a.id), [artist(1), artist(2), artist(3)]);
    let sort: Vec<&str> = artists.iter().map(|a| a.sort_name.as_str()).collect();
    assert_eq!(sort, ["beatles", "roux", "various artists"]);
    let albums: Vec<i64> = artists.iter().map(|a| a.album_count).collect();
    assert_eq!(albums, [2, 2, 1]);
    assert!(
        subsonic::artist_with_albums(&pool, ALICE, artist(4))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        subsonic::album_with_tracks(&pool, ALICE, album(6))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        subsonic::track(&pool, ALICE, track(20))
            .await
            .unwrap()
            .is_none()
    );
});

fixture_test!(album_tracks_ordered_by_disc_then_track, |pool| {
    let (al, songs) = subsonic::album_with_tracks(&pool, ALICE, album(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (al.title.as_str(), al.song_count, al.duration_secs),
        ("White Album", 4, 163 + 236 + 162 + 270)
    );
    assert_eq!(
        ids(&songs, |s| s.track.id),
        [track(6), track(7), track(8), track(9)]
    );
    assert_eq!(songs[2].track.disc, 2);
    assert_eq!(
        songs[0].path,
        "The Beatles/White Album/1-01 Back in the U.S.S.R..mp3"
    );
    assert_eq!(songs[0].size, 30000006);

    // Equal numbers: by title, then id.
    let (_, demos) = subsonic::album_with_tracks(&pool, ALICE, album(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ids(&demos, |s| s.track.id),
        [track(17), track(18), track(19)]
    );

    let (artist_item, albums) = subsonic::artist_with_albums(&pool, ALICE, artist(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(artist_item.name, "The Beatles");
    // Newest year first, undated last.
    assert_eq!(ids(&albums, |a| a.id), [album(1), album(2)]);
    let (_, albums) = subsonic::artist_with_albums(&pool, ALICE, artist(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids(&albums, |a| a.id), [album(3), album(5)]);
});

fixture_test!(starred_and_rating_joined_for_caller_only, |pool| {
    let alice = subsonic::track(&pool, ALICE, track(1))
        .await
        .unwrap()
        .unwrap();
    assert!(alice.starred_at.is_some());
    assert_eq!(alice.rating, Some(5));
    let admin = subsonic::track(&pool, ADMIN, track(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((admin.starred_at, admin.rating), (None, Some(1)));
    let admin2 = subsonic::track(&pool, ADMIN, track(2))
        .await
        .unwrap()
        .unwrap();
    assert!(admin2.starred_at.is_some());
    let alice2 = subsonic::track(&pool, ALICE, track(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((alice2.starred_at, alice2.rating), (None, None));

    let (al, songs) = subsonic::album_with_tracks(&pool, ALICE, album(1))
        .await
        .unwrap()
        .unwrap();
    assert!(al.starred_at.is_some());
    assert_eq!(al.rating, Some(4));
    assert_eq!(songs[0].rating, Some(5));
    let (al, _) = subsonic::album_with_tracks(&pool, ADMIN, album(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((al.starred_at, al.rating), (None, None));

    let artists = subsonic::artists_sorted(&pool, ALICE).await.unwrap();
    assert!(artists[0].starred_at.is_some() && artists[0].rating.is_none()); // Beatles
    assert_eq!((artists[1].starred_at, artists[1].rating), (None, Some(3))); // La Roux
    let artists = subsonic::artists_sorted(&pool, ADMIN).await.unwrap();
    assert!(
        artists
            .iter()
            .all(|a| a.starred_at.is_none() && a.rating.is_none())
    );
});

fixture_test!(stars_one_target_check, |pool| {
    let two =
        sqlx::query("INSERT INTO stars (id, user_id, track_id, album_id) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::now_v7())
            .bind(ALICE)
            .bind(track(5))
            .bind(album(1))
            .execute(&pool)
            .await;
    assert!(two.is_err());
    let none = sqlx::query("INSERT INTO stars (id, user_id) VALUES ($1, $2)")
        .bind(Uuid::now_v7())
        .bind(ALICE)
        .execute(&pool)
        .await;
    assert!(none.is_err());
    let two = sqlx::query(
        "INSERT INTO ratings (id, user_id, track_id, artist_id, rating) VALUES ($1, $2, $3, $4, 3)",
    )
    .bind(Uuid::now_v7())
    .bind(ALICE)
    .bind(track(5))
    .bind(artist(1))
    .execute(&pool)
    .await;
    assert!(two.is_err());
});

fixture_test!(star_set_unknown_target_false, |pool| {
    for kind in [StarKind::Track, StarKind::Album, StarKind::Artist] {
        assert!(
            !stars::set(&pool, ALICE, kind, Uuid::now_v7())
                .await
                .unwrap()
        );
        assert!(
            !ratings::set(&pool, ALICE, kind, Uuid::now_v7(), 3)
                .await
                .unwrap()
        );
    }
    // Known targets star once, and starring again keeps the first time.
    let before = count(&pool, "SELECT count(*) FROM stars").await;
    assert!(
        stars::set(&pool, ALICE, StarKind::Track, track(5))
            .await
            .unwrap()
    );
    assert!(
        stars::set(&pool, ALICE, StarKind::Track, track(5))
            .await
            .unwrap()
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM stars").await, before + 1);
    let first: time::OffsetDateTime =
        sqlx::query_scalar("SELECT starred_at FROM stars WHERE user_id = $1 AND track_id = $2")
            .bind(ALICE)
            .bind(track(5))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        stars::set(&pool, ALICE, StarKind::Track, track(5))
            .await
            .unwrap()
    );
    let again: time::OffsetDateTime =
        sqlx::query_scalar("SELECT starred_at FROM stars WHERE user_id = $1 AND track_id = $2")
            .bind(ALICE)
            .bind(track(5))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(first, again);
    // A missing track can still be starred (hidden until it returns).
    assert!(
        stars::set(&pool, ADMIN, StarKind::Track, track(20))
            .await
            .unwrap()
    );

    stars::clear(&pool, ALICE, StarKind::Track, track(5))
        .await
        .unwrap();
    stars::clear(&pool, ALICE, StarKind::Track, track(5))
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM stars WHERE track_id = '00000000-0000-7000-8000-300000000005'"
        )
        .await,
        0
    );
});

fixture_test!(star_lists_are_newest_first_and_present_only, |pool| {
    // Stored: tracks 1 and 20 (missing); albums 3, 1, 6 (no present track); artist 1.
    assert_eq!(
        stars::list_for_user(&pool, ALICE, StarKind::Track)
            .await
            .unwrap(),
        [track(1)]
    );
    assert_eq!(
        stars::list_for_user(&pool, ALICE, StarKind::Album)
            .await
            .unwrap(),
        [album(1), album(3)]
    );
    assert_eq!(
        stars::list_for_user(&pool, ALICE, StarKind::Artist)
            .await
            .unwrap(),
        [artist(1)]
    );
    assert_eq!(
        stars::list_for_user(&pool, ADMIN, StarKind::Track)
            .await
            .unwrap(),
        [track(2)]
    );
    assert!(
        stars::list_for_user(&pool, ADMIN, StarKind::Album)
            .await
            .unwrap()
            .is_empty()
    );
});

fixture_test!(rating_zero_is_clear, |pool| {
    assert!(
        ratings::set(&pool, ALICE, StarKind::Track, track(2), 3)
            .await
            .unwrap()
    );
    assert_eq!(
        subsonic::track(&pool, ALICE, track(2))
            .await
            .unwrap()
            .unwrap()
            .rating,
        Some(3)
    );
    // A new rating replaces the old one.
    assert!(
        ratings::set(&pool, ALICE, StarKind::Track, track(2), 5)
            .await
            .unwrap()
    );
    assert_eq!(
        subsonic::track(&pool, ALICE, track(2))
            .await
            .unwrap()
            .unwrap()
            .rating,
        Some(5)
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM ratings WHERE track_id = '00000000-0000-7000-8000-300000000002'"
        )
        .await,
        1
    );
    // Albums and artists rate too.
    assert!(
        ratings::set(&pool, ALICE, StarKind::Album, album(3), 2)
            .await
            .unwrap()
    );
    assert!(
        ratings::set(&pool, ALICE, StarKind::Artist, artist(3), 1)
            .await
            .unwrap()
    );

    // setRating 0 is `clear`; clearing nothing is fine.
    ratings::clear(&pool, ALICE, StarKind::Track, track(2))
        .await
        .unwrap();
    ratings::clear(&pool, ALICE, StarKind::Track, track(2))
        .await
        .unwrap();
    assert_eq!(
        subsonic::track(&pool, ALICE, track(2))
            .await
            .unwrap()
            .unwrap()
            .rating,
        None
    );
    // Stars are untouched by ratings.
    assert!(
        subsonic::track(&pool, ADMIN, track(2))
            .await
            .unwrap()
            .unwrap()
            .starred_at
            .is_some()
    );
    // The table only holds 1..=5.
    assert!(
        ratings::set(&pool, ALICE, StarKind::Track, track(2), 0)
            .await
            .is_err()
    );
    assert!(
        ratings::set(&pool, ALICE, StarKind::Track, track(2), 6)
            .await
            .is_err()
    );
});

fixture_test!(bookmarks_hide_missing_tracks, |pool| {
    let list = bookmarks::list_for_user(&pool, ALICE).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].track_id, track(3));
    assert_eq!(
        (list[0].position_ms, list[0].comment.as_deref()),
        (90000, Some("resume here"))
    );
    // The bookmark on the missing track is kept.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM subsonic_bookmarks").await,
        2
    );
    assert!(
        bookmarks::list_for_user(&pool, ADMIN)
            .await
            .unwrap()
            .is_empty()
    );

    // Moving a bookmark keeps its creation time and puts it first.
    assert!(
        bookmarks::upsert(&pool, ALICE, track(9), 5, None)
            .await
            .unwrap()
    );
    assert!(
        bookmarks::upsert(&pool, ALICE, track(3), 91000, Some("later"))
            .await
            .unwrap()
    );
    let list = bookmarks::list_for_user(&pool, ALICE).await.unwrap();
    assert_eq!(ids(&list, |b| b.track_id), [track(3), track(9)]);
    assert_eq!(
        (list[0].position_ms, list[0].comment.as_deref()),
        (91000, Some("later"))
    );
    assert_eq!(
        list[0].created_at,
        time::macros::datetime!(2026-01-01 06:00:00 UTC)
    );
    assert!(list[0].changed_at > list[0].created_at);

    assert!(
        !bookmarks::upsert(&pool, ALICE, Uuid::now_v7(), 1, None)
            .await
            .unwrap()
    );
    bookmarks::delete(&pool, ALICE, track(3)).await.unwrap();
    bookmarks::delete(&pool, ALICE, track(3)).await.unwrap();
    assert_eq!(
        ids(
            &bookmarks::list_for_user(&pool, ALICE).await.unwrap(),
            |b| b.track_id
        ),
        [track(9)]
    );
});

fixture_test!(album_list_each_kind, |pool| {
    let list = |kind| {
        let pool = pool.clone();
        async move {
            ids(
                &subsonic::album_list(&pool, ALICE, kind, 50, 0)
                    .await
                    .unwrap(),
                |a| a.id,
            )
        }
    };
    // Album 6 has no present track, so it is never listed.
    assert_eq!(
        list(AlbumListKind::Newest).await,
        [album(5), album(4), album(3), album(2), album(1)]
    );
    assert_eq!(list(AlbumListKind::Highest).await, [album(4), album(1)]);
    assert_eq!(
        list(AlbumListKind::Frequent).await,
        [album(3), album(1), album(4)]
    );
    assert_eq!(
        list(AlbumListKind::Recent).await,
        [album(1), album(3), album(4)]
    );
    assert_eq!(
        list(AlbumListKind::AlphabeticalByName).await,
        [album(1), album(4), album(3), album(5), album(2)] // abbey road, now hits, roux, untitled demos, white album
    );
    assert_eq!(
        list(AlbumListKind::AlphabeticalByArtist).await,
        [album(1), album(2), album(3), album(5), album(4)] // beatles, roux, various artists
    );
    assert_eq!(list(AlbumListKind::Starred).await, [album(1), album(3)]);
    assert_eq!(
        list(AlbumListKind::ByYear {
            from: 1968,
            to: 1969
        })
        .await,
        [album(2), album(1)]
    );
    assert_eq!(
        list(AlbumListKind::ByYear {
            from: 1969,
            to: 1968
        })
        .await,
        [album(1), album(2)]
    );
    assert_eq!(
        list(AlbumListKind::ByYear {
            from: 1960,
            to: 2010
        })
        .await,
        [album(2), album(1), album(3), album(4)]
    );
    assert_eq!(
        list(AlbumListKind::ByYear {
            from: 2010,
            to: 1960
        })
        .await,
        [album(4), album(3), album(1), album(2)]
    );

    // Another user's plays, stars and ratings do not count.
    let admin = subsonic::album_list(&pool, ADMIN, AlbumListKind::Frequent, 50, 0)
        .await
        .unwrap();
    assert_eq!(ids(&admin, |a| a.id), [album(2)]);
    assert!(
        subsonic::album_list(&pool, ADMIN, AlbumListKind::Highest, 50, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        subsonic::album_list(&pool, ADMIN, AlbumListKind::Starred, 50, 0)
            .await
            .unwrap()
            .is_empty()
    );

    // Paging.
    let page = subsonic::album_list(&pool, ALICE, AlbumListKind::Newest, 2, 1)
        .await
        .unwrap();
    assert_eq!(ids(&page, |a| a.id), [album(4), album(3)]);

    // Random: distinct present albums.
    let random = ids(
        &subsonic::album_list(&pool, ALICE, AlbumListKind::Random, 3, 0)
            .await
            .unwrap(),
        |a| a.id,
    );
    assert_eq!(random.len(), 3);
    assert!(!random.contains(&album(6)));
    let mut unique = random.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3);

    // The row carries the album's own data.
    let first = &subsonic::album_list(&pool, ALICE, AlbumListKind::Highest, 1, 0)
        .await
        .unwrap()[0];
    assert_eq!(
        (first.title.as_str(), first.artist_name.as_str(), first.year),
        ("Now Hits", "Various Artists", Some(2010))
    );
    assert_eq!(
        (first.song_count, first.rating, first.artist_id),
        (4, Some(5), artist(3))
    );
});

fixture_test!(random_and_top_tracks, |pool| {
    let all = subsonic::random_tracks(&pool, ALICE, 100, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 19); // not the missing one
    assert_eq!(
        subsonic::random_tracks(&pool, ALICE, 5, None)
            .await
            .unwrap()
            .len(),
        5
    );
    // The year is the track's own, else its album's.
    let by_year = |from, to| {
        let pool = pool.clone();
        async move {
            let mut got = ids(
                &subsonic::random_tracks(&pool, ALICE, 100, Some((from, to)))
                    .await
                    .unwrap(),
                |s| s.track.id,
            );
            got.sort();
            got
        }
    };
    assert_eq!(by_year(1970, 1970).await, [track(2)]);
    assert_eq!(
        by_year(2009, 2009).await,
        [track(10), track(11), track(12), track(13)]
    );
    assert_eq!(
        by_year(1968, 1969).await,
        [
            track(1),
            track(3),
            track(4),
            track(5),
            track(6),
            track(7),
            track(8),
            track(9)
        ]
    );

    // Top songs: plays of all users, then title; exact names first, else a substring.
    let top = subsonic::top_tracks(&pool, ALICE, "la roux", 50)
        .await
        .unwrap();
    assert_eq!(
        ids(&top, |s| s.track.id),
        [
            track(10),
            track(11),
            track(17),
            track(18),
            track(19),
            track(12)
        ]
    );
    let top = subsonic::top_tracks(&pool, ALICE, "roux", 3).await.unwrap();
    assert_eq!(ids(&top, |s| s.track.id), [track(10), track(11), track(17)]);
    let top = subsonic::top_tracks(&pool, ALICE, "KESHA", 50)
        .await
        .unwrap();
    assert_eq!(ids(&top, |s| s.track.id), [track(13)]);
    assert!(
        subsonic::top_tracks(&pool, ALICE, "nobody", 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        subsonic::top_tracks(&pool, ALICE, "100%", 50)
            .await
            .unwrap()
            .is_empty()
    );
    // Plays of every user count: the admin's one play lifts a Beatles song.
    let top = subsonic::top_tracks(&pool, ALICE, "The Beatles", 2)
        .await
        .unwrap();
    assert_eq!(ids(&top, |s| s.track.id), [track(1), track(6)]);
});

fixture_test!(tracks_by_ids_keeps_order_drops_missing, |pool| {
    let unknown = Uuid::now_v7();
    let asked = [track(13), unknown, track(1), track(20), track(10)];
    let got = subsonic::tracks_by_ids(&pool, ALICE, &asked).await.unwrap();
    assert_eq!(ids(&got, |s| s.track.id), [track(13), track(1), track(10)]);
    assert!(got[1].starred_at.is_some());
    assert!(
        subsonic::tracks_by_ids(&pool, ALICE, &[])
            .await
            .unwrap()
            .is_empty()
    );

    let albums = subsonic::albums_by_ids(&pool, ALICE, &[album(4), album(6), album(1)])
        .await
        .unwrap();
    assert_eq!(ids(&albums, |a| a.id), [album(4), album(1)]);
    let artists = subsonic::artists_by_ids(&pool, ALICE, &[artist(3), artist(4), artist(1)])
        .await
        .unwrap();
    assert_eq!(ids(&artists, |a| a.id), [artist(3), artist(1)]);
});

fixture_test!(search_offset_pages, |pool| {
    let page = |song_count, song_offset| SearchPage {
        artist_count: 0,
        artist_offset: 0,
        album_count: 0,
        album_offset: 0,
        song_count,
        song_offset,
    };
    let (_, _, everything) = subsonic::search_all(&pool, ALICE, page(500, 0))
        .await
        .unwrap();
    assert_eq!(everything.len(), 19);
    let titles: Vec<String> = everything
        .iter()
        .map(|s| s.track.title.to_lowercase())
        .collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(titles, sorted);
    // The tied titles come out by id.
    let intro: Vec<Uuid> = everything
        .iter()
        .filter(|s| s.track.title == "Intro")
        .map(|s| s.track.id)
        .collect();
    assert_eq!(intro, [track(17), track(18)]);

    // Paging by 7 sees every track exactly once, whatever the ties.
    let mut paged = Vec::new();
    for offset in (0..).step_by(7) {
        let (_, _, songs) = subsonic::search_all(&pool, ALICE, page(7, offset))
            .await
            .unwrap();
        let short = songs.len() < 7;
        paged.extend(ids(&songs, |s| s.track.id));
        if short {
            break;
        }
    }
    assert_eq!(paged, ids(&everything, |s| s.track.id));

    // Artists and albums page on their own offsets.
    let (artists, albums, songs) = subsonic::search_all(
        &pool,
        ALICE,
        SearchPage {
            artist_count: 2,
            artist_offset: 2,
            album_count: 2,
            album_offset: 1,
            song_count: 0,
            song_offset: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(ids(&artists, |a| a.id), [artist(3)]);
    assert_eq!(ids(&albums, |a| a.id), [album(4), album(3)]);
    assert!(songs.is_empty());
});

fixture_test!(search_module_takes_an_offset, |pool| {
    let q = kima_db::search::prefix_query(&pool, "intro")
        .await
        .unwrap()
        .unwrap();
    let first = kima_db::search::tracks(&pool, &q, 1, 0).await.unwrap();
    let second = kima_db::search::tracks(&pool, &q, 1, 1).await.unwrap();
    assert_eq!((first.len(), second.len()), (1, 1));
    assert_ne!(first[0].id, second[0].id);
    assert!(
        kima_db::search::tracks(&pool, &q, 5, 2)
            .await
            .unwrap()
            .is_empty()
    );
});

fixture_test!(kind_of_each_table, |pool| {
    assert_eq!(
        subsonic::kind_of(&pool, track(1)).await.unwrap(),
        Some(StarKind::Track)
    );
    assert_eq!(
        subsonic::kind_of(&pool, track(20)).await.unwrap(),
        Some(StarKind::Track)
    );
    assert_eq!(
        subsonic::kind_of(&pool, album(2)).await.unwrap(),
        Some(StarKind::Album)
    );
    assert_eq!(
        subsonic::kind_of(&pool, artist(2)).await.unwrap(),
        Some(StarKind::Artist)
    );
    assert_eq!(
        subsonic::kind_of(&pool, Uuid::now_v7()).await.unwrap(),
        None
    );
});

fixture_test!(purging_track_cascades_stars_ratings_bookmarks, |pool| {
    let hidden = |table: &'static str| {
        let pool = pool.clone();
        async move {
            count(&pool, &format!("SELECT count(*) FROM {table} WHERE track_id = '00000000-0000-7000-8000-300000000020'")).await
        }
    };
    // While the track is only flagged missing, its rows are kept.
    assert_eq!(
        (
            hidden("stars").await,
            hidden("ratings").await,
            hidden("subsonic_bookmarks").await
        ),
        (1, 1, 1)
    );
    assert!(
        stars::list_for_user(&pool, ALICE, StarKind::Track)
            .await
            .unwrap()
            .iter()
            .all(|t| *t != track(20))
    );

    // The purge deletes the track row, then the emptied album and artist.
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(track(20))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        (
            hidden("stars").await,
            hidden("ratings").await,
            hidden("subsonic_bookmarks").await
        ),
        (0, 0, 0)
    );
    let album_stars =
        "SELECT count(*) FROM stars WHERE album_id = '00000000-0000-7000-8000-200000000006'";
    assert_eq!(count(&pool, album_stars).await, 1);
    sqlx::query("DELETE FROM albums WHERE id = $1")
        .bind(album(6))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, album_stars).await, 0);

    // Deleting a user removes their state too.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(ALICE)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM stars WHERE user_id = '00000000-0000-7000-8000-400000000002'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM subsonic_bookmarks").await,
        0
    );
});

fixture_test!(set_fk_violation_is_false, |pool| {
    let done = stars::set(&pool, ALICE, StarKind::Track, Uuid::now_v7())
        .await
        .unwrap();
    assert!(!done);
});

fixture_test!(kinds_of_mixed_ids, |pool| {
    let ids = [track(1), album(1), artist(1), Uuid::now_v7()];
    let kinds = subsonic::kinds_of(&pool, &ids).await.unwrap();
    assert_eq!(kinds.len(), 3);
    assert_eq!(kinds[&track(1)], StarKind::Track);
    assert_eq!(kinds[&album(1)], StarKind::Album);
    assert_eq!(kinds[&artist(1)], StarKind::Artist);
});

fixture_test!(artist_exists_follows_visibility, |pool| {
    assert!(subsonic::artist_exists(&pool, artist(1)).await.unwrap());
    // Artist 4 has no present track.
    assert!(!subsonic::artist_exists(&pool, artist(4)).await.unwrap());
    assert!(
        !subsonic::artist_exists(&pool, Uuid::now_v7())
            .await
            .unwrap()
    );
});
