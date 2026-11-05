//! `/login`: username + password, `POST /api/auth/login`, the backend's error message shown verbatim.

use kima_api_types::auth::{Credentials, LoginResponse};
use kima_api_types::onboarding::OnboardingStatus;
use vertigo::{DomNode, Resource, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, card_css, error_css, flag, muted_css, resource_view};

/// The form's input check; the server stays the authority (its message is shown for anything else).
pub fn validate(username: &str, password: &str) -> Result<Credentials, String> {
    let username = username.trim();
    if username.is_empty() || password.is_empty() {
        return Err("Enter a username and a password".into());
    }
    Ok(Credentials {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

pub fn render(ctx: &Ctx) -> DomNode {
    let username = Value::new(String::new());
    let password = Value::new(String::new());
    let error: Value<Option<String>> = Value::new(None);
    let busy = Value::new(false);

    let submit = bind!(ctx, username, password, error, busy, || {
        let creds = match validate(
            &vertigo::transaction(|c| username.get(c)),
            &vertigo::transaction(|c| password.get(c)),
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
                .post::<_, LoginResponse>("/api/auth/login", &creds)
                .await
            {
                // The guard sees the new token and moves on to the library.
                Ok(done) => ctx.session.sign_in(&done.token),
                Err(err) => error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });

    // First run (no account yet): the only useful page is /setup.
    let status = load(bind!(ctx, async move {
        ctx.client
            .get::<OnboardingStatus>("/api/onboarding/status")
            .await
    }));
    let first_run = status.render_value(bind!(ctx, |state| {
        if let Resource::Ready(s) = state
            && s.needs_onboarding
            && !s.has_account
        {
            let router = ctx.router.clone();
            get_driver().spawn(async move { router.set(Route::Setup) });
        }
        dom! { <span /> }
    }));

    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p css={error_css()}>{message}</p> },
        None => dom! { <span /> },
    });
    let disabled = busy.to_computed();
    let setup_hint = resource_view(status, |s| {
        if s.registration_open {
            dom! { <a href={Route::Setup.to_string()}>"Create an account"</a> }
        } else {
            dom! { <span css={muted_css()}>"Registration is closed"</span> }
        }
    });

    dom! {
        <form css={card_css()} on_submit={submit}>
            <h2>"Sign in to Kima"</h2>
            <input type="text" name="username" placeholder="Username" autocomplete="username"
                value={username.clone()} on_input={bind!(username, |v: String| username.set(v))} />
            <input type="password" name="password" placeholder="Password" autocomplete="current-password"
                value={password.clone()} on_input={bind!(password, |v: String| password.set(v))} />
            {error_view}
            <button type="submit" disabled={flag(disabled)}>"Sign in"</button>
            {first_run}
            {setup_hint}
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fields_are_rejected_before_any_request() {
        assert!(validate("", "x").is_err());
        assert!(validate("  ", "x").is_err());
        assert!(validate("a", "").is_err());
    }

    #[test]
    fn username_is_trimmed_password_is_not() {
        let c = validate("  admin ", " p w ").unwrap();
        assert_eq!(c.username, "admin");
        assert_eq!(c.password, " p w ");
    }
}
