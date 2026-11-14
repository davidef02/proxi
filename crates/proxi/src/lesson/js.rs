//! JavaScript semantics the lesson port must reproduce.

use std::io;

use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{Formatter, PrettyFormatter, Serializer};

/// JS `\s` / `String#trim`: includes U+FEFF, excludes U+0085.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// ECMAScript `Number::toString`: the digits V8 prints for a double.
pub fn js_number(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if f == 0.0 {
        // Also -0: String(-0) is "0".
        return "0".to_string();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    // LowerExp prints the shortest round-trip digits, the same s/k as V8.
    let formatted = format!("{:e}", f.abs());
    let (mantissa, exponent) = formatted.split_once('e').expect("{:e} has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let e: i64 = exponent.parse().expect("{:e} exponent is an integer");
    let n = e + 1;
    let body = if k <= n && n <= 21 {
        digits.clone() + &"0".repeat((n - k) as usize)
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let fraction = if k > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let exp_sign = if n - 1 >= 0 { "+" } else { "-" };
        format!("{}{fraction}e{exp_sign}{}", &digits[..1], (n - 1).abs())
    };
    format!("{sign}{body}")
}

pub fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(num)) => num.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// JS `String(x)` on parsed JSON.
pub fn js_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(num) => js_number(num.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        // Array#join: null and undefined render as "".
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// JS `"" + x`: `undefined` for a missing member.
pub fn js_concat(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(x) => js_to_string(x),
    }
}

/// `Array#join` element rule: missing and null elements become "".
pub fn js_join_item(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(x) => js_to_string(x),
    }
}

pub fn text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

/// serde_json formatter that prints numbers the way V8 does.
struct JsFormatter(PrettyFormatter<'static>);

impl Formatter for JsFormatter {
    fn begin_array<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.begin_array(writer)
    }

    fn end_array<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.end_array(writer)
    }

    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.begin_array_value(writer, first)
    }

    fn end_array_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.end_array_value(writer)
    }

    fn begin_object<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.begin_object(writer)
    }

    fn end_object<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.end_object(writer)
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.begin_object_key(writer, first)
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.begin_object_value(writer)
    }

    fn end_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        self.0.end_object_value(writer)
    }

    fn write_i64<W>(&mut self, writer: &mut W, value: i64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value.unsigned_abs() <= (1u64 << 53) {
            writer.write_all(value.to_string().as_bytes())
        } else {
            writer.write_all(js_number(value as f64).as_bytes())
        }
    }

    fn write_i128<W>(&mut self, writer: &mut W, value: i128) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value.unsigned_abs() <= (1u128 << 53) {
            writer.write_all(value.to_string().as_bytes())
        } else {
            writer.write_all(js_number(value as f64).as_bytes())
        }
    }

    fn write_u64<W>(&mut self, writer: &mut W, value: u64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value <= (1u64 << 53) {
            writer.write_all(value.to_string().as_bytes())
        } else {
            writer.write_all(js_number(value as f64).as_bytes())
        }
    }

    fn write_u128<W>(&mut self, writer: &mut W, value: u128) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value <= (1u128 << 53) {
            writer.write_all(value.to_string().as_bytes())
        } else {
            writer.write_all(js_number(value as f64).as_bytes())
        }
    }

    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(js_number(f64::from(value)).as_bytes())
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(js_number(value).as_bytes())
    }
}

/// `JSON.stringify(v, null, 2)`.
pub fn stringify_pretty(v: &Value) -> String {
    let mut ser = Serializer::with_formatter(Vec::new(), JsFormatter(PrettyFormatter::new()));
    v.serialize(&mut ser)
        .expect("serializing a Value cannot fail");
    String::from_utf8(ser.into_inner()).expect("JSON output is UTF-8")
}

pub fn render_data_island(doc: &Value) -> String {
    format!(
        "<script type=\"application/json\" id=\"lesson-data\">\n{}\n  </script>",
        stringify_pretty(doc).replace('<', "\\u003c")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_number_matches_v8() {
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1e-7), "1e-7");
        assert_eq!(js_number(0.000001), "0.000001");
        assert_eq!(js_number(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(1.5), "1.5");
        assert_eq!(js_number(9007199254740993u64 as f64), "9007199254740992");
        assert_eq!(js_number(2.5e-10), "2.5e-10");
        assert_eq!(js_number(100.0), "100");
        assert_eq!(js_number(0.1), "0.1");
        assert_eq!(js_number(-1.5e-7), "-1.5e-7");
        assert_eq!(js_number(1e300), "1e+300");
        assert_eq!(js_number(5e-324), "5e-324");
        assert_eq!(js_number(123.456), "123.456");
        assert_eq!(js_number(f64::NAN), "NaN");
        assert_eq!(js_number(f64::INFINITY), "Infinity");
        assert_eq!(js_number(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn js_trim_uses_the_js_set() {
        assert_eq!(js_trim("\u{feff} a \u{85}"), "a \u{85}");
        assert_eq!(js_trim("\u{9}\u{a}\u{b}\u{c}\u{d}\u{20}\u{a0}x"), "x");
        assert_eq!(js_trim("\u{2028}y\u{2029}"), "y");
    }

    #[test]
    fn js_truthy_and_to_string() {
        assert!(!js_truthy(None));
        assert!(!js_truthy(Some(&json!(null))));
        assert!(!js_truthy(Some(&json!(false))));
        assert!(js_truthy(Some(&json!(true))));
        assert!(!js_truthy(Some(&json!(0))));
        assert!(!js_truthy(Some(&json!(0.0))));
        assert!(js_truthy(Some(&json!(2))));
        assert!(!js_truthy(Some(&json!(""))));
        assert!(js_truthy(Some(&json!("x"))));
        assert!(js_truthy(Some(&json!([]))));
        assert!(js_truthy(Some(&json!({}))));

        assert_eq!(js_to_string(&json!(null)), "null");
        assert_eq!(js_to_string(&json!(true)), "true");
        assert_eq!(js_to_string(&json!(false)), "false");
        assert_eq!(js_to_string(&json!(1.0)), "1");
        assert_eq!(js_to_string(&json!("x")), "x");
        assert_eq!(js_to_string(&json!([1, null, "x"])), "1,,x");
        assert_eq!(js_to_string(&json!({"a": 1})), "[object Object]");

        assert_eq!(js_concat(None), "undefined");
        assert_eq!(js_concat(Some(&json!(null))), "null");
        assert_eq!(js_concat(Some(&json!(3))), "3");

        assert_eq!(js_join_item(None), "");
        assert_eq!(js_join_item(Some(&json!(null))), "");
        assert_eq!(js_join_item(Some(&json!(3))), "3");

        assert_eq!(text(None), "");
        assert_eq!(text(Some(&json!(1))), "");
        assert_eq!(text(Some(&json!("hi"))), "hi");
    }

    #[test]
    fn stringify_matches_v8_numbers() {
        let v: Value = serde_json::from_str(
            r#"{"a":1.0,"b":1e21,"c":1E-7,"d":-0,"e":9007199254740993,"f":0.1}"#,
        )
        .unwrap();
        assert_eq!(
            stringify_pretty(&v),
            "{\n  \"a\": 1,\n  \"b\": 1e+21,\n  \"c\": 1e-7,\n  \"d\": 0,\n  \"e\": 9007199254740992,\n  \"f\": 0.1\n}"
        );
    }

    #[test]
    fn stringify_keeps_key_order_and_empty_containers() {
        let v: Value = serde_json::from_str(r#"{"z":[],"a":{},"m":[1,{"k":"v"}]}"#).unwrap();
        assert_eq!(
            stringify_pretty(&v),
            "{\n  \"z\": [],\n  \"a\": {},\n  \"m\": [\n    1,\n    {\n      \"k\": \"v\"\n    }\n  ]\n}"
        );
    }

    #[test]
    fn stringify_escapes_like_v8() {
        let v = Value::String("\u{1}\u{1f}\u{8}\t\n\u{c}\r\"\\\u{7f}\u{2028}é".to_string());
        assert_eq!(
            stringify_pretty(&v),
            "\"\\u0001\\u001f\\b\\t\\n\\f\\r\\\"\\\\\u{7f}\u{2028}é\""
        );
    }

    #[test]
    fn data_island_escapes_lt() {
        let doc = json!({"metadata": {"lessonNumber": 1, "title": "</script>"}, "nodes": []});
        assert_eq!(
            render_data_island(&doc),
            "<script type=\"application/json\" id=\"lesson-data\">\n{\n  \"metadata\": {\n    \"lessonNumber\": 1,\n    \"title\": \"\\u003c/script>\"\n  },\n  \"nodes\": []\n}\n  </script>"
        );
    }
}
