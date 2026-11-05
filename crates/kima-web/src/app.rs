//! The root component: routing, the auth guard and the page frame.
//!
//! Login and setup are bare pages; every other route is drawn inside the signed-in shell (`shell::frame`),
//! which keeps the `<audio>` and the player bars alive while the page inside it changes. The shell depends
//! on the route only, never on the session, so the server renders the same frame the browser ends up with
//! (a visitor without a session is sent to /login after hydration).

use std::rc::Rc;

use vertigo::router::Router;
use vertigo::{Computed, DomNode, Reactive, Value, bind, dom, get_driver};

use crate::api::client::ApiClient;
use crate::auth::{BrowserStorage, Session, effective_route};
use crate::pages;
use crate::player::Player;
use crate::route::Route;
use crate::shell;
use crate::style::stylesheet;
use crate::ui::Ctx;

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
        playlists_rev: Value::new(0),
        query: Value::new(String::new()),
        menu_open: Value::new(false),
        toast: Value::new(None),
    };

    // (requested, effective). On the server there is no session: the requested route is rendered.
    let routes: Computed<(Route, Route)> = {
        let (router, token) = (router.clone(), session.token());
        Computed::from(move |c| {
            let requested = router.get(c);
            let effective = if get_driver().is_browser() {
                effective_route(&requested, token.get(c).is_some())
            } else {
                requested.clone()
            };
            (requested, effective)
        })
    };
    // The shell is rebuilt only when switching between a bare page and an app page, not on every navigation.
    let bare: Computed<bool> =
        routes.map(|(_, effective)| matches!(effective, Route::Login | Route::Setup));

    let page = bind!(ctx, routes, || -> DomNode {
        routes.render_value(bind!(ctx, |(requested, effective): (Route, Route)| {
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
                Route::Playlists => pages::playlists::list::render(&ctx),
                Route::Playlist { id } => pages::playlists::detail::render(&ctx, id),
                Route::Settings => pages::settings::render(&ctx),
                Route::NotFound => pages::not_found(),
            }
        }))
    });

    let body = bare.render_value(bind!(ctx, page, |bare: bool| {
        if bare {
            ctx.player.stop();
            page()
        } else {
            shell::frame(&ctx, page())
        }
    }));

    dom! {
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
                <meta name="theme-color" content="#000000" />
                <title>"kima"</title>
                <style>{stylesheet()}</style>
            </head>
            <body class="antialiased">
                {body}
            </body>
        </html>
    }
}
