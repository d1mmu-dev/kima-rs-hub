//! The root component: routing, the auth guard, the shell (nav, scan, sign-out) and the player bar.

use std::rc::Rc;

use kima_api_types::library::{ScanStarted, ScanStatus};
use vertigo::router::Router;
use vertigo::{Computed, DomNode, Reactive, Value, bind, dom, get_driver};

use crate::api::client::ApiClient;
use crate::auth::{BrowserStorage, Session, effective_route};
use crate::pages;
use crate::player::Player;
use crate::route::Route;
use crate::ui::{Ctx, flag, global_css, muted_css, page_css};

/// Polls a scan job once a second for at most this many seconds.
const SCAN_POLL_SECS: u32 = 600;

pub fn render() -> DomNode {
    let session = Session::new(Rc::new(BrowserStorage));
    let router: Router<Route> = Router::new_history_router();
    // A 401 from any call drops the session; the guard below then shows /login.
    let client = ApiClient::new(
        session.token(),
        bind!(session, || {
            session.clear();
        }),
    );
    let player = Player::new(client.clone(), session.clone());
    let ctx = Ctx {
        client,
        session: session.clone(),
        router: router.clone(),
        player: player.clone(),
        refresh: Value::new(0),
    };

    // (requested, effective)
    let routes: Computed<(Route, Route)> = {
        let (router, token) = (router.clone(), session.token());
        Computed::from(move |c| {
            let requested = router.get(c);
            let effective = effective_route(&requested, token.get(c).is_some());
            (requested, effective)
        })
    };
    let page = routes.render_value(bind!(ctx, |(requested, effective): (Route, Route)| {
        if requested != effective {
            let url = effective.to_string();
            get_driver().spawn(async move { get_driver().history_replace(&url) });
        }
        match effective {
            Route::Login => pages::login::render(&ctx),
            Route::Setup => pages::onboarding::render(&ctx),
            Route::Artists => pages::artists::render(&ctx),
            Route::Albums { artist } => pages::albums::render(&ctx, artist),
            Route::Tracks { album } => pages::tracks::render(&ctx, album),
            Route::Search => pages::search::render(&ctx),
            Route::NotFound => dom! {
                <div css={page_css()}>
                    <h2>"Page not found"</h2>
                    <a href={Route::Artists.to_string()}>"Back to the library"</a>
                </div>
            },
        }
    }));

    let header =
        ctx.session
            .token()
            .map(|t| t.is_some())
            .render_value(bind!(ctx, |signed_in: bool| if signed_in {
                header(&ctx)
            } else {
                dom! { <span /> }
            }));

    dom! {
        <html>
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <title>"Kima"</title>
                <style>{global_css()}</style>
            </head>
            <body>
                {header}
                <main css={page_css()}>{page}</main>
                {player.render_for_session()}
            </body>
        </html>
    }
}

fn header(ctx: &Ctx) -> DomNode {
    let message = Value::new(String::new());
    let scanning = Value::new(false);

    let scan = bind!(ctx, message, scanning, |_| {
        scanning.set(true);
        message.set("Starting scan…".into());
        get_driver().spawn(bind!(ctx, message, scanning, async move {
            let outcome = scan_library(&ctx.client, &message).await;
            match outcome {
                Ok(()) => {
                    message.set("Scan finished".into());
                    ctx.refresh.change(|n| *n += 1);
                }
                Err(e) => message.set(e),
            }
            scanning.set(false);
        }));
    });
    let sign_out = bind!(ctx, |_| ctx.session.clear());

    dom! {
        <header css={crate::ui::row_css()}>
            <strong>"Kima"</strong>
            <a href={Route::Artists.to_string()}>"Artists"</a>
            <a href={Route::Albums { artist: None }.to_string()}>"Albums"</a>
            <a href={Route::Tracks { album: None }.to_string()}>"Tracks"</a>
            <a href={Route::Search.to_string()}>"Search"</a>
            <button on_click={scan} disabled={flag(scanning.to_computed())}>"Scan library"</button>
            <span css={muted_css()}>{message.to_computed()}</span>
            <button on_click={sign_out}>"Sign out"</button>
        </header>
    }
}

/// `POST /library/scan` (admin), then follow the job until it ends.
async fn scan_library(client: &ApiClient, message: &Value<String>) -> Result<(), String> {
    let started = client
        .post_empty::<ScanStarted>("/api/library/scan")
        .await
        .map_err(|e| e.message)?;
    let dir = started
        .music_path
        .unwrap_or_else(|| "the music directory".into());
    message.set(format!("Scanning {dir}…"));
    for _ in 0..SCAN_POLL_SECS {
        get_driver().sleep(1000).await;
        let status = client
            .get::<ScanStatus>(&format!("/api/library/scan/status/{}", started.job_id))
            .await
            .map_err(|e| e.message)?;
        match status.status.as_str() {
            "completed" => return Ok(()),
            "failed" => {
                return Err(format!(
                    "Scan failed: {}",
                    status.error.unwrap_or_else(|| "unknown error".into())
                ));
            }
            _ => {}
        }
    }
    Err("Scan is still running; refresh the page later".into())
}
