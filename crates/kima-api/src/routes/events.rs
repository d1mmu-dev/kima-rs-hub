use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use kima_db::users;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

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

enum Step {
    Send(String),
    Skip,
    End,
}

async fn stream(
    State(state): State<AppState>,
    Query(query): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let ticket = query.ticket.ok_or(ApiError::Unauthorized("Unauthorized"))?;
    // Order matters: subscribe -> redeem -> read version.
    // - A revoke before the redeem clears the ticket, so the redeem fails (401).
    // - A revoke after the redeem (even before the version read) is delivered on the receiver,
    //   already subscribed, or shows up as a lag, which re-checks the DB.
    // - A revoke before the version read is also reflected in the version we read.
    // Subscribing after the redeem would leave a gap where the revoke event is missed while the
    // stored version already matches.
    let receiver = state.events.subscribe();
    let user = state
        .sse
        .redeem_ticket(&ticket)
        .ok_or(ApiError::Unauthorized("Invalid or expired ticket"))?;
    let version = users::find_by_id(&state.pool, user)
        .await?
        .ok_or(ApiError::Unauthorized("Invalid or expired ticket"))?
        .token_version;
    let slot = state
        .sse
        .try_open(user)
        .ok_or(ApiError::TooManyRequests("Too many event streams open"))?;

    let connected = futures::stream::once(async {
        Ok::<_, Infallible>(SseEvent::default().data(r#"{"type":"connected"}"#))
    });
    // A lagging client may have skipped its own revocation, so on lag the session is re-checked
    // in the DB: revoked (or deleted) ends the stream, otherwise the client gets a resync hint.
    // A revocation event seen in time also ends the stream (the frame itself is never sent).
    let pool = state.pool.clone();
    let events = BroadcastStream::new(receiver)
        .then(move |message| {
            let pool = pool.clone();
            async move {
                match message {
                    Ok(event) if event.is_for(user) => {
                        if event.is_revocation() {
                            Step::End
                        } else {
                            Step::Send(event.data.to_string())
                        }
                    }
                    Ok(_) => Step::Skip,
                    Err(BroadcastStreamRecvError::Lagged(_)) => {
                        match users::find_by_id(&pool, user).await {
                            Ok(Some(row)) if row.token_version == version => {
                                Step::Send(r#"{"type":"resync"}"#.to_owned())
                            }
                            _ => Step::End,
                        }
                    }
                }
            }
        })
        .take_while(|step| std::future::ready(!matches!(step, Step::End)))
        .filter_map(|step| {
            std::future::ready(match step {
                Step::Send(data) => Some(Ok::<_, Infallible>(SseEvent::default().data(data))),
                _ => None,
            })
        });
    // The slot lives as long as the stream; the client hanging up drops both.
    // Shutdown ends the stream so graceful shutdown is not held open by idle clients.
    let stream = connected
        .chain(events)
        .map(move |item| {
            let _held = &slot;
            item
        })
        .take_until(state.shutdown.clone().cancelled_owned());

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("heartbeat"),
    );
    Ok(([("x-accel-buffering", "no")], sse).into_response())
}
