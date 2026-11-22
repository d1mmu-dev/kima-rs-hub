use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/system/features", get(features))
}

/// `fpcalc` says whether fingerprinting can run (spec §9.4, D-106); the other three upstream
/// flags stay off until their subsystems land. The rest say which sections of the app exist:
/// `library` is on from M1, everything not yet ported is off, so the frontend hides those
/// sections (D-030); `stats` is on from M-STATS S1 (D-170), `mixes` from M5b; `changelog` follows
/// kima-rs's own release catalog (D-267).
async fn features(State(state): State<AppState>, _user: AuthUser) -> Json<Value> {
    let vibe_embeddings = kima_ident::stages::vibe_assets::vibe_embeddings(&state.ident_ctx())
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(?error, "Vibe feature eligibility unavailable");
            false
        });
    Json(json!({
        "musicCNN": false,
        "vibeEmbeddings": vibe_embeddings,
        "audiobookshelfEnabled": false,
        "fpcalc": state.fpcalc,
        "library": true,
        // M6a: every `/podcasts/*` route the v1.12.3 pages call is served (D-030/D-170,
        // `tests/auth_flow.rs::every_upstream_podcast_route_is_served`); audiobooks are M6b
        // (D-201).
        "podcasts": true,
        "audiobooks": false,
        "discover": false,
        // M5b: every `/mixes/*`, radio/decade, `/whats-new/*` and `/recommendations/for-you`
        // route is served.
        "mixes": true,
        "downloads": false,
        "imports": false,
        // M-STATS S1: every `/stats/*` route the v1.12.3 page calls is served; analysis,
        // spoken-word and system parts answer empty (D-174–D-176). `accountDeletion` stays
        // unsent, which the frontend reads as off.
        "stats": true,
        // Owner Q3 = B (D-267): off until kima-rs's own catalog has its first release.
        "changelog": !state.releases.is_empty(),
    }))
}
