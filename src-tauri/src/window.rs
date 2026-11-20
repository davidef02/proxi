//! Pure window/chrome/bounds/zoom module for the Tauri shell.
//!
//! Ports the Electron window bounds, chrome persistence and zoom contract of
//! `app/desktop/main.mjs:62-93` into testable Rust functions with no Tauri runtime imports.

use std::borrow::Cow;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Default window width in logical DIP units.
pub const DEFAULT_WIDTH: f64 = 1440.0;
/// Default window height in logical DIP units.
pub const DEFAULT_HEIGHT: f64 = 920.0;
/// Minimum zoom factor supported by the shell (AC4).
pub const MIN_ZOOM: f64 = 0.5;
/// Maximum zoom factor supported by the shell (AC4).
pub const MAX_ZOOM: f64 = 3.0;
/// Title bar overlay height: the app's top bar is h-9 = 36px (app/web/src/shell/Shell.tsx).
pub const OVERLAY_HEIGHT: u32 = 36;

/// A pair of background and symbol colors for native titlebar overlay and window background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromePair {
    pub color: Cow<'static, str>,
    pub symbol_color: Cow<'static, str>,
}

impl ChromePair {
    pub fn new(
        color: impl Into<Cow<'static, str>>,
        symbol_color: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            color: color.into(),
            symbol_color: symbol_color.into(),
        }
    }
}

/// Chrome configuration for the desktop shell.
///
/// Mirrors `app/shared/chrome.mjs`: `modePref` controls whether the OS dark mode
/// is followed ("system") or forced ("dark" | "light").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chrome {
    pub mode_pref: Cow<'static, str>,
    pub dark: ChromePair,
    pub light: ChromePair,
}

impl Chrome {
    pub fn new(
        mode_pref: impl Into<Cow<'static, str>>,
        dark: ChromePair,
        light: ChromePair,
    ) -> Self {
        Self {
            mode_pref: mode_pref.into(),
            dark,
            light,
        }
    }
}

impl Default for Chrome {
    fn default() -> Self {
        DEFAULT_CHROME
    }
}

/// Default chrome mirroring `app/shared/chrome.mjs`.
///
/// Neutro chrome = --background / --muted-foreground of the Neutro blocks in `app/web/src/styles.css`.
pub const DEFAULT_CHROME: Chrome = Chrome {
    mode_pref: Cow::Borrowed("system"),
    dark: ChromePair {
        color: Cow::Borrowed("#131416"),
        symbol_color: Cow::Borrowed("#96938d"),
    },
    light: ChromePair {
        color: Cow::Borrowed("#f6f4ef"),
        symbol_color: Cow::Borrowed("#6c675e"),
    },
};

fn is_valid_hex(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return false;
    }
    bytes[1..].iter().all(|b| b.is_ascii_hexdigit())
}

fn parse_pair(v: &serde_json::Value) -> Option<ChromePair> {
    let obj = v.as_object()?;
    let color_raw = obj.get("color")?.as_str()?;
    let symbol_color_raw = obj.get("symbolColor")?.as_str()?;
    if !is_valid_hex(color_raw) || !is_valid_hex(symbol_color_raw) {
        return None;
    }
    Some(ChromePair {
        color: Cow::Owned(color_raw.to_ascii_lowercase()),
        symbol_color: Cow::Owned(symbol_color_raw.to_ascii_lowercase()),
    })
}

/// Validates a chrome payload (from the renderer or `window.json`); None when anything is off.
///
/// Mirrors `parseChrome` in `app/shared/chrome.mjs` byte-for-byte in error boundaries:
/// modePref in system|dark|light, hex colors formatted as `^#[0-9a-f]{6}$` case-insensitive,
/// lowercased output, and unknown keys dropped.
pub fn parse_chrome(v: &serde_json::Value) -> Option<Chrome> {
    let obj = v.as_object()?;
    let mode_pref = obj.get("modePref")?.as_str()?;
    if !matches!(mode_pref, "system" | "dark" | "light") {
        return None;
    }
    let dark = parse_pair(obj.get("dark")?)?;
    let light = parse_pair(obj.get("light")?)?;
    Some(Chrome {
        mode_pref: Cow::Owned(mode_pref.to_string()),
        dark,
        light,
    })
}

/// The colours for the current mode: the OS decides when the preference is "system".
///
/// Mirrors `resolveChrome` in `app/shared/chrome.mjs`.
pub fn resolve_chrome<'a>(c: &'a Chrome, os_dark: bool) -> &'a ChromePair {
    match c.mode_pref.as_ref() {
        "system" => {
            if os_dark {
                &c.dark
            } else {
                &c.light
            }
        }
        "dark" => &c.dark,
        "light" => &c.light,
        _ => &c.dark,
    }
}

/// Strict intersection test mirroring `main.mjs:68-71`:
/// `b.x < a.x + a.width && b.x + b.width > a.x && b.y < a.y + a.height && b.y + b.height > a.y`.
/// Edge-touch and zero-size bounds count as not visible.
pub fn bounds_visible(x: f64, y: f64, w: f64, h: f64, displays: &[(f64, f64, f64, f64)]) -> bool {
    if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
        return false;
    }
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    displays.iter().any(|&(dx, dy, dw, dh)| {
        if !dx.is_finite() || !dy.is_finite() || !dw.is_finite() || !dh.is_finite() {
            return false;
        }
        if dw <= 0.0 || dh <= 0.0 {
            return false;
        }
        x < dx + dw && x + w > dx && y < dy + dh && y + h > dy
    })
}

/// Clamps finite zoom values to [MIN_ZOOM, MAX_ZOOM] (0.5 to 3.0), non-finite values to 1.0.
pub fn clamp_zoom(z: f64) -> f64 {
    if !z.is_finite() {
        1.0
    } else if z < MIN_ZOOM {
        MIN_ZOOM
    } else if z > MAX_ZOOM {
        MAX_ZOOM
    } else {
        z
    }
}

/// Extracts a saved zoom factor from JSON if it is a number in [0.5, 3.0], else returns 1.0.
///
/// Mirrors `main.mjs:93`.
pub fn saved_zoom(v: &serde_json::Value) -> f64 {
    if let Some(n) = v.as_f64() {
        if n.is_finite() && n >= MIN_ZOOM && n <= MAX_ZOOM {
            return n;
        }
    }
    1.0
}

/// Window state loaded from `window.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedWindowState {
    pub position: Option<(f64, f64)>,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
    pub chrome: Chrome,
    pub zoom: f64,
}

impl LoadedWindowState {
    pub fn x(&self) -> Option<f64> {
        self.position.map(|(x, _)| x)
    }

    pub fn y(&self) -> Option<f64> {
        self.position.map(|(_, y)| y)
    }
}

fn default_window_state() -> LoadedWindowState {
    LoadedWindowState {
        position: None,
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
        maximized: false,
        chrome: DEFAULT_CHROME,
        zoom: 1.0,
    }
}

/// Loads window state from the given path, degrading off-screen positions to `None`.
///
/// Missing or unparseable files default to 1440x920 + DEFAULT_CHROME + zoom 1.0 + no position + maximized false.
/// Legacy `theme`-only chrome falls back to `DEFAULT_CHROME`.
/// Off-screen or non-finite positions degrade to `position: None`, keeping width/height/zoom/chrome (main.mjs:72).
pub fn load_window_state(
    path: impl AsRef<Path>,
    displays: &[(f64, f64, f64, f64)],
) -> LoadedWindowState {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return default_window_state(),
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return default_window_state(),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return default_window_state(),
    };

    let chrome = obj
        .get("chrome")
        .and_then(parse_chrome)
        .unwrap_or(DEFAULT_CHROME);

    let zoom = obj
        .get("zoom")
        .map(saved_zoom)
        .unwrap_or(1.0);

    let width = obj
        .get("width")
        .and_then(|w| w.as_f64())
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(DEFAULT_WIDTH);

    let height = obj
        .get("height")
        .and_then(|h| h.as_f64())
        .filter(|h| h.is_finite() && *h > 0.0)
        .unwrap_or(DEFAULT_HEIGHT);

    let x = obj.get("x").and_then(|x| x.as_f64()).filter(|x| x.is_finite());
    let y = obj.get("y").and_then(|y| y.as_f64()).filter(|y| y.is_finite());

    match (x, y) {
        (Some(x), Some(y)) if bounds_visible(x, y, width, height, displays) => {
            let maximized = obj
                .get("maximized")
                .and_then(|m| m.as_bool())
                .unwrap_or(false);
            LoadedWindowState {
                position: Some((x, y)),
                width,
                height,
                maximized,
                chrome,
                zoom,
            }
        }
        _ => LoadedWindowState {
            position: None,
            width,
            height,
            maximized: false,
            chrome,
            zoom,
        },
    }
}

/// Fallback helper to load window state without display bounds.
pub fn load_window_state_without_displays(path: impl AsRef<Path>) -> LoadedWindowState {
    load_window_state(path, &[])
}

/// State serialized to `window.json`.
///
/// Field order is pinned to Electron's exact key order:
/// `x`, `y`, `width`, `height`, `maximized`, `chrome`, `zoom`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveState {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
    pub chrome: Chrome,
    pub zoom: f64,
}

/// Persists window state to `path` with `serde_json::to_string`, creating parent directories.
pub fn save_window_state(path: impl AsRef<Path>, state: &SaveState) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_chrome_malformed_input() {
        let pair_val = json!({ "color": "#000000", "symbolColor": "#ffffff" });
        let ok = json!({
            "modePref": "system",
            "dark": pair_val,
            "light": pair_val
        });
        let parsed_ok = parse_chrome(&ok).expect("valid ok payload");
        assert_eq!(parsed_ok.mode_pref, "system");

        // Unknown keys are dropped
        let with_extra = json!({
            "modePref": "system",
            "dark": pair_val,
            "light": pair_val,
            "theme": "light",
            "extra": 1
        });
        let parsed = parse_chrome(&with_extra).expect("valid with extra keys");
        assert_eq!(parsed.mode_pref, "system");
        assert_eq!(parsed.dark.color, "#000000");
        assert_eq!(parsed.dark.symbol_color, "#ffffff");
        assert_eq!(parsed.light.color, "#000000");
        assert_eq!(parsed.light.symbol_color, "#ffffff");

        // Non-object
        assert_eq!(parse_chrome(&json!(null)), None);
        assert_eq!(parse_chrome(&json!(42)), None);
        assert_eq!(parse_chrome(&json!("system")), None);
        assert_eq!(parse_chrome(&json!([1, 2, 3])), None);
        assert_eq!(parse_chrome(&json!(true)), None);

        // Empty object
        assert_eq!(parse_chrome(&json!({})), None);

        // Legacy window.json theme key alone
        assert_eq!(parse_chrome(&json!({ "theme": "light" })), None);

        // Missing or bad modePref
        assert_eq!(parse_chrome(&json!({ "dark": pair_val, "light": pair_val })), None);
        assert_eq!(
            parse_chrome(&json!({ "modePref": "other", "dark": pair_val, "light": pair_val })),
            None
        );
        assert_eq!(
            parse_chrome(&json!({ "modePref": 123, "dark": pair_val, "light": pair_val })),
            None
        );

        // dark null or string
        assert_eq!(
            parse_chrome(&json!({ "modePref": "system", "dark": null, "light": pair_val })),
            None
        );
        assert_eq!(
            parse_chrome(&json!({ "modePref": "system", "dark": "#000000", "light": pair_val })),
            None
        );

        // Missing symbolColor
        assert_eq!(
            parse_chrome(&json!({
                "modePref": "system",
                "dark": { "color": "#000000" },
                "light": pair_val
            })),
            None
        );

        // Non-string colour
        assert_eq!(
            parse_chrome(&json!({
                "modePref": "system",
                "dark": pair_val,
                "light": { "color": 0, "symbolColor": "#ffffff" }
            })),
            None
        );

        // 7-digit hex
        assert_eq!(
            parse_chrome(&json!({
                "modePref": "system",
                "dark": pair_val,
                "light": { "color": "#0000000", "symbolColor": "#ffffff" }
            })),
            None
        );

        // rgb() not accepted
        assert_eq!(
            parse_chrome(&json!({
                "modePref": "system",
                "dark": pair_val,
                "light": { "color": "rgb(0 0 0)", "symbolColor": "#ffffff" }
            })),
            None
        );

        // Untrimmed hex
        assert_eq!(
            parse_chrome(&json!({
                "modePref": "system",
                "dark": pair_val,
                "light": { "color": " #000000", "symbolColor": "#ffffff" }
            })),
            None
        );

        // Case-insensitive valid payload lowercases
        let upper = json!({
            "modePref": "DARK",
        });
        assert_eq!(parse_chrome(&upper), None); // modePref itself is case-sensitive system|dark|light

        let mixed = json!({
            "modePref": "dark",
            "dark": { "color": "#AABBCC", "symbolColor": "#DdEeFf" },
            "light": { "color": "#112233", "symbolColor": "#445566" }
        });
        let parsed_mixed = parse_chrome(&mixed).expect("mixed case hex valid");
        assert_eq!(parsed_mixed.dark.color, "#aabbcc");
        assert_eq!(parsed_mixed.dark.symbol_color, "#ddeeff");
        assert_eq!(parsed_mixed.light.color, "#112233");
        assert_eq!(parsed_mixed.light.symbol_color, "#445566");
    }

    #[test]
    fn test_parse_chrome_default_roundtrip() {
        let val = serde_json::to_value(&DEFAULT_CHROME).unwrap();
        let parsed = parse_chrome(&val).expect("DEFAULT_CHROME round-trips");
        assert_eq!(parsed, DEFAULT_CHROME);
        assert_eq!(DEFAULT_CHROME.mode_pref, "system");
        assert_eq!(DEFAULT_CHROME.dark.color, "#131416");
        assert_eq!(DEFAULT_CHROME.dark.symbol_color, "#96938d");
        assert_eq!(DEFAULT_CHROME.light.color, "#f6f4ef");
        assert_eq!(DEFAULT_CHROME.light.symbol_color, "#6c675e");
    }

    #[test]
    fn test_resolve_chrome() {
        let c = Chrome {
            mode_pref: Cow::Borrowed("system"),
            dark: ChromePair::new("#000000", "#111111"),
            light: ChromePair::new("#eeeeee", "#ffffff"),
        };
        assert_eq!(resolve_chrome(&c, true), &c.dark);
        assert_eq!(resolve_chrome(&c, false), &c.light);

        let dark_pref = Chrome {
            mode_pref: Cow::Borrowed("dark"),
            dark: ChromePair::new("#000000", "#111111"),
            light: ChromePair::new("#eeeeee", "#ffffff"),
        };
        assert_eq!(resolve_chrome(&dark_pref, false), &dark_pref.dark);
        assert_eq!(resolve_chrome(&dark_pref, true), &dark_pref.dark);

        let light_pref = Chrome {
            mode_pref: Cow::Borrowed("light"),
            dark: ChromePair::new("#000000", "#111111"),
            light: ChromePair::new("#eeeeee", "#ffffff"),
        };
        assert_eq!(resolve_chrome(&light_pref, true), &light_pref.light);
        assert_eq!(resolve_chrome(&light_pref, false), &light_pref.light);
    }

    #[test]
    fn test_bounds_visible_cases() {
        let single_display = [(0.0, 0.0, 1920.0, 1080.0)];

        // Overlap: standard window inside display
        assert!(bounds_visible(100.0, 100.0, 800.0, 600.0, &single_display));
        // Overlap: partially hanging off the right edge but intersecting
        assert!(bounds_visible(1800.0, 100.0, 800.0, 600.0, &single_display));

        // Edge-touch: exactly touching right edge -> not visible (strict < and >)
        assert!(!bounds_visible(1920.0, 100.0, 800.0, 600.0, &single_display));
        // Edge-touch: exactly touching bottom edge
        assert!(!bounds_visible(100.0, 1080.0, 800.0, 600.0, &single_display));
        // Edge-touch: exactly touching left edge from outside (x + w == dx)
        assert!(!bounds_visible(-800.0, 100.0, 800.0, 600.0, &single_display));
        // Edge-touch: exactly touching top edge from outside (y + h == dy)
        assert!(!bounds_visible(100.0, -600.0, 800.0, 600.0, &single_display));

        // Multi-display: display 1 (0..1920), display 2 (1920..3840)
        let dual_display = [(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 1920.0, 1080.0)];
        assert!(bounds_visible(2000.0, 100.0, 800.0, 600.0, &dual_display));
        assert!(!bounds_visible(4000.0, 100.0, 800.0, 600.0, &dual_display));

        // Zero-size window
        assert!(!bounds_visible(100.0, 100.0, 0.0, 600.0, &single_display));
        assert!(!bounds_visible(100.0, 100.0, 800.0, 0.0, &single_display));
        assert!(!bounds_visible(100.0, 100.0, 0.0, 0.0, &single_display));

        // Zero-size display
        let zero_display = [(0.0, 0.0, 0.0, 1080.0)];
        assert!(!bounds_visible(100.0, 100.0, 800.0, 600.0, &zero_display));

        // Empty displays
        assert!(!bounds_visible(100.0, 100.0, 800.0, 600.0, &[]));
    }

    #[test]
    fn test_clamp_zoom() {
        assert_eq!(clamp_zoom(0.1), 0.5);
        assert_eq!(clamp_zoom(0.5), 0.5);
        assert_eq!(clamp_zoom(2.5), 2.5);
        assert_eq!(clamp_zoom(3.0), 3.0);
        assert_eq!(clamp_zoom(9.9), 3.0);
        assert_eq!(clamp_zoom(f64::NAN), 1.0);
        assert_eq!(clamp_zoom(f64::INFINITY), 1.0);
        assert_eq!(clamp_zoom(f64::NEG_INFINITY), 1.0);
    }

    #[test]
    fn test_saved_zoom() {
        assert_eq!(saved_zoom(&json!(2.5)), 2.5);
        assert_eq!(saved_zoom(&json!(0.5)), 0.5);
        assert_eq!(saved_zoom(&json!(3.0)), 3.0);
        assert_eq!(saved_zoom(&json!(0.1)), 1.0);
        assert_eq!(saved_zoom(&json!(9.9)), 1.0);
        assert_eq!(saved_zoom(&json!("2.5")), 1.0);
        assert_eq!(saved_zoom(&json!(null)), 1.0);
        assert_eq!(saved_zoom(&json!({})), 1.0);
    }

    #[test]
    fn test_load_window_state() {
        let temp_dir = std::env::temp_dir().join(format!("proxi-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let displays = [(0.0, 0.0, 1920.0, 1080.0)];

        // 1. Missing file -> defaults
        let missing = temp_dir.join("missing.json");
        let s_missing = load_window_state(&missing, &displays);
        assert_eq!(s_missing.position, None);
        assert_eq!(s_missing.width, DEFAULT_WIDTH);
        assert_eq!(s_missing.height, DEFAULT_HEIGHT);
        assert_eq!(s_missing.maximized, false);
        assert_eq!(s_missing.chrome, DEFAULT_CHROME);
        assert_eq!(s_missing.zoom, 1.0);

        // 2. Invalid JSON -> defaults
        let invalid = temp_dir.join("invalid.json");
        std::fs::write(&invalid, "not json {}}").unwrap();
        let s_invalid = load_window_state(&invalid, &displays);
        assert_eq!(s_invalid.position, None);
        assert_eq!(s_invalid.width, DEFAULT_WIDTH);
        assert_eq!(s_invalid.height, DEFAULT_HEIGHT);
        assert_eq!(s_invalid.maximized, false);
        assert_eq!(s_invalid.chrome, DEFAULT_CHROME);
        assert_eq!(s_invalid.zoom, 1.0);

        // 3. Legacy theme-only -> DEFAULT_CHROME and defaults
        let legacy = temp_dir.join("legacy.json");
        std::fs::write(&legacy, r#"{"theme":"dark"}"#).unwrap();
        let s_legacy = load_window_state(&legacy, &displays);
        assert_eq!(s_legacy.position, None);
        assert_eq!(s_legacy.width, DEFAULT_WIDTH);
        assert_eq!(s_legacy.height, DEFAULT_HEIGHT);
        assert_eq!(s_legacy.maximized, false);
        assert_eq!(s_legacy.chrome, DEFAULT_CHROME);
        assert_eq!(s_legacy.zoom, 1.0);

        // 4. Off-screen degrade -> position None, maximized false, keeping width/height/zoom/chrome
        let custom_chrome = json!({
            "modePref": "dark",
            "dark": { "color": "#112233", "symbolColor": "#445566" },
            "light": { "color": "#778899", "symbolColor": "#aabbcc" }
        });
        let offscreen = temp_dir.join("offscreen.json");
        std::fs::write(
            &offscreen,
            json!({
                "x": -32000.0,
                "y": -32000.0,
                "width": 800.0,
                "height": 600.0,
                "maximized": true,
                "chrome": custom_chrome,
                "zoom": 2.5
            })
            .to_string(),
        )
        .unwrap();
        let s_offscreen = load_window_state(&offscreen, &displays);
        assert_eq!(s_offscreen.position, None);
        assert_eq!(s_offscreen.width, 800.0);
        assert_eq!(s_offscreen.height, 600.0);
        assert_eq!(s_offscreen.maximized, false); // degraded
        assert_eq!(s_offscreen.zoom, 2.5);
        assert_eq!(s_offscreen.chrome.mode_pref, "dark");
        assert_eq!(s_offscreen.chrome.dark.color, "#112233");

        // 5. On-screen kept -> position Some((x, y)), maximized true
        let onscreen = temp_dir.join("onscreen.json");
        std::fs::write(
            &onscreen,
            json!({
                "x": 100.0,
                "y": 150.0,
                "width": 1200.0,
                "height": 800.0,
                "maximized": true,
                "chrome": custom_chrome,
                "zoom": 2.5
            })
            .to_string(),
        )
        .unwrap();
        let s_onscreen = load_window_state(&onscreen, &displays);
        assert_eq!(s_onscreen.position, Some((100.0, 150.0)));
        assert_eq!(s_onscreen.x(), Some(100.0));
        assert_eq!(s_onscreen.y(), Some(150.0));
        assert_eq!(s_onscreen.width, 1200.0);
        assert_eq!(s_onscreen.height, 800.0);
        assert_eq!(s_onscreen.maximized, true);
        assert_eq!(s_onscreen.zoom, 2.5);
        assert_eq!(s_onscreen.chrome.mode_pref, "dark");

        // 6. Zoom validation: 2.5 kept, 0.1 -> 1.0, non-number -> 1.0
        let zoom_low = temp_dir.join("zoom_low.json");
        std::fs::write(&zoom_low, json!({ "zoom": 0.1 }).to_string()).unwrap();
        assert_eq!(load_window_state(&zoom_low, &displays).zoom, 1.0);

        let zoom_str = temp_dir.join("zoom_str.json");
        std::fs::write(&zoom_str, json!({ "zoom": "2.5" }).to_string()).unwrap();
        assert_eq!(load_window_state(&zoom_str, &displays).zoom, 1.0);

        let zoom_high = temp_dir.join("zoom_high.json");
        std::fs::write(&zoom_high, json!({ "zoom": 5.0 }).to_string()).unwrap();
        assert_eq!(load_window_state(&zoom_high, &displays).zoom, 1.0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_save_window_state_exact_string_and_key_order() {
        let temp_dir = std::env::temp_dir().join(format!("proxi-test-save-{}", std::process::id()));
        let save_path = temp_dir.join("nested").join("window.json");

        let state = SaveState {
            x: 100.0,
            y: 200.0,
            width: 1440.0,
            height: 920.0,
            maximized: false,
            chrome: DEFAULT_CHROME,
            zoom: 1.0,
        };

        save_window_state(&save_path, &state).expect("save_window_state succeeds");
        let content = std::fs::read_to_string(&save_path).expect("read saved window.json");

        let expected = r##"{"x":100.0,"y":200.0,"width":1440.0,"height":920.0,"maximized":false,"chrome":{"modePref":"system","dark":{"color":"#131416","symbolColor":"#96938d"},"light":{"color":"#f6f4ef","symbolColor":"#6c675e"}},"zoom":1.0}"##;
        assert_eq!(content, expected);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
