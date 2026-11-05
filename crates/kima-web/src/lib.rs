//! kima-web: Kima's Rust/WASM frontend (vertigo). See README.md.

mod api;
mod app;
mod auth;
mod pages;
mod player;
mod route;
mod ui;

use vertigo::{DomNode, main};

#[main]
fn app() -> DomNode {
    app::render()
}
