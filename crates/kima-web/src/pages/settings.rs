//! `/settings`: the account slice (spec §5.7): who you are and a password change. Everything else on
//! the legacy Settings page is M10.

use kima_api_types::auth::{ChangePassword, ChangePasswordResponse, MeResponse};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::ui::{Ctx, card_css, error_css, flag, muted_css, resource_view};

/// The backend's rule (6+ characters) and the confirm field, checked before a round trip; the
/// backend's own message is shown verbatim for anything else (e.g. a wrong current password).
pub fn validate(current: &str, new: &str, confirm: &str) -> Result<ChangePassword, String> {
    if current.is_empty() {
        return Err("Enter your current password".into());
    }
    if new.chars().count() < 6 {
        return Err("New password must be at least 6 characters".into());
    }
    if new != confirm {
        return Err("Passwords do not match".into());
    }
    Ok(ChangePassword {
        current_password: current.to_owned(),
        new_password: new.to_owned(),
    })
}

pub fn render(ctx: &Ctx) -> DomNode {
    let me = load(bind!(ctx, async move {
        ctx.client.get::<MeResponse>("/api/auth/me").await
    }));
    let account = resource_view(me, |me: std::rc::Rc<MeResponse>| {
        let username = me.username.clone();
        let role = me.role.clone();
        dom! {
            <div>
                <p>"Signed in as " <strong id="account-username">{username}</strong></p>
                <p css={muted_css()}>{format!("Role: {role}")}</p>
            </div>
        }
    });
    dom! {
        <div>
            <h2>"Settings"</h2>
            <h3>"Account"</h3>
            {account}
            {password_form(ctx)}
        </div>
    }
}

fn password_form(ctx: &Ctx) -> DomNode {
    let current = Value::new(String::new());
    let new = Value::new(String::new());
    let confirm = Value::new(String::new());
    let error: Value<Option<String>> = Value::new(None);
    let done: Value<Option<String>> = Value::new(None);
    let busy = Value::new(false);

    let submit = bind!(ctx, current, new, confirm, error, done, busy, || {
        let body = match validate(
            &vertigo::transaction(|c| current.get(c)),
            &vertigo::transaction(|c| new.get(c)),
            &vertigo::transaction(|c| confirm.get(c)),
        ) {
            Ok(b) => b,
            Err(message) => {
                done.set(None);
                error.set(Some(message));
                return;
            }
        };
        error.set(None);
        done.set(None);
        busy.set(true);
        get_driver().spawn(bind!(
            ctx,
            current,
            new,
            confirm,
            error,
            done,
            busy,
            async move {
                match ctx
                    .client
                    .post::<_, ChangePasswordResponse>("/api/auth/change-password", &body)
                    .await
                {
                    Ok(answer) => {
                        // The backend revokes the old tokens; the reply carries the replacement.
                        ctx.session.sign_in(&answer.token);
                        current.set(String::new());
                        new.set(String::new());
                        confirm.set(String::new());
                        done.set(Some(answer.message));
                    }
                    Err(err) => error.set(Some(err.message)),
                }
                busy.set(false);
            }
        ));
    });

    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p css={error_css()} id="password-error">{message}</p> },
        None => dom! { <span /> },
    });
    let done_view = done.render_value(|d| match d {
        Some(message) => dom! { <p id="password-done">{message}</p> },
        None => dom! { <span /> },
    });

    dom! {
        <form css={card_css()} on_submit={submit}>
            <h3>"Change password"</h3>
            <input type="password" name="current" placeholder="Current password" autocomplete="current-password"
                value={current.clone()} on_input={bind!(current, |v: String| current.set(v))} />
            <input type="password" name="new" placeholder="New password (6+ characters)" autocomplete="new-password"
                value={new.clone()} on_input={bind!(new, |v: String| new.set(v))} />
            <input type="password" name="confirm" placeholder="Repeat new password" autocomplete="new-password"
                value={confirm.clone()} on_input={bind!(confirm, |v: String| confirm.set(v))} />
            {error_view}
            {done_view}
            <button type="submit" disabled={flag(busy.to_computed())}>"Change password"</button>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_the_backend_rules() {
        assert!(validate("", "secret1", "secret1").is_err());
        assert_eq!(
            validate("old", "short", "short").unwrap_err(),
            "New password must be at least 6 characters"
        );
        assert_eq!(
            validate("old", "secret1", "other").unwrap_err(),
            "Passwords do not match"
        );
        let ok = validate("old", " secret1 ", " secret1 ").unwrap();
        assert_eq!(ok.current_password, "old");
        // Passwords are sent as typed, spaces included.
        assert_eq!(ok.new_password, " secret1 ");
    }
}
