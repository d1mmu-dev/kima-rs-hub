//! Later-milestone and compat stubs (spec §4.9 #58-#71, §4.10 #72-#81, D-086). Each answers with
//! the empty element or the error its row names, so clients that probe these endpoints get a
//! well-formed answer instead of `Not implemented`.

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::subsonic;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids::{self, Prefix};
use crate::params::Params;
use crate::response::{Answer, Node};

const NO_SHARES: &str = "Sharing is not supported by this server yet";
const NO_RADIO: &str = "Internet radio is not supported by this server";
const NO_CHAT: &str = "Chat is not supported by this server";
const NO_JUKEBOX: &str = "Jukebox mode is not supported by this server";

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getAlbumInfo", album_info);
    let router = add(router, "getAlbumInfo2", album_info);
    let router = add(router, "getArtistInfo", artist_info);
    let router = add(router, "getArtistInfo2", artist_info2);
    let router = add(router, "getLyrics", lyrics);
    let router = add(router, "getLyricsBySongId", lyrics_by_song_id);
    let router = add(router, "getPodcasts", podcasts);
    let router = add(router, "getNewestPodcasts", newest_podcasts);
    let router = add(router, "getPodcastEpisode", podcast_episode);
    let router = add(router, "refreshPodcasts", refresh_podcasts);
    let router = add(router, "getShares", shares);
    let router = add(router, "createShare", no_shares);
    let router = add(router, "updateShare", no_shares);
    let router = add(router, "deleteShare", no_shares);
    let router = add(router, "getInternetRadioStations", radio_stations);
    let router = add(router, "createInternetRadioStation", no_radio);
    let router = add(router, "updateInternetRadioStation", no_radio);
    let router = add(router, "deleteInternetRadioStation", no_radio);
    let router = add(router, "getChatMessages", chat_messages);
    let router = add(router, "addChatMessage", no_chat);
    let router = add(router, "getVideos", videos);
    let router = add(router, "getVideoInfo", video_info);
    let router = add(router, "getCaptions", captions);
    add(router, "jukeboxControl", jukebox_control)
}

fn empty(name: &'static str) -> Result<Answer, SubsonicError> {
    Ok(Answer::node(Node::new(name)))
}

async fn album_info(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("albumInfo")
}

/// 10 without `id`, 70 for an unknown artist, else an empty element.
async fn known_artist(
    user: &SubsonicUser,
    app: &AppState,
    params: &Params,
) -> Result<(), SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Artist not found");
    let id = ids::parse(raw, &[Prefix::Ar, Prefix::ArtistColon]).ok_or_else(not_found)?;
    subsonic::artist_with_albums(&app.pool, user.user.id, id)
        .await?
        .map(|_| ())
        .ok_or_else(not_found)
}

async fn artist_info(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    known_artist(&user, &app, &params).await?;
    empty("artistInfo")
}

async fn artist_info2(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    known_artist(&user, &app, &params).await?;
    empty("artistInfo2")
}

async fn lyrics(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("lyrics")
}

async fn lyrics_by_song_id(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("id")?;
    empty("lyricsList")
}

async fn podcasts(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("podcasts")
}

async fn newest_podcasts(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("newestPodcasts")
}

async fn podcast_episode(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Err(SubsonicError::not_found("Podcast episode not found"))
}

async fn refresh_podcasts(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Ok(Answer::ok())
}

async fn shares(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("shares")
}

async fn no_shares(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Err(SubsonicError::generic(NO_SHARES))
}

async fn radio_stations(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("internetRadioStations")
}

async fn no_radio(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Err(SubsonicError::generic(NO_RADIO))
}

async fn chat_messages(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("chatMessages")
}

async fn no_chat(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Err(SubsonicError::generic(NO_CHAT))
}

async fn videos(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    empty("videos")
}

async fn video_info(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("id")?;
    empty("videoInfo")
}

async fn captions(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("id")?;
    Err(SubsonicError::not_found("No captions available"))
}

async fn jukebox_control(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("action")?;
    Err(SubsonicError::generic(NO_JUKEBOX))
}
