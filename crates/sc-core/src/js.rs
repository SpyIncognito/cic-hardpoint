//! The few JavaScript conversions the display code relies on, so text output matches the
//! TypeScript core byte for byte.

use serde_json::Value;

/// `Boolean(v)`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Number(v)`. Arrays and objects are NaN (JS would coerce a one-element array; frames never
/// carry one where a number is read).
pub fn to_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => string_to_number(s),
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

fn string_to_number(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    let radix = match s.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        return parse_radix(&s[2..], radix);
    }
    // Rust also reads "inf" and "nan", which JS does not; both are non-finite either way, and
    // every caller treats non-finite alike.
    s.parse().unwrap_or(f64::NAN)
}

fn parse_radix(digits: &str, radix: u32) -> f64 {
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return f64::NAN;
    }
    match u128::from_str_radix(digits, radix) {
        Ok(n) => n as f64,
        Err(_) => digits
            .chars()
            .filter_map(|c| c.to_digit(radix))
            .fold(0.0, |acc, d| acc * f64::from(radix) + f64::from(d)),
    }
}

/// `String(v)`.
pub fn to_display_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), number_to_string),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    to_display_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// `Number.prototype.toString()`: the shortest round-trip digits, laid out as ECMA-262 says.
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    // `{:e}` gives the shortest round-trip digits, as JS uses: "d.ddde±x".
    let exp_form = format!("{:e}", x.abs());
    let (mantissa, exp) = exp_form
        .split_once('e')
        .expect("LowerExp always has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().expect("LowerExp exponent is an integer") + 1;

    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (int, frac) = digits.split_at(n as usize);
        format!("{int}.{frac}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e_sign = if e < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        let dot = if rest.is_empty() { "" } else { "." };
        format!("{first}{dot}{rest}e{e_sign}{}", e.abs())
    };
    format!("{sign}{body}")
}

/// `Number.prototype.toFixed(digits)`. Rust's `{:.N}` rounds an exact tie to even; JS rounds it
/// away from zero (0.125 to "0.13", 10.25 to "10.3"), so round the exact decimal expansion here.
pub fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return number_to_string(x);
    }
    // 1100 places hold the full expansion of any f64 (at most 1074 fractional digits).
    let exact = format!("{:.1100}", x.abs());
    let (int, frac) = exact.split_once('.').expect("formatted with a fraction");
    let mut kept: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes()[digits] >= b'5' {
        round_up(&mut kept);
    }
    let int_len = kept.len() - digits;
    let (int, frac) = kept.split_at(int_len);
    let int = std::str::from_utf8(int).expect("ASCII digits");
    let frac = std::str::from_utf8(frac).expect("ASCII digits");
    // JS keeps the sign of a negative number that rounds to zero ("-0.00") but not of -0.
    let sign = if x < 0.0 { "-" } else { "" };
    if digits == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    }
}

fn round_up(digits: &mut Vec<u8>) {
    for d in digits.iter_mut().rev() {
        if *d == b'9' {
            *d = b'0';
        } else {
            *d += 1;
            return;
        }
    }
    digits.insert(0, b'1');
}

/// `String.prototype.trim`: JS whitespace and line terminators, which differ from Rust's
/// `White_Space` (JS trims U+FEFF, Rust trims U+0085).
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)` in UTF-16 units; a surrogate pair cut in half is dropped whole, since a Rust
/// string cannot hold the lone half JS would keep.
pub fn utf16_prefix(s: &str, n: usize) -> String {
    let mut len = 0;
    s.chars()
        .take_while(|c| {
            len += c.len_utf16();
            len <= n
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_fixed_rounds_ties_away_from_zero_like_node() {
        // Expected strings printed by node 22: `x.toFixed(d)`.
        let cases = [
            (0.125, 2, "0.13"),
            (2.675, 2, "2.67"),
            (1.005, 2, "1.00"),
            (10.25, 1, "10.3"),
            (100.5, 0, "101"),
            (12.5, 0, "13"),
            (-0.125, 2, "-0.13"),
            (-0.0, 2, "0.00"),
            (0.5, 0, "1"),
            (1.5, 0, "2"),
            (2.5, 0, "3"),
            (-2.5, 0, "-3"),
            (1e21, 0, "1e+21"),
            (1.45, 1, "1.4"),
            (0.045, 2, "0.04"),
            (-0.001, 2, "-0.00"),
            (9.995, 2, "9.99"),
            (99.95, 1, "100.0"),
        ];
        for (x, d, want) in cases {
            assert_eq!(to_fixed(x, d), want, "{x}.toFixed({d})");
        }
    }

    #[test]
    fn number_to_string_matches_node() {
        let cases = [
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (123.456, "123.456"),
            (-0.0, "0"),
            (1e300, "1e+300"),
            (0.000001, "0.000001"),
            (100.0, "100"),
            (-42.5, "-42.5"),
        ];
        for (x, want) in cases {
            assert_eq!(number_to_string(x), want, "String({x})");
        }
    }

    #[test]
    fn coercions_follow_js() {
        assert_eq!(to_number(&json!("212.4")), 212.4);
        assert_eq!(to_number(&json!(" 12 ")), 12.0);
        assert_eq!(to_number(&json!("")), 0.0);
        assert_eq!(to_number(&json!("0x1A")), 26.0);
        assert!(to_number(&json!("abc")).is_nan());
        assert_eq!(to_number(&json!(true)), 1.0);
        assert!(!truthy(&json!("")));
        assert!(!truthy(&json!(0)));
        assert!(truthy(&json!({})));
        assert_eq!(to_display_string(&json!(1e21)), "1e+21");
        assert_eq!(to_display_string(&json!([1, null, "a"])), "1,,a");
    }
}
