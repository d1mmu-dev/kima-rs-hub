//! Tokens belong to the authenticated profile; no profile selector or account-wide list.
use super::{BUTTON, FIELD, message_view};
use crate::api::app_tokens as api;
use crate::ui::{Ctx, flag, resource_view};
use kima_api_types::app_tokens::{AppTokensResponse, CreatedAppToken};
use std::rc::Rc;
use vertigo::{DomNode, Resource, Value, bind, dom, get_driver};

type Data = Value<Resource<Rc<AppTokensResponse>>>;

fn refresh(ctx: &Ctx, data: Data, busy: Value<bool>) {
    get_driver().spawn(bind!(ctx, async move {
        data.set(match api::list(&ctx.client).await {
            Ok(answer) => Resource::Ready(Rc::new(answer)),
            Err(err) => Resource::Error(err.message),
        });
        busy.set(false);
    }));
}

pub(super) fn render(ctx: &Ctx) -> DomNode {
    let data: Data = Value::new(Resource::Loading);
    // Serialize mutations with the initial list too: a late initial GET must not release
    // another request's busy guard or permit overlapping one-time-secret creations.
    let busy = Value::new(true);
    let name = Value::new(String::new());
    let error = Value::new(None);
    // Page-local only. Never put the returned secret into storage, logs, or list data.
    let secret: Value<Option<CreatedAppToken>> = Value::new(None);
    if get_driver().is_browser() {
        refresh(ctx, data.clone(), busy.clone());
    }
    let rows = resource_view(
        data.to_computed(),
        bind!(ctx, data, busy, error, |answer: Rc<AppTokensResponse>| {
            let rows: Vec<DomNode> = answer.api_keys.iter().map(|token| {
            let name = token.name.clone();
            let created = format!("Created {} · Last used {}", token.created_at, token.last_used.as_deref().unwrap_or("never"));
            let id = token.id;
            let revoke = bind!(ctx, data, busy, error, |_| {
                if vertigo::transaction(|c| busy.get(c)) { return; }
                busy.set(true); error.set(None);
                get_driver().spawn(bind!(ctx, data, busy, error, async move {
                    match api::revoke(&ctx.client, id).await {
                        Ok(_) => { ctx.notify("App token revoked"); refresh(&ctx, data, busy); }
                        Err(err) => { error.set(Some(err.message)); busy.set(false); }
                    }
                }));
            });
            dom! { <div class="border border-white/10 rounded-lg p-4 space-y-2">
                <p class="text-white font-semibold">{name}</p><p class="text-xs text-[var(--text-muted)]">{created}</p>
                <button type="button" class={BUTTON} on_click={revoke} disabled={flag(busy.to_computed())}>"Revoke"</button>
            </div> }
        }).collect();
            if rows.is_empty() {
                dom! { <p class="text-sm text-[var(--text-muted)]">"No app tokens for this profile."</p> }
            } else {
                dom! { <div class="space-y-3">{..rows}</div> }
            }
        }),
    );
    let submit = bind!(ctx, name, secret, busy, data, error, || {
        if vertigo::transaction(|c| busy.get(c) || secret.get(c).is_some()) {
            return;
        }
        let body = match api::create_body(&vertigo::transaction(|c| name.get(c))) {
            Ok(body) => body,
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        busy.set(true);
        error.set(None);
        get_driver().spawn(bind!(ctx, name, secret, busy, data, error, async move {
            match api::create(&ctx.client, &body).await {
                Ok(answer) => {
                    secret.set(Some(answer));
                    name.set(String::new());
                    refresh(&ctx, data, busy);
                }
                Err(err) => {
                    error.set(Some(err.message));
                    busy.set(false);
                }
            }
        }));
    });
    let reveal = secret.render_value(bind!(ctx, secret, |value: Option<CreatedAppToken>| match value {
        Some(value) => {
            let dismiss = bind!(secret, |_| secret.set(None));
            let copy = bind!(ctx, |_| {
                // Selection + execCommand also works on HTTP local development origins.
                // Do not claim success when the browser denies clipboard access.
                vertigo::js! { document.getElementById("app-token-secret").select() };
                if matches!(vertigo::js! { document.execCommand("copy") }, vertigo::JsJson::True) {
                    ctx.notify("App token copied");
                } else { ctx.notify("Copy was blocked. Select and copy the token manually."); }
            });
            dom! { <div class="border border-brand/40 rounded-lg p-4 space-y-3">
                <p role="status" class="text-sm text-warning">"Save this app token now. It is shown only once; dismissing or leaving Settings hides it permanently. Use your account username and this token in your Subsonic app. It signs into this profile."</p>
                <input id="app-token-secret" aria-label="New app token (shown once)" class={FIELD} readonly="readonly" autocomplete="off" spellcheck="false" value={value.api_key} />
                <div class="flex flex-wrap gap-2"><button type="button" class={BUTTON} on_click={copy}>"Copy token"</button><button type="button" class={BUTTON} on_click={dismiss}>"I saved it — hide token"</button></div>
            </div> }
        },
        None => dom! { <span /> },
    }));
    let create_disabled = vertigo::Computed::from(bind!(busy, secret, |c| {
        busy.get(c) || secret.get(c).is_some()
    }));
    let reload = bind!(ctx, data, busy, |_| {
        if vertigo::transaction(|c| busy.get(c)) {
            return;
        }
        busy.set(true);
        refresh(&ctx, data.clone(), busy.clone());
    });
    dom! { <section id="apps" class="space-y-4">
        <h2 class="text-lg font-bold text-white">"Apps"</h2>
        <p class="text-sm text-[var(--text-muted)]">"Only this profile's Subsonic app tokens are listed. Each profile manages its own tokens, including main. The account shares a limit of 50 tokens across all profiles; the server enforces it."</p>
        {reveal}{rows}
        <button type="button" class={BUTTON} on_click={reload} disabled={flag(busy.to_computed())}>"Refresh app tokens"</button>
        <form class="space-y-3" on_submit={submit}>
            <input aria-label="App device name" placeholder="Device name (e.g. Phone)" maxlength="100" class={FIELD} value={name.clone()} on_input={bind!(name, |v: String| name.set(v))} disabled={flag(create_disabled.clone())} />
            <button type="submit" class={BUTTON} disabled={flag(create_disabled)}>"Create app token"</button>
        </form>{message_view(error)}
    </section> }
}
