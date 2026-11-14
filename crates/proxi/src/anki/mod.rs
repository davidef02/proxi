//! `proxi anki`: card derivation, TSV export, the AnkiConnect ping (port of extensions/anki-sync.ts)
//! and the board's deck stats (`board`, port of `fetchAnkiStatus` in extensions/homepage.ts).

pub mod board;
pub mod cards;
pub mod cli;
pub mod format;
pub mod ping;

pub use board::{
    fetch_anki_status, invoke, AnkiResponse, AnkiStatus, CourseCounts, DEFAULT_ANKI_URL, DEFAULT_TIMEOUT,
};
