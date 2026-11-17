use super::{BUTTON, FIELD, message_view};
use crate::api::profiles as api;
use crate::ui::{Ctx, flag, resource_view};
use kima_api_types::profiles::{
    AVATAR_PRESETS, CreateProfile, Profile, ProfileKind, ProfilesResponse, is_avatar_preset,
};
use std::rc::Rc;
use vertigo::{DomNode, Resource, Value, bind, dom, get_driver};

type Data = Value<Resource<Rc<ProfilesResponse>>>;

fn refresh(ctx: &Ctx, data: Data, error: Value<Option<String>>, busy: Value<bool>) {
    get_driver().spawn(bind!(ctx, async move {
        match api::list(&ctx.client).await {
            Ok(answer) => data.set(Resource::Ready(Rc::new(answer))),
            Err(err) => {
                data.set(Resource::Error(err.message.clone()));
                error.set(Some(err.message));
            }
        }
        busy.set(false);
    }));
}

pub(super) fn render(ctx: &Ctx) -> DomNode {
    let data: Data = Value::new(Resource::Loading);
    let error = Value::new(None);
    let busy = Value::new(false);
    if get_driver().is_browser() {
        refresh(ctx, data.clone(), error.clone(), busy.clone());
    }
    let content = resource_view(
        data.to_computed(),
        bind!(ctx, data, error, busy, |answer: Rc<ProfilesResponse>| {
            let manager = answer
                .profiles
                .iter()
                .find(|p| p.id == answer.current)
                .is_some_and(|p| can_manage(p.kind) && p.is_main);
            let rows: Vec<DomNode> = answer.profiles.iter().map(|p| {
            let picture = crate::pages::profiles::avatar(p);
            let name = p.name.clone();
            let kind = format!("{:?}", p.kind);
            let edit = if manager { editor(&ctx, Some(p.clone()), data.clone(), error.clone(), busy.clone()) } else { dom! { <span /> } };
            dom! { <div class="border border-white/10 rounded-lg p-4 space-y-3">
                <div class="flex items-center gap-3">{picture}<span class="font-semibold text-white">{name}</span><span class="text-xs text-brand">{kind}</span></div>
                {edit}
            </div> }
        }).collect();
            let add = if can_add(manager, answer.profiles.len(), answer.max) {
                editor(&ctx, None, data.clone(), error.clone(), busy.clone())
            } else if manager {
                dom! { <p class="text-sm text-warning">"Profile limit reached. Delete a profile before adding another."</p> }
            } else {
                dom! { <p class="text-sm text-[var(--text-muted)]">"Only the main profile can manage profiles."</p> }
            };
            let count = format!("{} / {} profiles", answer.profiles.len(), answer.max);
            dom! { <div class="space-y-4"><p class="text-sm text-[var(--text-muted)]">{count}</p>{..rows}{add}</div> }
        }),
    );
    dom! { <section id="profiles" class="space-y-4">
        <h2 class="text-lg font-bold text-white">"Profiles"</h2>
        <p class="text-sm text-[var(--text-muted)]">"Up to 5 profiles, including main. Multiple kids profiles are allowed. Kids has no content filters yet. No PIN is required: profiles are not a security boundary."</p>
        {content}{message_view(error)}
    </section> }
}

fn editor(
    ctx: &Ctx,
    profile: Option<Profile>,
    data: Data,
    error: Value<Option<String>>,
    busy: Value<bool>,
) -> DomNode {
    let name = Value::new(profile.as_ref().map(|p| p.name.clone()).unwrap_or_default());
    let kind = Value::new(
        profile
            .as_ref()
            .map(|p| p.kind)
            .unwrap_or(ProfileKind::Standard),
    );
    let avatar = Value::new(
        profile
            .as_ref()
            .and_then(|p| p.avatar.clone())
            .unwrap_or_default(),
    );
    let is_main = profile
        .as_ref()
        .is_some_and(|p| p.kind == ProfileKind::Main);
    let title = if profile.is_some() {
        "Edit profile"
    } else {
        "Add profile"
    };
    let submit = bind!(ctx, profile, name, kind, avatar, data, error, busy, || {
        if vertigo::transaction(|c| busy.get(c)) {
            return;
        }
        let (n, k, a) = vertigo::transaction(|c| (name.get(c), kind.get(c), avatar.get(c)));
        let update = match update_form(&n, k, &a) {
            Ok(b) => b,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        // Creating a main profile is never possible; main's kind is omitted on edit.
        let create = if profile.is_none() {
            match profile_form(&n, k, &a) {
                Ok(b) => Some(b),
                Err(e) => {
                    error.set(Some(e));
                    return;
                }
            }
        } else {
            None
        };
        busy.set(true);
        error.set(None);
        get_driver().spawn(bind!(ctx, profile, data, error, busy, async move {
            let result = match (profile, create) {
                (Some(p), _) => api::update(&ctx.client, p.id, &update).await,
                (_, Some(body)) => api::create(&ctx.client, &body).await,
                _ => return,
            };
            match result {
                Ok(_) => {
                    api::invalidate(&ctx.profiles_rev);
                    ctx.notify("Profile saved");
                    refresh(&ctx, data, error, busy);
                }
                Err(err) => {
                    error.set(Some(err.message));
                    busy.set(false);
                }
            }
        }));
    });
    let kinds = if is_main {
        dom! { <p class="text-xs text-[var(--text-muted)]">"Main profile kind cannot be changed."</p> }
    } else {
        let choices: Vec<DomNode> = [(ProfileKind::Standard, "Standard"), (ProfileKind::Kids, "Kids")].into_iter().map(|(k, label)| {
            let checked = kind.map(move |v| (v == k).to_string());
            let click = bind!(kind, |_| kind.set(k));
            dom! { <button type="button" class={BUTTON} aria-pressed={checked} on_click={click} disabled={flag(busy.to_computed())}>{label}</button> }
        }).collect();
        dom! { <div role="group" aria-label="Profile kind" class="flex gap-2">{..choices}</div> }
    };
    let avatars: Vec<DomNode> = std::iter::once("").chain(AVATAR_PRESETS).map(|preset| {
        let label = if preset.is_empty() { "Initial".to_owned() } else { format!("{} {preset}", crate::pages::profiles::avatar_text(Some(preset), "")) };
        let checked = avatar.map(move |v| (v == preset).to_string());
        let click = bind!(avatar, |_| avatar.set(preset.into()));
        dom! { <button type="button" class={BUTTON} aria-pressed={checked} on_click={click} disabled={flag(busy.to_computed())}>{label}</button> }
    }).collect();
    let delete = match profile {
        Some(p) if !is_main => deletion(ctx, p, data, error, busy.clone()),
        _ => dom! { <span /> },
    };
    dom! { <details class="space-y-3">
        <summary class="text-brand cursor-pointer">{title}</summary>
        <form on_submit={submit} class="space-y-3 py-3">
            <input class={FIELD} aria-label="Profile name" placeholder="Profile name (1–50 characters)" maxlength="50" value={name.clone()} on_input={bind!(name, |v: String| name.set(v))} disabled={flag(busy.to_computed())} />
            {kinds}<div role="group" aria-label="Avatar preset" class="flex flex-wrap gap-2">{..avatars}</div>
            <button type="submit" class={BUTTON} disabled={flag(busy.to_computed())}>"Save profile"</button>
        </form>{delete}
    </details> }
}

fn deletion(
    ctx: &Ctx,
    profile: Profile,
    data: Data,
    error: Value<Option<String>>,
    busy: Value<bool>,
) -> DomNode {
    let typed = Value::new(String::new());
    // Track typed text explicitly as well as busy.
    let allowed = {
        let typed = typed.clone();
        let busy = busy.clone();
        let p = profile.clone();
        vertigo::Computed::from(move |c| !busy.get(c) && can_delete(p.kind, &p.name, &typed.get(c)))
    };
    let click = bind!(ctx, profile, data, error, busy, typed, |_| {
        if vertigo::transaction(|c| {
            busy.get(c) || !can_delete(profile.kind, &profile.name, &typed.get(c))
        }) {
            return;
        }
        busy.set(true);
        error.set(None);
        get_driver().spawn(bind!(ctx, profile, data, error, busy, async move {
            match api::delete(&ctx.client, profile.id).await {
                Ok(_) => {
                    api::invalidate(&ctx.profiles_rev);
                    ctx.notify("Profile and personal data deleted");
                    refresh(&ctx, data, error, busy);
                }
                Err(err) => {
                    error.set(Some(err.message));
                    busy.set(false);
                }
            }
        }));
    });
    let prompt = format!("Type {} exactly to confirm", profile.name);
    dom! { <details class="border-t border-white/10 py-3 space-y-3">
        <summary class="text-danger-light cursor-pointer">"Delete profile permanently"</summary>
        <p class="text-sm text-warning">"This cannot be undone. Deleting this profile permanently deletes its playlists, listening history, stars, ratings, settings, playback state, bookmarks, podcast follows/progress/download records and app tokens. Its sessions and app tokens stop working. Other profiles and the shared library are not deleted."</p>
        <label class="text-sm text-white">{prompt}<input class={FIELD} aria-label="Confirm profile name" autocomplete="off" value={typed.clone()} on_input={bind!(typed, |v: String| typed.set(v))} /></label>
        <button type="button" class={BUTTON} disabled={flag(allowed.map(|v| !v))} on_click={click}>"Delete permanently"</button>
    </details> }
}

fn profile_form(name: &str, kind: ProfileKind, avatar: &str) -> Result<CreateProfile, String> {
    let name = name.trim();
    if !(1..=50).contains(&name.chars().count()) {
        return Err("Profile name must be 1 to 50 characters".into());
    }
    if !matches!(kind, ProfileKind::Kids | ProfileKind::Standard) {
        return Err("Invalid profile kind".into());
    }
    if !avatar.is_empty() && !is_avatar_preset(avatar) {
        return Err("Unknown avatar".into());
    }
    Ok(CreateProfile {
        name: name.into(),
        kind,
        avatar: (!avatar.is_empty()).then(|| avatar.into()),
    })
}

fn can_add(manager: bool, count: usize, max: u32) -> bool {
    manager && count < max as usize
}

pub(super) fn can_manage(kind: ProfileKind) -> bool {
    kind == ProfileKind::Main
}

fn can_delete(kind: ProfileKind, name: &str, typed: &str) -> bool {
    matches!(kind, ProfileKind::Kids | ProfileKind::Standard) && !name.is_empty() && name == typed
}

fn update_form(
    name: &str,
    kind: ProfileKind,
    avatar: &str,
) -> Result<kima_api_types::profiles::UpdateProfile, String> {
    let body = profile_form(
        name,
        if kind == ProfileKind::Main {
            ProfileKind::Standard
        } else {
            kind
        },
        avatar,
    )?;
    Ok(kima_api_types::profiles::UpdateProfile {
        name: Some(body.name),
        kind: (kind != ProfileKind::Main).then_some(kind),
        avatar: Some(body.avatar),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_api_types::profiles::ProfileKind;

    #[test]
    fn add_stops_at_advertised_limit_without_restricting_kids_count() {
        assert!(can_add(true, 4, 5));
        assert!(!can_add(true, 5, 5));
        assert!(!can_add(false, 1, 5));
        assert!(profile_form("Child one", ProfileKind::Kids, "cat").is_ok());
        assert!(profile_form("Child two", ProfileKind::Kids, "dog").is_ok());
    }

    #[test]
    fn management_and_password_require_main_not_admin_role() {
        assert!(can_manage(ProfileKind::Main));
        for kind in [
            ProfileKind::Kids,
            ProfileKind::Standard,
            ProfileKind::Unknown,
        ] {
            assert!(!can_manage(kind));
        }
    }

    #[test]
    fn deletion_requires_exact_name_and_never_main() {
        assert!(can_delete(ProfileKind::Kids, "Kids", "Kids"));
        assert!(!can_delete(ProfileKind::Kids, "Kids", "kids"));
        assert!(!can_delete(ProfileKind::Kids, "Kids", " Kids "));
        assert!(!can_delete(ProfileKind::Main, "Main", "Main"));
        assert!(!can_delete(ProfileKind::Unknown, "Future", "Future"));
    }

    #[test]
    fn editing_main_keeps_kind_and_can_clear_avatar() {
        let update = update_form(" Renamed ", ProfileKind::Main, "").unwrap();
        assert_eq!(update.name.as_deref(), Some("Renamed"));
        assert_eq!(update.kind, None);
        assert_eq!(update.avatar, Some(None));
        let update = update_form("Child", ProfileKind::Kids, "dog").unwrap();
        assert_eq!(update.kind, Some(ProfileKind::Kids));
        assert_eq!(update.avatar, Some(Some("dog".into())));
    }

    #[test]
    fn profile_form_trims_counts_unicode_and_only_accepts_presets() {
        let body = profile_form("  Żółw 🐢  ", ProfileKind::Kids, "cat").unwrap();
        assert_eq!(body.name, "Żółw 🐢");
        assert_eq!(body.avatar.as_deref(), Some("cat"));
        assert!(profile_form(" ", ProfileKind::Kids, "").is_err());
        assert!(profile_form(&"ż".repeat(50), ProfileKind::Standard, "").is_ok());
        assert!(profile_form(&"ż".repeat(51), ProfileKind::Kids, "").is_err());
        assert!(profile_form("Kids", ProfileKind::Main, "").is_err());
        assert!(profile_form("Kids", ProfileKind::Unknown, "").is_err());
        assert!(profile_form("Kids", ProfileKind::Kids, "upload").is_err());
    }
}
