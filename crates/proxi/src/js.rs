//! JSON helpers that behave like JavaScript, for byte parity with the Node server.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Number, Value};

/// 2^53, the largest integer a JS number holds exactly.
const MAX_SAFE: f64 = 9007199254740992.0;

/// `JSON.parse` plus JS number shapes: an integral float comes back as an integer (`1.0` -> `1`).
pub fn parse(text: &str) -> Result<Value, serde_json::Error> {
    let value: Value = serde_json::from_str(text)?;
    Ok(normalise(value))
}

fn normalise(value: Value) -> Value {
    match value {
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                return Value::Number(n);
            }
            match n.as_f64() {
                Some(x) if is_safe_integer(x) => Value::from(x as i64),
                _ => Value::Number(n),
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalise).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, normalise(item)))
                .collect(),
        ),
        other => other,
    }
}

fn is_safe_integer(x: f64) -> bool {
    x.is_finite() && x.fract() == 0.0 && x.abs() <= MAX_SAFE
}

/// A JS number as JSON: `5`, never `5.0`; `-0` prints as `0`; NaN and infinities become `null`.
pub fn number(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    if is_safe_integer(x) {
        return Value::from(x as i64);
    }
    Value::Number(Number::from_f64(x).unwrap())
}

/// JS truthiness: only null, false, 0, NaN and "" are falsy (empty arrays and objects are true).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// JS `String(v)` / template interpolation.
pub fn display(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => {
            let shape = number(n.as_f64().unwrap_or(f64::NAN));
            if shape.is_null() {
                "null".to_string()
            } else {
                serde_json::to_string(&shape).unwrap_or_default()
            }
        }
        Some(Value::String(s)) => s.clone(),
        // Array.join: null and undefined elements print as nothing.
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    display(Some(item))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// JS `Number(v)`, coercion included.
pub fn to_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `Number.isFinite(v)`: no coercion, strings and booleans are not numbers.
pub fn finite(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

/// tmp + rename; Windows can refuse the rename (EPERM, AV/indexer), so fall back to a direct write.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    fs::write(&tmp, data)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::write(path, data)?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-js-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn string_of(value: &Value) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn parse_reads_integral_floats_as_integers() {
        let parsed =
            parse("{\"a\":1.0,\"b\":2.5,\"c\":[3.0],\"d\":-0.0,\"e\":1e300,\"f\":7}").unwrap();
        assert_eq!(
            string_of(&parsed),
            "{\"a\":1,\"b\":2.5,\"c\":[3],\"d\":0,\"e\":1e+300,\"f\":7}"
        );
    }

    #[test]
    fn number_keeps_the_js_shape() {
        assert_eq!(string_of(&number(64.0)), "64");
        assert_eq!(string_of(&number(-0.0)), "0");
        assert_eq!(string_of(&number(42.5)), "42.5");
        assert_eq!(string_of(&number(f64::NAN)), "null");
        assert_eq!(string_of(&number(f64::INFINITY)), "null");
    }

    #[test]
    fn truthy_follows_js() {
        assert!(!truthy(None));
        assert!(!truthy(Some(&Value::Null)));
        assert!(!truthy(Some(&json!(false))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!(""))));
        assert!(truthy(Some(&json!("0"))));
        assert!(truthy(Some(&json!([]))));
        assert!(truthy(Some(&json!({}))));
    }

    #[test]
    fn display_follows_string() {
        assert_eq!(display(None), "undefined");
        assert_eq!(display(Some(&Value::Null)), "null");
        assert_eq!(display(Some(&json!(true))), "true");
        assert_eq!(display(Some(&json!(1))), "1");
        assert_eq!(display(Some(&json!(1.5))), "1.5");
        assert_eq!(display(Some(&json!("x"))), "x");
        assert_eq!(display(Some(&json!([1, null, "a"]))), "1,,a");
        assert_eq!(display(Some(&json!({}))), "[object Object]");
    }

    #[test]
    fn to_number_follows_number() {
        assert_eq!(to_number(Some(&json!("  2 "))), 2.0);
        assert_eq!(to_number(Some(&json!(""))), 0.0);
        assert!(to_number(Some(&json!("x"))).is_nan());
        assert_eq!(to_number(Some(&json!(true))), 1.0);
        assert_eq!(to_number(Some(&json!(null))), 0.0);
        assert!(to_number(None).is_nan());
    }

    #[test]
    fn finite_does_not_coerce() {
        assert_eq!(finite(Some(&json!("5"))), None);
        assert_eq!(finite(Some(&json!(5))), Some(5.0));
        assert_eq!(finite(Some(&Value::Null)), None);
    }

    #[test]
    fn write_atomic_leaves_the_bytes_and_no_tmp() {
        let dir = temp_dir("atomic");
        let file = dir.join("layout.json");
        write_atomic(&file, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"{\"a\":1}");
        assert!(!dir.join("layout.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
