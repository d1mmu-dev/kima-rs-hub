//! kima-web: Kima's Rust/WASM frontend (vertigo). See README.md.

mod api;
mod app;
mod auth;
mod components;
mod icons;
#[rustfmt::skip]
mod icons_data;
mod pages;
mod player;
mod position;
mod route;
mod shell;
mod style;
mod tracklist;
mod ui;

use vertigo::{DomNode, main};

#[main]
fn app() -> DomNode {
    app::render()
}
