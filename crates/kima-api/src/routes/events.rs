use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/events/ticket", post(issue_ticket))
        .route("/events", get(stream))
}

async fn issue_ticket(State(state): State<AppState>, user: AuthUser) -> Json<Value> {
    Json(json!({ "ticket": state.sse.issue_ticket(user.id) }))
}

#[derive(Deserialize)]
struct StreamQuery {
    ticket: Option<String>,
}

async fn stream(
    State(state): State<AppState>,
    Query(query): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let ticket = query.ticket.ok_or(ApiError::Unauthorized("Unauthorized"))?;
    let user = state
        .sse
        .redeem_ticket(&ticket)
        .ok_or(ApiError::Unauthorized("Invalid or expired ticket"))?;
    let slot = state
        .sse
        .try_open(user)
        .ok_or(ApiError::TooManyRequests("Too many event streams open"))?;

    let connected = futures::stream::once(async {
        Ok::<_, Infallible>(SseEvent::default().data(r#"{"type":"connected"}"#))
    });
    // A lagging client skips what it missed rather than being disconnected.
    let events = BroadcastStream::new(state.events.subscribe()).filter_map(move |message| {
        let event = match message {
            Ok(event) if event.is_for(user) => {
                Some(Ok(SseEvent::default().data(event.data.to_string())))
            }
            _ => None,
        };
        std::future::ready(event)
    });
    // The slot lives as long as the stream; the client hanging up drops both.
    let stream = connected.chain(events).map(move |item| {
        let _held = &slot;
        item
    });

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("heartbeat"),
    );
    Ok(([("x-accel-buffering", "no")], sse).into_response())
}
