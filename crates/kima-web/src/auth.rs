//! Session storage and the auth guard's decision.
//!
//! The token lives in `localStorage` under [`STORAGE_KEY`], a key only kima-web uses (owner decision;
//! no cookie). The legacy Next.js app uses `auth_token` / `refresh_token`, so the two frontends never
//! read or overwrite each other's session during the side-by-side period.

use std::cell::Cell;
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::rc::Rc;

use vertigo::{Computed, JsJson, Value, get_driver, js};

use crate::route::Route;

pub const STORAGE_KEY: &str = "kima-web.session";
const LEGACY_TOKEN_KEY: &str = "kima-web.token";
const LEGACY_REFRESH_KEY: &str = "kima-web.refresh";

#[derive(serde::Deserialize)]
struct StoredSession {
    token: String,
    #[serde(rename = "refresh")]
    _refresh: Option<String>,
    generation: String,
}

fn generation() -> String {
    // Ownership nonce, not an authentication secret. Use Vertigo's browser RNG so this
    // also works on self-hosted HTTP origins without secure-context crypto.randomUUID.
    #[cfg(target_arch = "wasm32")]
    {
        let driver = get_driver();
        format!(
            "{:08x}{:08x}{:08x}{:08x}",
            driver.get_random(0, u32::MAX),
            driver.get_random(0, u32::MAX),
            driver.get_random(0, u32::MAX),
            driver.get_random(0, u32::MAX)
        )
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Native tests / SSR (which never persists credentials).
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string()
    }
}

pub fn login_destination(profile_count: usize) -> &'static str {
    if profile_count > 1 {
        "/profiles"
    } else {
        "/collection"
    }
}

/// Fail closed for older servers or an unknown profile kind.
pub fn is_main(profile: Option<&kima_api_types::profiles::Profile>) -> bool {
    profile.is_some_and(|p| p.is_main && p.kind == kima_api_types::profiles::ProfileKind::Main)
}

pub fn is_admin(me: &kima_api_types::auth::MeResponse) -> bool {
    me.role == "admin" && is_main(me.profile.as_ref())
}

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
    owned_record: Rc<RefCell<Option<String>>>,
    storage: Rc<dyn Storage>,
    switching: Rc<Cell<bool>>,
    profile: Rc<RefCell<Option<kima_api_types::profiles::Profile>>>,
}

impl Session {
    pub fn new(storage: Rc<dyn Storage>) -> Self {
        let record = storage.get(STORAGE_KEY);
        let token = record.as_deref().and_then(|raw| {
            let parsed: StoredSession = serde_json::from_str(raw).ok()?;
            (!parsed.token.is_empty() && !parsed.generation.is_empty()).then_some(parsed.token)
        });
        let session = Self {
            token: Value::new(token),
            owned_record: Rc::new(RefCell::new(record.clone())),
            storage,
            switching: Rc::new(Cell::new(false)),
            profile: Rc::new(RefCell::new(None)),
        };
        // One-way migration. An existing record is authoritative even when corrupt: never
        // resurrect stale legacy credentials. Old tabs running old code are not coordinated.
        if record.is_none()
            && let Some(token) = session.storage.get(LEGACY_TOKEN_KEY)
        {
            let refresh = session.storage.get(LEGACY_REFRESH_KEY);
            session.persist(&token, refresh.as_deref());
        }
        // Also discard leftovers when the authoritative record already exists, otherwise
        // clearing it could resurrect an older credential pair on the following reload.
        session.storage.remove(LEGACY_TOKEN_KEY);
        session.storage.remove(LEGACY_REFRESH_KEY);
        session
    }

    /// The backing storage, for other kima-web-owned keys (the player's resume position).
    pub fn storage(&self) -> Rc<dyn Storage> {
        self.storage.clone()
    }

    pub fn begin_switch(&self) -> bool {
        !self.switching.replace(true)
    }

    pub fn end_switch(&self) {
        self.switching.set(false);
    }

    pub fn token(&self) -> Computed<Option<String>> {
        self.token.to_computed()
    }

    pub fn current_token(&self) -> Option<String> {
        vertigo::transaction(|ctx| self.token.get(ctx))
    }

    pub fn sign_in(&self, token: &str) {
        // Password replacement / onboarding may issue only an access token. Never attach
        // a refresh credential belonging to a different tab or an earlier token version.
        self.persist(token, None);
    }

    fn persist(&self, token: &str, refresh: Option<&str>) {
        let record = serde_json::json!({
            "token": token, "refresh": refresh, "generation": generation()
        })
        .to_string();
        // setItem publishes one complete pair; concurrent publishers are last-writer-wins.
        self.storage.set(STORAGE_KEY, &record);
        *self.owned_record.borrow_mut() = Some(record);
        self.token.set(Some(token.to_owned()));
    }

    /// Persist both credentials; the switch caller replaces the document at the returned URL.
    pub fn replace_pair(&self, token: &str, refresh: &str) -> &'static str {
        self.persist(token, Some(refresh));
        "/"
    }

    pub fn remember_profile(&self, profile: &kima_api_types::profiles::Profile) {
        *self.profile.borrow_mut() = Some(profile.clone());
        crate::position::migrate_legacy(
            self.storage.as_ref(),
            &profile.id.to_string(),
            is_main(Some(profile)),
        );
    }

    pub fn position_key(&self) -> Option<String> {
        // Bound to this tab's authenticated session, never another tab's localStorage metadata.
        self.profile
            .borrow()
            .as_ref()
            .map(|profile| crate::position::profile_key(&profile.id.to_string()))
    }

    /// Invalidate this tab; remove shared credentials only if they still belong to it.
    /// This synchronous comparison/removal has no await, but localStorage has no cross-tab CAS:
    /// a simultaneous writer can still race it. Already-published newer sessions are preserved.
    pub fn clear(&self) {
        if self.owned_record.borrow().is_some()
            && self.storage.get(STORAGE_KEY) == *self.owned_record.borrow()
        {
            self.storage.remove(STORAGE_KEY);
        }
        self.invalidate_tab();
    }

    fn invalidate_tab(&self) {
        *self.owned_record.borrow_mut() = None;
        *self.profile.borrow_mut() = None;
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
    fn switch_replaces_token_and_requests_reload() {
        let storage = Rc::new(MemoryStorage::default());
        let session = Session::new(storage.clone());
        session.sign_in("old");
        assert_eq!(session.replace_pair("new", "refresh"), "/");
        assert_eq!(session.current_token().as_deref(), Some("new"));
        let record: StoredSession =
            serde_json::from_str(&storage.get(STORAGE_KEY).unwrap()).unwrap();
        assert_eq!(record._refresh.as_deref(), Some("refresh"));
        session.clear();
        assert_eq!(storage.get(STORAGE_KEY), None);
    }

    #[test]
    fn stale_401_cannot_erase_another_tabs_newer_session() {
        let storage = Rc::new(MemoryStorage::default());
        let kids = Session::new(storage.clone());
        kids.replace_pair("kids", "kids-refresh");
        let main = Session::new(storage.clone());
        main.replace_pair("main", "main-refresh");
        kids.clear(); // Deleted Kids gets 401 after main has switched.
        assert_eq!(kids.current_token(), None);
        assert_eq!(main.current_token().as_deref(), Some("main"));
        assert_eq!(
            Session::new(storage).current_token().as_deref(),
            Some("main")
        );
    }

    #[test]
    fn pair_is_published_in_one_complete_storage_write() {
        #[derive(Default)]
        struct ObservedStorage {
            memory: MemoryStorage,
            writes: RefCell<Vec<(String, String)>>,
        }
        impl Storage for ObservedStorage {
            fn get(&self, key: &str) -> Option<String> {
                self.memory.get(key)
            }
            fn set(&self, key: &str, value: &str) {
                self.memory.set(key, value);
                self.writes.borrow_mut().push((key.into(), value.into()));
            }
            fn remove(&self, key: &str) {
                self.memory.remove(key);
            }
        }
        let storage = Rc::new(ObservedStorage::default());
        let session = Session::new(storage.clone());
        session.replace_pair("kids", "kids-refresh");
        storage.writes.borrow_mut().clear();
        session.replace_pair("main", "main-refresh");
        let writes = storage.writes.borrow();
        assert_eq!(
            writes.len(),
            1,
            "observers must never see a split credential pair"
        );
        let record: serde_json::Value = serde_json::from_str(&writes[0].1).unwrap();
        assert_eq!(record["token"], "main");
        assert_eq!(record["refresh"], "main-refresh");
        assert!(record["generation"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn identical_tokens_in_a_new_generation_are_not_owned_by_the_old_tab() {
        let storage = Rc::new(MemoryStorage::default());
        let old = Session::new(storage.clone());
        old.replace_pair("same-access", "same-refresh");
        let newer = Session::new(storage.clone());
        newer.replace_pair("same-access", "same-refresh");
        let record = storage.get(STORAGE_KEY);
        old.clear();
        old.clear(); // Repeated callbacks from the invalidated tab are harmless too.
        assert_eq!(storage.get(STORAGE_KEY), record);
        assert_eq!(
            Session::new(storage).current_token().as_deref(),
            Some("same-access")
        );
    }

    #[test]
    fn legacy_pair_migrates_once_and_cannot_resurrect_after_sign_out() {
        let storage = Rc::new(MemoryStorage::default());
        storage.set(LEGACY_TOKEN_KEY, "legacy");
        storage.set(LEGACY_REFRESH_KEY, "legacy-refresh");
        let session = Session::new(storage.clone());
        assert_eq!(session.current_token().as_deref(), Some("legacy"));
        let record: StoredSession =
            serde_json::from_str(&storage.get(STORAGE_KEY).unwrap()).unwrap();
        assert_eq!(record._refresh.as_deref(), Some("legacy-refresh"));
        assert_eq!(storage.get(LEGACY_TOKEN_KEY), None);
        assert_eq!(storage.get(LEGACY_REFRESH_KEY), None);
        session.clear();
        assert_eq!(Session::new(storage).current_token(), None);
    }

    #[test]
    fn authoritative_record_ignores_stale_legacy_pair_and_token_only_write_drops_refresh() {
        let storage = Rc::new(MemoryStorage::default());
        let session = Session::new(storage.clone());
        session.replace_pair("new", "new-refresh");
        storage.set(LEGACY_TOKEN_KEY, "stale");
        storage.set(LEGACY_REFRESH_KEY, "stale-refresh");
        let loaded = Session::new(storage.clone());
        assert_eq!(loaded.current_token().as_deref(), Some("new"));
        loaded.clear();
        assert_eq!(Session::new(storage.clone()).current_token(), None);
        session.sign_in("password-changed");
        let record: StoredSession =
            serde_json::from_str(&storage.get(STORAGE_KEY).unwrap()).unwrap();
        assert_eq!(record._refresh, None);
        storage.set(STORAGE_KEY, "corrupt");
        assert_eq!(Session::new(storage).current_token(), None);
    }

    #[test]
    fn guard_sends_multi_profile_login_to_picker() {
        assert_eq!(login_destination(2), "/profiles");
        assert_eq!(login_destination(5), "/profiles");
    }

    #[test]
    fn guard_skips_picker_for_single_profile() {
        assert_eq!(login_destination(1), "/collection");
    }

    #[test]
    fn only_one_profile_switch_can_be_in_flight_across_entry_points() {
        let session = Session::new(Rc::new(MemoryStorage::default()));
        let second = session.clone();
        assert!(session.begin_switch());
        assert!(!second.begin_switch());
        session.end_switch();
        assert!(second.begin_switch());
    }

    #[test]
    fn another_tabs_profile_cannot_change_this_players_resume_namespace() {
        let storage = Rc::new(MemoryStorage::default());
        let main = Session::new(storage.clone());
        let kids = Session::new(storage);
        let mut profile: kima_api_types::profiles::Profile = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-0000-0000-000000000001","name":"Main","kind":"main","isMain":true,"avatar":null,"createdAt":"now"
        })).unwrap();
        main.remember_profile(&profile);
        let main_key = main.position_key();
        profile.id = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        profile.is_main = false;
        profile.kind = kima_api_types::profiles::ProfileKind::Kids;
        kids.remember_profile(&profile);
        assert_eq!(main.position_key(), main_key);
        assert_ne!(main.position_key(), kids.position_key());
    }

    #[test]
    fn admin_controls_require_admin_account_and_main_profile() {
        let mut me: kima_api_types::auth::MeResponse = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001", "username": "a", "role": "admin",
            "onboardingComplete": true, "createdAt": "now",
            "profile": {"id": "00000000-0000-0000-0000-000000000001", "name": "a", "kind": "kids", "avatar": null, "isMain": false, "createdAt": "now"}
        })).unwrap();
        assert!(!is_admin(&me));
        let p = me.profile.as_mut().unwrap();
        p.is_main = true;
        p.kind = kima_api_types::profiles::ProfileKind::Main;
        assert!(is_admin(&me));
        me.role = "user".into();
        assert!(!is_admin(&me));
        me.profile = None;
        assert!(!is_main(me.profile.as_ref()));
    }

    #[test]
    fn guarded_routes_render_login_without_a_token() {
        for r in [
            Route::Artists,
            Route::Albums { artist: None },
            Route::Tracks { album: None },
            Route::Search,
            Route::Playlists,
            Route::Settings,
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
        assert!(storage.get(STORAGE_KEY).is_some());
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
