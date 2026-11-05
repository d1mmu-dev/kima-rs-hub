//! Session storage and the auth guard's decision.
//!
//! The token lives in `localStorage` under [`STORAGE_KEY`], a key only kima-web uses (owner decision;
//! no cookie). The legacy Next.js app uses `auth_token` / `refresh_token`, so the two frontends never
//! read or overwrite each other's session during the side-by-side period.

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::rc::Rc;

use vertigo::{Computed, JsJson, Value, get_driver, js};

use crate::route::Route;

pub const STORAGE_KEY: &str = "kima-web.token";

/// Where the token is persisted. The browser implementation talks to `window.localStorage`; tests use
/// [`MemoryStorage`].
pub trait Storage {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str);
    fn remove(&self, key: &str);
}

pub struct BrowserStorage;

impl Storage for BrowserStorage {
    fn get(&self, key: &str) -> Option<String> {
        // No `window` while rendering on the server: there is simply no session there.
        if !get_driver().is_browser() {
            return None;
        }
        match js! { window.localStorage.getItem(key) } {
            JsJson::String(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    fn set(&self, key: &str, value: &str) {
        if get_driver().is_browser() {
            js! { window.localStorage.setItem(key, value) };
        }
    }

    fn remove(&self, key: &str) {
        if get_driver().is_browser() {
            js! { window.localStorage.removeItem(key) };
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryStorage(RefCell<HashMap<String, String>>);

#[cfg(test)]
impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.0.borrow().get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) {
        self.0.borrow_mut().insert(key.into(), value.into());
    }
    fn remove(&self, key: &str) {
        self.0.borrow_mut().remove(key);
    }
}

/// The signed-in state: a reactive token mirrored into storage.
#[derive(Clone)]
pub struct Session {
    token: Value<Option<String>>,
    storage: Rc<dyn Storage>,
}

impl Session {
    pub fn new(storage: Rc<dyn Storage>) -> Self {
        Self {
            token: Value::new(storage.get(STORAGE_KEY)),
            storage,
        }
    }

    pub fn token(&self) -> Computed<Option<String>> {
        self.token.to_computed()
    }

    pub fn current_token(&self) -> Option<String> {
        vertigo::transaction(|ctx| self.token.get(ctx))
    }

    pub fn sign_in(&self, token: &str) {
        self.storage.set(STORAGE_KEY, token);
        self.token.set(Some(token.to_owned()));
    }

    /// Sign out locally (explicit sign-out, or the server answered 401).
    pub fn clear(&self) {
        self.storage.remove(STORAGE_KEY);
        self.token.set(None);
    }
}

/// The guard: what to actually show for a requested route.
/// Guarded routes need a session (else `/login`); `/login` and `/setup` with a session go to the library
/// (right after registering, the session exists and the setup form must leave). Without a session
/// `/setup` is reachable; the page itself redirects to `/login` once an admin exists.
pub fn effective_route(requested: &Route, signed_in: bool) -> Route {
    match (requested, signed_in) {
        (Route::Login | Route::Setup, true) => Route::Artists,
        (r, false) if r.requires_auth() => Route::Login,
        (r, _) => r.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_routes_render_login_without_a_token() {
        for r in [
            Route::Artists,
            Route::Albums { artist: None },
            Route::Tracks { album: None },
            Route::Search,
            Route::Playlists,
        ] {
            assert_eq!(effective_route(&r, false), Route::Login);
            assert_eq!(effective_route(&r, true), r);
        }
    }

    #[test]
    fn public_routes_and_login_redirect() {
        assert_eq!(effective_route(&Route::Setup, false), Route::Setup);
        assert_eq!(effective_route(&Route::Login, false), Route::Login);
        assert_eq!(effective_route(&Route::Login, true), Route::Artists);
        assert_eq!(effective_route(&Route::Setup, true), Route::Artists);
        assert_eq!(effective_route(&Route::NotFound, false), Route::NotFound);
    }

    #[test]
    fn session_persists_and_clears_through_storage() {
        let storage = Rc::new(MemoryStorage::default());
        let session = Session::new(storage.clone());
        assert_eq!(session.current_token(), None);
        session.sign_in("abc");
        assert_eq!(storage.get(STORAGE_KEY).as_deref(), Some("abc"));
        // A fresh Session (page reload) picks the stored token up.
        assert_eq!(
            Session::new(storage.clone()).current_token().as_deref(),
            Some("abc")
        );
        // A 401 handler calls clear(): token gone from memory and storage.
        session.clear();
        assert_eq!(session.current_token(), None);
        assert_eq!(storage.get(STORAGE_KEY), None);
    }

    #[test]
    fn storage_key_is_not_the_legacy_frontends() {
        assert_ne!(STORAGE_KEY, "auth_token");
        assert_ne!(STORAGE_KEY, "refresh_token");
    }
}
