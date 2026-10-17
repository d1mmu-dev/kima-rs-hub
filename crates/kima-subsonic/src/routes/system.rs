//! Endpoints that need no Subsonic auth.

use axum::Router;
use axum::extract::State;
use kima_api::AppState;

use super::add;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::params::Params;
use crate::response::{Answer, Node};

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getOpenSubsonicExtensions", extensions);
    add(router, "tokenInfo", token_info)
}

/// No auth: OpenSubsonic requires clients to be able to ask before signing in (D-075).
async fn extensions() -> Answer {
    let extension = |name: &'static str| {
        Node::new("openSubsonicExtensions")
            .attr("name", name)
            .scalars("versions", vec![1.into()])
    };
    Answer::node(Node::fragment().list(
        "openSubsonicExtensions",
        vec![
            extension("apiKeyAuthentication"),
            extension("formPost"),
            extension("indexBasedQueue"),
        ],
    ))
}

async fn token_info(State(app): State<AppState>, params: Params) -> Result<Answer, SubsonicError> {
    let key = params.required("apiKey")?;
    let owner = kima_db::api_keys::find_by_hash(&app.pool, &kima_core::app_token::hash(key))
        .await?
        .ok_or_else(SubsonicError::invalid_api_key)?;
    Ok(Answer::node(
        Node::new("tokenInfo").attr("username", owner.user.username),
    ))
}
