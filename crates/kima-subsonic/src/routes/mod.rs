use axum::Router;
use axum::handler::Handler;
use axum::http::Uri;
use axum::routing::get;

use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;

mod browse;
mod system;

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

pub fn router() -> Router<SubsonicCtx> {
    let router = Router::new();
    let router = system::add_routes(router);
    let router = browse::add_routes(router);
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
