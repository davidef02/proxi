//! Homepage regeneration after a lesson CLI write (Node `refreshBoard`, extensions/html-lesson.ts:1200-1204).

use std::io::Write;

/// Regenerates the board after `compile-ref`/`reference` (D12: `rebuild` never calls it).
pub trait BoardRefresh {
    /// `Err(message)` makes the CLI print `Error: <message>` and exit 1, like a throwing `refreshBoard`.
    fn refresh(&mut self, err: &mut dyn Write) -> Result<(), String>;
}

/// Production seam until the Rust homepage generator (#7) lands: writes nothing, never fails.
pub struct NoBoardRefresh;

impl BoardRefresh for NoBoardRefresh {
    fn refresh(&mut self, _err: &mut dyn Write) -> Result<(), String> {
        Ok(())
    }
}

/// Test double: counts calls and optionally fails.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingRefresh {
    pub calls: usize,
    pub fail: Option<String>,
}

#[cfg(test)]
impl BoardRefresh for RecordingRefresh {
    fn refresh(&mut self, _err: &mut dyn Write) -> Result<(), String> {
        self.calls += 1;
        match &self.fail {
            Some(m) => Err(m.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_board_refresh_writes_nothing() {
        let mut out = Vec::new();
        let mut board = NoBoardRefresh;
        assert_eq!(board.refresh(&mut out), Ok(()));
        assert!(out.is_empty());
    }

    #[test]
    fn recording_refresh_counts_and_fails() {
        let mut err = Vec::new();
        let mut board = RecordingRefresh::default();
        assert_eq!(board.refresh(&mut err), Ok(()));
        assert_eq!(board.calls, 1);

        let mut failing = RecordingRefresh {
            calls: 0,
            fail: Some("boom".to_string()),
        };
        assert_eq!(failing.refresh(&mut err), Err("boom".to_string()));
        assert_eq!(failing.calls, 1);
        assert!(err.is_empty());
    }
}
