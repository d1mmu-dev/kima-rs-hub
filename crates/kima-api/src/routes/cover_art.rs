//! `GET /api/library/cover-art/:id?size=N`: an album's cover or an artist's image (D-113),
//! resized and cached. The id must parse as a UUID; nothing of the request text reaches a path.
//! `GET /api/library/cover-art?url=<url>[&size=N]` (Task 15, D-210): the external-image proxy
//! podcast show/episode covers point at.

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::media::{self, RemoteCoverResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        // The frontend also builds `?url=` links for external covers; M1 serves library covers only.
        .route("/library/cover-art", get(no_id))
        .route("/library/cover-art/{id}", get(cover_art))
}

#[derive(Deserialize)]
struct Params {
    size: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct UrlParams {
    url: Option<String>,
    size: Option<String>,
}

fn parse_width(size: Option<&str>) -> Option<u32> {
    size.and_then(|s| s.parse::<u32>().ok()).filter(|n| *n > 0)
}

async fn no_id(
    State(state): State<AppState>,
    _user: AuthUser,
    headers: axum::http::HeaderMap,
    Query(params): Query<UrlParams>,
) -> Result<Response, ApiError> {
    let Some(url) = params.url.filter(|u| !u.is_empty()) else {
        return Err(ApiError::BadRequest("No cover ID or URL provided".into()));
    };
    let width = parse_width(params.size.as_deref());
    match media::remote_cover(&state, &url, width).await {
        RemoteCoverResult::Found(cover) => {
            let fresh = headers
                .get(IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v == cover.etag);
            if fresh {
                return Ok((
                    StatusCode::NOT_MODIFIED,
                    [
                        (ETAG, cover.etag),
                        (CACHE_CONTROL, "private, max-age=86400".into()),
                    ],
                )
                    .into_response());
            }
            struct ResponseBytes {
                bytes: axum::body::Bytes,
                _permit: Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
            }
            impl AsRef<[u8]> for ResponseBytes {
                fn as_ref(&self) -> &[u8] {
                    &self.bytes
                }
            }
            let bytes = axum::body::Bytes::from_owner(ResponseBytes {
                bytes: cover.bytes,
                _permit: cover.permit,
            });
            Ok((
                [
                    (CONTENT_TYPE, cover.content_type),
                    (ETAG, cover.etag),
                    (CACHE_CONTROL, "private, max-age=86400".into()),
                ],
                bytes,
            )
                .into_response())
        }
        RemoteCoverResult::NotFound => Err(ApiError::NotFound("Cover art not found")),
        RemoteCoverResult::InvalidAddress => {
            Err(ApiError::BadRequest("Invalid cover art URL".into()))
        }
        RemoteCoverResult::Busy => Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "1")],
            "Cover art proxy busy; retry shortly",
        )
            .into_response()),
        RemoteCoverResult::Unavailable => Err(ApiError::BadGateway(
            "The picture's host did not answer; try again shortly",
        )),
    }
}

async fn cover_art(
    State(state): State<AppState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    headers: axum::http::HeaderMap,
    Query(params): Query<Params>,
) -> Result<Response, ApiError> {
    if let Some(url) = params
        .url
        .filter(|url| !url.is_empty())
        .or_else(|| (id.starts_with("http://") || id.starts_with("https://")).then_some(id.clone()))
    {
        return no_id(
            State(state),
            user,
            headers,
            Query(UrlParams {
                url: Some(url),
                size: params.size,
            }),
        )
        .await;
    }
    let size = match params.size.as_deref() {
        None | Some("") => None,
        Some(s) => match s.parse::<u32>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                return Err(ApiError::BadRequest(
                    "size must be a positive integer".into(),
                ));
            }
        },
    };
    let not_found = || ApiError::NotFound("Cover art not found");
    let id = id
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid cover ID format".into()))?;
    let img = media::cover(&state, id, size)
        .await?
        .ok_or_else(not_found)?;
    Ok((
        [
            (CONTENT_TYPE, img.content_type),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        img.bytes,
    )
        .into_response())
}
