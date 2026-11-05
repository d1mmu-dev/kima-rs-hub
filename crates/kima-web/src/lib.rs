//! kima-web: Kima's Rust/WASM frontend. This is the build-pipeline scaffold; pages arrive in later tasks.

use vertigo::{DomNode, dom, main};

// Proves the shared DTO crate links into the wasm build.
use kima_api_types as _;

#[main]
fn app() -> DomNode {
    dom! {
        <html>
            <head>
                <title>"kima-web"</title>
            </head>
            <body>
                <h1>"kima-web"</h1>
            </body>
        </html>
    }
}
