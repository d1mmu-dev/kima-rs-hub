//! One module per page.

pub mod albums;
pub mod artists;
pub mod login;
pub mod onboarding;
pub mod playlists;
pub mod search;
pub mod settings;
pub mod tracks;

use vertigo::{DomNode, dom};

use crate::route::Route;

/// Unknown URL, inside the shell.
pub fn not_found() -> DomNode {
    dom! {
        <div class="page-shell py-12">
            <h1 class="text-2xl font-semibold mb-3 tracking-tight">"Page not found"</h1>
            <a class="text-brand hover:underline" href={Route::Artists.to_string()}>"Back to the library"</a>
        </div>
    }
}
