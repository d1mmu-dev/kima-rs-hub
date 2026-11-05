//! `/settings`: the account slice — who you are and a password change. Everything else on the legacy
//! Settings page is M10.

use kima_api_types::auth::{ChangePassword, ChangePasswordResponse, MeResponse};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::ui::{BTN_OUTLINE, BTN_PRIMARY, CARD, Ctx, INPUT, LABEL, flag, resource_view};

/// The backend's rule (6+ characters) and the confirm field, checked before a round trip; the backend's
/// own message is shown verbatim for anything else (e.g. a wrong current password).
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
                <p>"Signed in as " <strong>{username}</strong></p>
                <p class="text-xs tabular-nums text-[var(--text-secondary)]">"Role: " {role}</p>
            </div>
        }
    });
    dom! {
        <div>
            <h2 class="text-2xl font-bold text-white mb-1">"Settings"</h2>
            <div class={CARD}>
                <h3 class="text-lg font-bold text-white mb-4">"Account"</h3>
                {account}
                {password_form(ctx)}
            </div>
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
        Some(message) => {
            dom! { <p class="text-sm text-danger-light" id="password-error">{message}</p> }
        }
        None => dom! { <span /> },
    });
    let done_view = done.render_value(|d| match d {
        Some(message) => {
            dom! { <p class="text-sm text-success-light" id="password-done">{message}</p> }
        }
        None => dom! { <span /> },
    });

    dom! {
        <form class={CARD} on_submit={submit}>
            <h3 class="text-lg font-bold text-white mb-4">"Change password"</h3>
            <div class="space-y-4">
                <div>
                    <label for="settings-current" class={LABEL}>"Current password"</label>
                    <input id="settings-current" type="password" name="current" autocomplete="current-password"
                        class={INPUT} value={current.clone()} on_input={bind!(current, |v: String| current.set(v))} />
                </div>
                <div>
                    <label for="settings-new" class={LABEL}>"New password (6+ characters)"</label>
                    <input id="settings-new" type="password" name="new" autocomplete="new-password"
                        class={INPUT} value={new.clone()} on_input={bind!(new, |v: String| new.set(v))} />
                </div>
                <div>
                    <label for="settings-confirm" class={LABEL}>"Confirm new password"</label>
                    <input id="settings-confirm" type="password" name="confirm" autocomplete="new-password"
                        class={INPUT} value={confirm.clone()} on_input={bind!(confirm, |v: String| confirm.set(v))} />
                </div>
            </div>
            {error_view}
            {done_view}
            <div class="flex gap-3 mt-4">
                <button type="submit" class={BTN_PRIMARY} disabled={flag(busy.to_computed())}>"Change password"</button>
                <button type="button" class={BTN_OUTLINE}>"Cancel"</button>
            </div>
        </form>
    }
}
