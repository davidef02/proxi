//! Copy of `app/shared/identity.mjs`; renaming `DECK_ROOT` orphans every synced deck (AGENTS.md).

pub const APP_NAME: &str = "Proxi";
pub const DECK_ROOT: &str = "Proxi";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matches_identity_mjs() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/shared/identity.mjs");
        let raw = std::fs::read_to_string(path).expect("read app/shared/identity.mjs");

        assert!(raw.contains("export const APP_NAME = \"Proxi\";"));
        assert!(raw.contains("export const DECK_ROOT = \"Proxi\";"));
        assert_eq!(APP_NAME, "Proxi");
        assert_eq!(DECK_ROOT, "Proxi");
    }
}
