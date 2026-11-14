//! vitest JSON report parsing (port of parseVitestReport, app/server/exercises.ts:139-170).

use serde_json::{Map, Value, json};

use crate::js;

const INVALID_REPORT: &str = "Report di vitest non valido";
const LOAD_FAILURE: &str = "Il file non si carica";

/// Drop every `ESC [ digits/; letter` run, like `String.replace(ANSI_RE, "")`.
/// An unterminated escape stays, exactly as the JS regex leaves it.
pub fn strip_ansi(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while matches!(chars.get(j), Some(c) if c.is_ascii_digit() || *c == ';') {
                j += 1;
            }
            if chars.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// JS `s.slice(0, max)` in UTF-16 units; a cut inside a surrogate pair drops that char
/// (a Rust string cannot hold a lone surrogate).
pub fn truncate_utf16(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in s.chars() {
        let width = c.len_utf16();
        if units + width > max {
            break;
        }
        out.push(c);
        units += width;
    }
    out
}

/// An ANSI-stripped failure message, cut where Node's `slice(0, 1000)` cuts.
pub fn clean_message(s: &str) -> String {
    truncate_utf16(&strip_ansi(s), 1000)
}

/// Pure: vitest JSON report text to the run result Node returns.
pub fn parse_vitest_report(json: &str, duration_ms: u64, at: &str) -> Result<Value, String> {
    let report = js::parse(json).map_err(|e| format!("{INVALID_REPORT}: {e}"))?;
    let report = report.as_object().ok_or(INVALID_REPORT)?;

    let mut failures = Vec::new();
    let mut tests = Vec::new();
    let test_results = match report.get("testResults") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => return Err(INVALID_REPORT.to_string()),
    };
    for file in test_results {
        // null throws in JS (`null.testResults`); other scalars just have no fields
        if file.is_null() {
            return Err(INVALID_REPORT.to_string());
        }
        let assertions = match file.get("assertionResults") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(items)) => items.as_slice(),
            Some(_) => return Err(INVALID_REPORT.to_string()),
        };
        if file.get("status").and_then(Value::as_str) == Some("failed") && assertions.is_empty() {
            // a suite that fails to load (syntax error, bad import) has no assertions
            let message = match file.get("message") {
                None | Some(Value::Null) => String::new(),
                Some(v) => js::js_string(Some(v)),
            };
            failures.push(json!({ "name": LOAD_FAILURE, "message": clean_message(&message) }));
            tests.push(json!({ "name": LOAD_FAILURE, "status": "failed" }));
        }
        for assertion in assertions {
            if assertion.is_null() {
                return Err(INVALID_REPORT.to_string());
            }
            let name = assertion_name(assertion);
            let status = match assertion.get("status").and_then(Value::as_str) {
                Some("passed") => "passed",
                Some("failed") => "failed",
                _ => "skipped",
            };
            tests.push(json!({ "name": name.clone(), "status": status }));
            if status != "failed" {
                continue;
            }
            let messages = match assertion.get("failureMessages") {
                None | Some(Value::Null) => &[][..],
                Some(Value::Array(items)) => items.as_slice(),
                Some(_) => return Err(INVALID_REPORT.to_string()),
            };
            let message = messages
                .iter()
                .map(|m| {
                    if m.is_null() {
                        String::new()
                    } else {
                        js::js_string(Some(m))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(json!({ "name": name, "message": clean_message(&message) }));
        }
    }

    let failed = match report.get("numFailedTests") {
        None | Some(Value::Null) => Value::from(failures.len()),
        Some(v) => v.clone(),
    };
    let total = match report.get("numTotalTests") {
        None | Some(Value::Null) => Value::from(0),
        Some(v) => v.clone(),
    };
    let passed = match report.get("numPassedTests") {
        None | Some(Value::Null) => Value::from(0),
        Some(v) => v.clone(),
    };
    let ok = failed.as_f64() == Some(0.0)
        && js::to_number(Some(&total)) > 0.0
        && !(js::to_number(report.get("numFailedTestSuites")) > 0.0);

    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(ok));
    out.insert("passed".into(), passed);
    out.insert("failed".into(), failed);
    out.insert("total".into(), total);
    out.insert("failures".into(), Value::Array(failures));
    out.insert("tests".into(), Value::Array(tests));
    out.insert("durationMs".into(), Value::from(duration_ms));
    out.insert("at".into(), Value::from(at));
    Ok(Value::Object(out))
}

/// `a.fullName ?? a.title ?? "?"`, keeping the value's shape.
fn assertion_name(assertion: &Value) -> Value {
    for key in ["fullName", "title"] {
        match assertion.get(key) {
            None | Some(Value::Null) => {}
            Some(v) => return v.clone(),
        }
    }
    Value::from("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-01-01T00:00:00.000Z";

    #[test]
    fn failing_report_is_not_ok_with_failures_ansi_stripped_and_truncated() {
        let report = serde_json::to_string(&json!({
            "numTotalTests": 3, "numPassedTests": 2, "numFailedTests": 1, "numFailedTestSuites": 1,
            "testResults": [{ "assertionResults": [
                { "status": "passed", "fullName": "a" },
                { "status": "failed", "fullName": "b ok", "failureMessages": [format!("\u{1b}[31mAssertionError\u{1b}[39m: {}", "x".repeat(2000))] },
                { "status": "passed", "fullName": "c" },
            ] }],
        }))
        .unwrap();

        let r = parse_vitest_report(&report, 5, AT).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r.get("error").is_none());
        assert_eq!(r["passed"], json!(2));
        assert_eq!(r["failed"], json!(1));
        let failure = &r["failures"][0];
        assert_eq!(failure["name"], json!("b ok"));
        let message = failure["message"].as_str().unwrap();
        assert!(message.starts_with("AssertionError"), "{message}");
        assert_eq!(message.encode_utf16().count(), 1000);
    }

    #[test]
    fn all_green_is_ok() {
        let report =
            r#"{"numTotalTests":1,"numPassedTests":1,"numFailedTests":0,"testResults":[]}"#;
        let r = parse_vitest_report(report, 1, AT).unwrap();
        assert_eq!(r["ok"], json!(true));
    }

    #[test]
    fn lists_every_test_in_order() {
        let report = r#"{"numTotalTests":2,"numPassedTests":1,"numFailedTests":1,"testResults":[{"assertionResults":[{"status":"passed","fullName":"1. a"},{"status":"failed","fullName":"2. b","failureMessages":["x"]}]}]}"#;
        let r = parse_vitest_report(report, 1, AT).unwrap();
        assert_eq!(
            r["tests"],
            json!([
                { "name": "1. a", "status": "passed" },
                { "name": "2. b", "status": "failed" },
            ])
        );
    }

    #[test]
    fn suite_that_fails_to_load_becomes_one_failure() {
        let report = r#"{"numTotalTests":0,"numFailedTestSuites":1,"testResults":[{"status":"failed","message":"\u001b[31mSyntaxError\u001b[39m"}]}"#;
        let r = parse_vitest_report(report, 0, AT).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(
            r["failures"],
            json!([{ "name": "Il file non si carica", "message": "SyntaxError" }])
        );
        assert_eq!(r["failed"], json!(1));
    }

    #[test]
    fn malformed_report_is_an_error_not_a_green_run() {
        for input in ["{nope", "null", r#"{"testResults":5}"#] {
            let err = parse_vitest_report(input, 0, AT).unwrap_err();
            assert!(
                err.starts_with("Report di vitest non valido"),
                "{input}: {err}"
            );
        }
    }

    #[test]
    fn key_order_matches_node() {
        let report =
            r#"{"numTotalTests":1,"numPassedTests":1,"numFailedTests":0,"testResults":[]}"#;
        let r = parse_vitest_report(report, 1, AT).unwrap();
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.starts_with("{\"ok\":"), "{text}");
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "ok",
                "passed",
                "failed",
                "total",
                "failures",
                "tests",
                "durationMs",
                "at"
            ]
        );
    }

    #[test]
    fn strip_ansi_keeps_unterminated_escapes() {
        assert_eq!(
            strip_ansi("a\u{1b}[31mb\u{1b}[c\u{1b}x\u{1b}[1;"),
            "ab\u{1b}x\u{1b}[1;"
        );
    }

    #[test]
    fn truncate_counts_utf16_units() {
        assert_eq!(truncate_utf16("😀a", 1), "");
        assert_eq!(truncate_utf16("😀a", 2), "😀");
    }
}
