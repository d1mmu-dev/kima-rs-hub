//! Account-local profile selection (D-248); profiles are not a security boundary without PINs.

use crate::api::{client::load, profiles};
use crate::route::Route;
use crate::ui::{Ctx, flag, resource_view};
use kima_api_types::profiles::{AVATAR_PRESETS, Profile, ProfileKind, ProfilesResponse};
use vertigo::{DomNode, Value, bind, dom, get_driver};

pub fn avatar_text(preset: Option<&str>, name: &str) -> String {
    let glyphs = [
        "🐱", "🐶", "🦊", "🦉", "🐻", "🐼", "🐰", "🐧", "🤖", "🚀", "⭐", "🎵",
    ];
    preset
        .and_then(|key| AVATAR_PRESETS.iter().position(|p| *p == key))
        .map(|i| glyphs[i].to_owned())
        .unwrap_or_else(|| name.chars().next().unwrap_or('?').to_uppercase().collect())
}

pub fn avatar(profile: &Profile) -> DomNode {
    let text = avatar_text(profile.avatar.as_deref(), &profile.name);
    let hue = profile
        .id
        .as_bytes()
        .iter()
        .fold(0u32, |sum, b| sum + u32::from(*b))
        % 360;
    let style = format!("background: hsl({hue} 45% 28%);");
    dom! { <span aria-hidden="true" class="inline-flex items-center justify-center rounded-lg w-12 h-12 text-2xl shrink-0" style={style}>{text}</span> }
}

/// Both entry points use the same switch transaction; the successful document replacement
/// discards page data, queue, podcast marks, search state, timers and request Resources.
pub fn choose(ctx: &Ctx, profile: Profile, busy: Value<bool>, error: Value<Option<String>>) {
    if !ctx.session.begin_switch() {
        return;
    }
    busy.set(true);
    error.set(None);
    get_driver().spawn(bind!(ctx, busy, error, async move {
        match profiles::switch(&ctx.client, &profile).await {
            Ok(done) => {
                ctx.client.retire();
                ctx.player.stop();
                ctx.session.remember_profile(&done.profile);
                let url = ctx.session.replace_pair(&done.token, &done.refresh_token);
                vertigo::js! { window.location.replace(url) };
            }
            Err(err) => {
                ctx.session.end_switch();
                error.set(Some(err.message));
                busy.set(false);
            }
        }
    }));
}

pub fn render(ctx: &Ctx) -> DomNode {
    let busy = Value::new(false);
    let error = Value::new(None);
    let data = load(bind!(ctx, async move { profiles::list(&ctx.client).await }));
    let tiles = resource_view(
        data,
        bind!(ctx, busy, error, |data: std::rc::Rc<ProfilesResponse>| {
            let rows: Vec<DomNode> = data.profiles.iter().map(|profile| {
            let name = profile.name.clone();
            let badge = if profile.kind == ProfileKind::Kids { "Kids" } else { "" };
            let picture = avatar(profile);
            let profile = profile.clone();
            let click = bind!(ctx, busy, error, |_| choose(&ctx, profile.clone(), busy.clone(), error.clone()));
            dom! {
                <button class="flex flex-col items-center gap-3 p-6 rounded-lg border border-white/10 hover:border-brand hover:bg-white/5 disabled:opacity-50" on_click={click} disabled={flag(busy.to_computed())}>
                    {picture}
                    <span class="text-lg font-semibold text-white">{name}</span>
                    <span class="text-xs text-brand">{badge}</span>
                </button>
            }
        }).collect();
            let main = data
                .profiles
                .iter()
                .any(|p| p.id == data.current && crate::auth::is_main(Some(p)));
            let manage = if main {
                dom! { <a href={Route::Settings.to_string()} class="text-brand hover:underline">"Manage profiles"</a> }
            } else {
                dom! { <span /> }
            };
            dom! {
                <div class="space-y-8">
                    <div class="grid grid-cols-2 md:grid-cols-3 gap-4">{..rows}</div>
                    {manage}
                </div>
            }
        }),
    );
    let error_view = error.render_value(|message| match message {
        Some(message) => dom! { <p role="alert" class="text-danger-light py-4">{message}</p> },
        None => dom! { <span /> },
    });
    let sign_out = bind!(ctx, |_| ctx.sign_out());
    dom! {
        <main class="min-h-screen bg-black text-white flex items-center justify-center p-6">
            <div class="w-full max-w-3xl space-y-8 text-center">
                <h1 class="text-4xl font-bold">"Who's listening?"</h1>
                {tiles}
                {error_view}
                <button class="text-sm text-[var(--text-muted)] hover:text-white" on_click={sign_out}>"Sign out"</button>
            </div>
        </main>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shared_avatar_preset_has_a_glyph_and_unknown_uses_initial() {
        for preset in kima_api_types::profiles::AVATAR_PRESETS {
            assert_ne!(avatar_text(Some(preset), "Alice"), "A", "{preset}");
        }
        assert_eq!(avatar_text(None, "Alice"), "A");
        assert_eq!(avatar_text(Some("future"), "Alice"), "A");
        assert_eq!(avatar_text(None, ""), "?");
    }
}
