//! Pretty-printer for [`serde_json::Value`] with optional ANSI colouring.
//!
//! `serde_json` can already pretty-print, but it has no syntax highlighting and
//! pulling in a whole crate for a few escape codes is not worth it. This walks
//! the value with a two-space indent and, when `color` is set, wraps each token
//! in an SGR sequence.

use serde_json::Value;

const RESET: &str = "\x1b[0m";
const KEY: &str = "\x1b[34;1m"; // bold blue
const STR: &str = "\x1b[32m"; // green
const NUM: &str = "\x1b[33m"; // yellow
const LIT: &str = "\x1b[35m"; // magenta (true / false)
const NULL: &str = "\x1b[90m"; // bright black

/// Render `value` as pretty JSON, colourised when `color` is true, with a
/// trailing newline.
pub fn to_string(value: &Value, color: bool) -> String {
    let mut out = String::new();
    write(&mut out, value, 0, color);
    out.push('\n');
    out
}

fn paint(out: &mut String, color: bool, code: &str, text: &str) {
    if color {
        out.push_str(code);
        out.push_str(text);
        out.push_str(RESET);
    } else {
        out.push_str(text);
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write(out: &mut String, value: &Value, depth: usize, color: bool) {
    match value {
        Value::Null => paint(out, color, NULL, "null"),
        Value::Bool(b) => paint(out, color, LIT, if *b { "true" } else { "false" }),
        Value::Number(n) => paint(out, color, NUM, &n.to_string()),
        Value::String(s) => paint(out, color, STR, &encode_str(s)),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write(out, item, depth + 1, color);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            let len = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                indent(out, depth + 1);
                paint(out, color, KEY, &encode_str(k));
                out.push_str(": ");
                write(out, v, depth + 1, color);
                if i + 1 < len {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// A JSON string literal, quotes and escaping included. `serde_json` already
/// knows how to do this correctly, so lean on it.
fn encode_str(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_round_trips() {
        let v = json!({ "b": 1, "a": [true, null, "x"], "empty": {} });
        let rendered = to_string(&v, false);
        let back: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn colour_adds_escapes_and_still_parses_when_stripped() {
        let v = json!({ "n": 2, "s": "hi" });
        let rendered = to_string(&v, true);
        assert!(rendered.contains("\x1b["));
        let stripped = strip_ansi(&rendered);
        let back: Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v, back);
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
