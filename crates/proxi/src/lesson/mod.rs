//! `proxi lesson`: port of extensions/html-lesson.ts (init, append, list).

pub mod cli;
pub mod doc;
pub mod js;
pub mod node_path;
pub mod render;

pub const APP_NAME: &str = "Proxi";
// Same values as APP_NAME / DECK_ROOT in app/shared/identity.mjs: renaming DECK_ROOT orphans every synced deck.
pub const DECK_ROOT: &str = "Proxi";
