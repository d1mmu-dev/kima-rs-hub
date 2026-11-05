//! `/settings`: the account slice — who you are and a password change. Everything else on the legacy
//! Settings page is M10.

use kima_api_types::auth::{ChangePassword, ChangePasswordResponse, MeResponse};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::components::eyebrow;
use crate::ui::{Ctx, flag, resource_view};

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
    let show_form = Value::new(false);
    let signed_in = resource_view(me, |me: std::rc::Rc<MeResponse>| {
        let username = me.username.clone();
        let role = if me.role == "admin" {
            "Administrator"
        } else {
            "Member"
        };
        row(
            "Signed in as",
            dom! { <p class="text-xs tabular-nums text-[var(--text-muted)] mt-0.5">{role}</p> },
            dom! { <span id="account-username" class="text-sm text-white">{username}</span> },
        )
    });
    let password_row = show_form.render_value(bind!(ctx, show_form, |open: bool| {
        let (class, label) = if open {
            ("text-xs text-[var(--text-muted)] hover:text-white/70 transition-colors", "Cancel")
        } else {
            ("text-xs text-brand hover:text-brand-hover transition-colors", "Change")
        };
        let toggle = bind!(show_form, |_| {
            let now = vertigo::transaction(|c| show_form.get(c));
            show_form.set(!now);
        });
        let form = if open {
            password_form(&ctx)
        } else {
            dom! { <span class="hidden" /> }
        };
        dom! {
            <div>
                {row(
                    "Password",
                    dom! { <p class="text-xs tabular-nums text-[var(--text-muted)] mt-0.5">"Change your account password"</p> },
                    dom! { <button type="button" class={class} on_click={toggle}>{label}</button> },
                )}
                {form}
            </div>
        }
    }));
    dom! {
        <div class="min-h-screen bg-[var(--bg-primary)]">
            <header class="page-shell pt-8 pb-6">
                {eyebrow("System configuration")}
                <div class="flex flex-wrap items-end justify-between gap-4">
                    <div class="min-w-0">
                        <h1 class="text-4xl md:text-5xl font-bold tracking-tight text-white leading-none">"Settings"</h1>
                    </div>
                </div>
            </header>
            <div class="page-shell pb-8 flex gap-12">
                <nav class="w-48 shrink-0 sticky top-8 self-start hidden md:block" aria-label="Settings sections">
                    <div class="space-y-0.5">
                        <div aria-current="true" class="w-full min-h-[44px] flex items-center text-left px-3 py-2 rounded-lg text-sm tabular-nums transition-all text-brand bg-brand/10 border border-brand/20">"Account"</div>
                    </div>
                </nav>
                <div class="flex-1 min-w-0">
                    <div class="section-stack">
                        <section id="account" class="scroll-mt-24 animate-rise">
                            <div class="mb-4">
                                <div class="flex items-center gap-3 mb-1">
                                    <span aria-hidden="true" class="w-1 rounded-full shrink-0 h-6 bg-brand"></span>
                                    <h2 class="font-bold tracking-tight text-white text-lg">"Account"</h2>
                                    <span class="flex-1 border-t border-white/10"></span>
                                </div>
                            </div>
                            <div class="space-y-1">
                                {signed_in}
                                {password_row}
                            </div>
                            <div class="border-t border-white/5 mt-8 mb-8"></div>
                        </section>
                    </div>
                </div>
            </div>
        </div>
    }
}

/// `SettingsRow`: label and description on the left, the control on the right (stacked on phones).
fn row(label: &str, description: DomNode, control: DomNode) -> DomNode {
    let label = label.to_owned();
    dom! {
        <div class="flex flex-col gap-1.5 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4 sm:min-h-[56px]">
            <div class="sm:flex-1">
                <span class="text-sm font-medium text-white">{label}</span>
                {description}
            </div>
            <div class="w-full sm:w-auto sm:shrink-0">{control}</div>
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
            dom! { <p class="inline-flex items-center gap-1.5 text-sm text-danger-light" id="password-error">{message}</p> }
        }
        None => dom! { <span class="hidden" /> },
    });
    let done_view = done.render_value(|d| match d {
        Some(message) => {
            dom! { <p class="inline-flex items-center gap-1.5 text-sm text-success" id="password-done">{message}</p> }
        }
        None => dom! { <span class="hidden" /> },
    });

    const FIELD: &str = "w-full bg-white/5 text-white text-sm tabular-nums px-3 py-2 rounded-lg border border-white/10 outline-none focus:ring-2 focus:ring-brand/30 focus:border-brand/40 placeholder:text-[var(--text-muted)] transition-all hover:bg-white/[0.08] hover:border-white/20 focus:bg-white/[0.08]";
    dom! {
        <form class="py-4 space-y-3 border-t border-b border-white/5" on_submit={submit}>
            <div class="relative">
                <input id="settings-current" type="password" name="current" aria-label="Current password" placeholder="Current password" autocomplete="current-password"
                    class={FIELD} value={current.clone()} on_input={bind!(current, |v: String| current.set(v))} />
            </div>
            <div class="relative">
                <input id="settings-new" type="password" name="new" aria-label="New password (6+ characters)" placeholder="New password (min 6 characters)" autocomplete="new-password"
                    class={FIELD} value={new.clone()} on_input={bind!(new, |v: String| new.set(v))} />
            </div>
            <div class="relative">
                <input id="settings-confirm" type="password" name="confirm" aria-label="Confirm new password" placeholder="Confirm new password" autocomplete="new-password"
                    class={FIELD} value={confirm.clone()} on_input={bind!(confirm, |v: String| confirm.set(v))} />
            </div>
            <div class="inline-flex items-center gap-3">
                <button type="submit" class="inline-flex items-center justify-center gap-2 rounded-lg font-semibold transition-colors duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-brand hover:bg-brand-hover text-black px-4 py-2 text-xs" disabled={flag(busy.to_computed())}>"Change Password"</button>
                {error_view}
                {done_view}
            </div>
            <p class="text-xs text-warning">"Changing your password signs out every app that uses these tokens. You will need to create new ones."</p>
        </form>
    }
}
