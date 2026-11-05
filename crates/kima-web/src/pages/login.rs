//! `/login`: username + password, `POST /api/auth/login`, the backend's error message shown verbatim.

use kima_api_types::auth::{Credentials, LoginResponse};
use kima_api_types::onboarding::OnboardingStatus;
use vertigo::{DomNode, Resource, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::route::Route;
use crate::shell::kima_mark;
use crate::ui::{BTN_PRIMARY, Ctx, INPUT, LABEL, flag};

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
        dom! { <span class="hidden" /> }
    }));

    let error_view = error.render_value(|e| match e {
        Some(message) => dom! {
            <div role="alert" class="p-3 rounded-lg bg-danger/10 border border-danger/30 text-sm text-danger-light">{message}</div>
        },
        None => dom! { <span class="hidden" /> },
    });
    let disabled = busy.to_computed();
    // Only when registration is open; nothing while loading, on an error or when it is closed (the legacy page
    // shows nothing either), so the card does not change height when the status arrives.
    let setup_hint = status.render_value(|state| match state {
        Resource::Ready(s) if s.registration_open => dom! {
            <a class="block mt-4 text-center text-sm text-[var(--text-secondary)] hover:text-white" href={Route::Setup.to_string()}>"Create an account"</a>
        },
        _ => dom! { <span class="hidden" /> },
    });

    dom! {
        <div class="min-h-screen w-full relative overflow-hidden">
            <div class="absolute inset-0 bg-[#000]">
                <div class="absolute inset-0 bg-gradient-to-br from-brand/5 via-transparent to-transparent"></div>
                <div class="opacity-[0.08]">
                    <div class="fixed inset-0 pointer-events-none z-0 overflow-hidden">
                        <div class="absolute inset-0 bg-gradient-to-t to-transparent" style="background-image: linear-gradient(to top, rgba(252, 162, 0, 0.15), rgba(252, 162, 0, 0.05), transparent);"></div>
                        <div class="absolute inset-0 bg-gradient-to-t from-black/40 via-black/10 to-transparent"></div>
                    </div>
                </div>
            </div>
            <div class="relative z-(--z-raised) min-h-screen flex items-center justify-center p-4">
                <div class="w-full max-w-md">
                    <div class="flex items-center justify-center mb-8">
                        <div class="relative flex gap-3 items-center group">
                            <div class="relative">
                                <div class="absolute inset-0 bg-white/10 blur-xl rounded-full group-hover:bg-white/20 transition-all duration-300"></div>
                                {kima_mark(40, "kima-mark inline-block shrink-0 relative z-(--z-raised)")}
                            </div>
                            <span class="text-3xl font-bold text-white">"Kima"</span>
                        </div>
                    </div>
                    <div class="bg-[#111]/90 rounded-lg p-6 md:p-8 border border-white/10 animate-rise transition-shadow duration-200">
                        <h1 class="text-2xl font-bold text-white mb-1 text-center">"Welcome back"</h1>
                        <p class="text-[var(--text-secondary)] text-center mb-8">"Sign in to continue to Kima"</p>
                        <form class="space-y-4" on_submit={submit}>
                            <div>
                                <label for="username" class={LABEL}>"Username"</label>
                                <input id="username" type="text" name="username" placeholder="Enter your username" autocomplete="username"
                                    autocapitalize="none" class={format!("{INPUT} ")}
                                    value={username.clone()} on_input={bind!(username, |v: String| username.set(v))} />
                            </div>
                            <div>
                                <label for="password" class={LABEL}>"Password"</label>
                                <input id="password" type="password" name="password" placeholder="Enter your password" autocomplete="current-password"
                                    autocapitalize="none" class={format!("{INPUT} ")}
                                    value={password.clone()} on_input={bind!(password, |v: String| password.set(v))} />
                            </div>
                            {error_view}
                            <button type="submit" class={format!("{BTN_PRIMARY} w-full")} disabled={flag(disabled)}>"Sign In"</button>
                        </form>
                        {first_run}
                        {setup_hint}
                    </div>
                    <p class="text-center text-[var(--text-muted)] text-sm mt-6">"© 2026 Kima. Your music, your way."</p>
                </div>
            </div>
        </div>
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
