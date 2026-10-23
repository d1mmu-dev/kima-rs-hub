use axum::Router;
use axum::handler::Handler;
use axum::http::Uri;
use axum::routing::get;

use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;

mod bookmarks;
mod browse;
mod info;
mod media;
mod playlists;
mod queue;
mod scrobble;
mod search;
mod stars;
mod stubs;
mod system;
mod users;

/// Registers `/{name}` and `/{name}.view` for GET, HEAD and POST.
pub fn add<H, T>(router: Router<SubsonicCtx>, name: &'static str, handler: H) -> Router<SubsonicCtx>
where
    H: Handler<T, SubsonicCtx> + Clone + Send + 'static,
    T: 'static,
{
    let methods = get(handler.clone()).head(handler.clone()).post(handler);
    router
        .route(&format!("/{name}"), methods.clone())
        .route(&format!("/{name}.view"), methods)
}

/// Every endpoint name, in spec §4 order. Each one answers with and without `.view`; `hls` also
/// answers as `hls.m3u8`, which is the one name that is not a plain `{name}` route.
pub const ENDPOINTS: &[&str] = &[
    "ping",
    "getLicense",
    "getMusicFolders",
    "getOpenSubsonicExtensions",
    "tokenInfo",
    "getScanStatus",
    "startScan",
    "getArtists",
    "getIndexes",
    "getArtist",
    "getMusicDirectory",
    "getAlbum",
    "getSong",
    "getAlbumList2",
    "getAlbumList",
    "search3",
    "search2",
    "search",
    "getRandomSongs",
    "getSongsByGenre",
    "getTopSongs",
    "getSimilarSongs",
    "getSimilarSongs2",
    "getGenres",
    "getNowPlaying",
    "stream",
    "download",
    "hls",
    "hls.m3u8",
    "getTranscodeDecision",
    "getTranscodeStream",
    "getCoverArt",
    "getAvatar",
    "scrobble",
    "getPlayQueue",
    "getPlayQueueByIndex",
    "savePlayQueue",
    "savePlayQueueByIndex",
    "getBookmarks",
    "createBookmark",
    "deleteBookmark",
    "getPlaylists",
    "getPlaylist",
    "createPlaylist",
    "updatePlaylist",
    "deletePlaylist",
    "star",
    "unstar",
    "getStarred",
    "getStarred2",
    "setRating",
    "getUser",
    "getUsers",
    "createUser",
    "updateUser",
    "deleteUser",
    "changePassword",
    "getAlbumInfo",
    "getAlbumInfo2",
    "getArtistInfo",
    "getArtistInfo2",
    "getLyrics",
    "getLyricsBySongId",
    "getPodcasts",
    "getNewestPodcasts",
    "getPodcastEpisode",
    "refreshPodcasts",
    "getShares",
    "createShare",
    "updateShare",
    "deleteShare",
    "getInternetRadioStations",
    "createInternetRadioStation",
    "updateInternetRadioStation",
    "deleteInternetRadioStation",
    "getChatMessages",
    "addChatMessage",
    "getVideos",
    "getVideoInfo",
    "getCaptions",
    "jukeboxControl",
];

pub fn router() -> Router<SubsonicCtx> {
    let router = Router::new();
    let router = system::add_routes(router);
    let router = browse::add_routes(router);
    let router = search::add_routes(router);
    let router = media::add_routes(router);
    let router = scrobble::add_routes(router);
    let router = queue::add_routes(router);
    let router = playlists::add_routes(router);
    let router = stars::add_routes(router);
    let router = bookmarks::add_routes(router);
    let router = users::add_routes(router);
    let router = info::add_routes(router);
    let router = stubs::add_routes(router);
    router.fallback(not_implemented)
}

/// Unknown endpoints answer 0, as upstream's catch-all does, after auth (so an unauthenticated
/// probe learns nothing about which endpoints exist).
async fn not_implemented(_: SubsonicUser, uri: Uri) -> SubsonicError {
    let path = uri.path();
    let path = if path.ends_with(".view") {
        path.to_owned()
    } else {
        format!("{path}.view")
    };
    SubsonicError::generic(format!("Not implemented: {path}"))
}
