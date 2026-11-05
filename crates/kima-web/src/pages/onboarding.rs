//! `/setup`: create the first admin account (only while the backend reports no account), sign in and
//! mark onboarding complete so the library opens straight away.

use kima_api_types::auth::{Credentials, RegisterResponse};
use kima_api_types::onboarding::OnboardingStatus;
use serde_json::Value as Json;
use vertigo::{DomNode, Resource, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, card_css, error_css, flag, muted_css};

/// Mirrors the backend's rules (3-50 character username, 6+ character password) so the common
/// mistakes are caught without a round trip; the backend's own message wins for anything else.
pub fn validate(username: &str, password: &str, confirm: &str) -> Result<Credentials, String> {
    let username = username.trim();
    if !(3..=50).contains(&username.chars().count()) {
        return Err("Username must be 3 to 50 characters".into());
    }
    if password.chars().count() < 6 {
        return Err("Password must be at least 6 characters".into());
    }
    if password != confirm {
        return Err("Passwords do not match".into());
    }
    Ok(Credentials {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

pub fn render(ctx: &Ctx) -> DomNode {
    let status = load(bind!(ctx, async move {
        ctx.client
            .get::<OnboardingStatus>("/api/onboarding/status")
            .await
    }));
    status.render_value(bind!(ctx, |state| match state {
        Resource::Loading => dom! { <p css={muted_css()}>"Loading…"</p> },
        Resource::Error(message) => dom! { <p css={error_css()}>{message}</p> },
        Resource::Ready(s) if s.has_account => {
            // An admin exists already: registration happens elsewhere, sign in instead.
            let router = ctx.router.clone();
            get_driver().spawn(async move { router.set(Route::Login) });
            dom! { <p css={muted_css()}>"An account already exists. Redirecting to sign in…"</p> }
        }
        Resource::Ready(_) => form(&ctx),
    }))
}

fn form(ctx: &Ctx) -> DomNode {
    let username = Value::new(String::new());
    let password = Value::new(String::new());
    let confirm = Value::new(String::new());
    let error: Value<Option<String>> = Value::new(None);
    let busy = Value::new(false);

    let submit = bind!(ctx, username, password, confirm, error, busy, || {
        let creds = match validate(
            &vertigo::transaction(|c| username.get(c)),
            &vertigo::transaction(|c| password.get(c)),
            &vertigo::transaction(|c| confirm.get(c)),
        ) {
            Ok(c) => c,
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        error.set(None);
        busy.set(true);
        get_driver().spawn(bind!(ctx, error, busy, async move {
            match ctx
                .client
                .post::<_, RegisterResponse>("/api/onboarding/register", &creds)
                .await
            {
                Ok(done) => {
                    // The new token must be in place before the authenticated `complete` call.
                    ctx.session.sign_in(&done.token);
                    let _ = ctx
                        .client
                        .post_empty::<Json>("/api/onboarding/complete")
                        .await;
                }
                Err(err) => error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });

    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p css={error_css()}>{message}</p> },
        None => dom! { <span /> },
    });

    dom! {
        <form css={card_css()} on_submit={submit}>
            <h2>"Welcome to Kima"</h2>
            <p css={muted_css()}>"Create the administrator account."</p>
            <input type="text" name="username" placeholder="Username" autocomplete="username"
                value={username.clone()} on_input={bind!(username, |v: String| username.set(v))} />
            <input type="password" name="password" placeholder="Password (6+ characters)" autocomplete="new-password"
                value={password.clone()} on_input={bind!(password, |v: String| password.set(v))} />
            <input type="password" name="confirm" placeholder="Repeat password" autocomplete="new-password"
                value={confirm.clone()} on_input={bind!(confirm, |v: String| confirm.set(v))} />
            {error_view}
            <button type="submit" disabled={flag(busy.to_computed())}>"Create account"</button>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_the_backend_rules() {
        assert!(validate("ab", "secret1", "secret1").is_err());
        assert!(validate("admin", "short", "short").is_err());
        assert_eq!(
            validate("admin", "secret1", "other").unwrap_err(),
            "Passwords do not match"
        );
        assert_eq!(
            validate(" admin ", "secret1", "secret1").unwrap().username,
            "admin"
        );
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // Three characters, six bytes.
        assert!(validate("żół", "secret1", "secret1").is_ok());
    }
}
