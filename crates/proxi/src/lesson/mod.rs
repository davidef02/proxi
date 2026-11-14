//! Lesson artifacts and the `proxi lesson` CLI: init, append and list (port of
//! extensions/html-lesson.ts), plus the read side of the engine in [`artifact`].

pub mod artifact;
pub mod cli;
pub mod doc;
pub mod js;
pub mod node_path;
pub mod render;

pub use artifact::{
    Lesson, LessonDoc, LessonStatus, doc_rel, lesson_rel, list_lessons, read_doc_body, read_lesson,
    read_lesson_html, read_reference_document, scan_lesson_docs,
};

pub const APP_NAME: &str = "Proxi";
// Same values as APP_NAME / DECK_ROOT in app/shared/identity.mjs: renaming DECK_ROOT orphans every synced deck.
pub const DECK_ROOT: &str = "Proxi";
