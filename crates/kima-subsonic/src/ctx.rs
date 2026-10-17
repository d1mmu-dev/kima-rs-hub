use axum::extract::FromRef;
use kima_api::AppState;

/// What Subsonic handlers share. Later tasks add the now-playing map, the password cache, etc.
#[derive(Clone)]
pub struct SubsonicCtx {
    pub app: AppState,
}

impl FromRef<SubsonicCtx> for AppState {
    fn from_ref(ctx: &SubsonicCtx) -> AppState {
        ctx.app.clone()
    }
}
