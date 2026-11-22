//! JSON as the TypeScript service writes and reads it: `JSON.stringify` number forms, and
//! `Object.keys` order for the keys of an object a page or file sent.

use serde::Serialize;
use serde_json::{Map, Number, Value};

/// Largest integer a JavaScript number holds exactly.
const MAX_SAFE: f64 = 9_007_199_254_740_992.0;

/// `JSON.stringify(value)`: an integral float is written without a fraction (`1`, not `1.0`).
pub fn to_text(value: &impl Serialize) -> String {
    to_js_value(value).to_string()
}

/// `JSON.stringify(value, null, 2)`.
pub fn to_pretty_text(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(&to_js_value(value)).expect("a JSON value serializes")
}

pub fn to_js_value(value: &impl Serialize) -> Value {
    let mut v = serde_json::to_value(value).expect("service messages serialize to JSON");
    normalize_numbers(&mut v);
    v
}

fn normalize_numbers(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| n.is_f64())
                && f.fract() == 0.0
                && f.abs() < MAX_SAFE
            {
                #[allow(clippy::cast_possible_truncation)]
                let i = f as i64;
                *n = Number::from(i);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_numbers),
        Value::Object(map) => map.values_mut().for_each(normalize_numbers),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// `Object.keys(obj)`: array-index keys first in ascending order, then the rest as written.
pub fn js_keys(map: &Map<String, Value>) -> Vec<&str> {
    let mut indexes: Vec<(u32, &str)> = map
        .keys()
        .filter_map(|k| array_index(k).map(|i| (i, k.as_str())))
        .collect();
    indexes.sort_unstable_by_key(|(i, _)| *i);
    indexes
        .into_iter()
        .map(|(_, k)| k)
        .chain(
            map.keys()
                .map(String::as_str)
                .filter(|k| array_index(k).is_none()),
        )
        .collect()
}

fn array_index(key: &str) -> Option<u32> {
    let digits = !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit());
    if !digits || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&i| i != u32::MAX)
}

/// `Number.isInteger(v)` for a JSON value.
pub fn is_integer(v: &Value) -> bool {
    v.as_f64().is_some_and(|f| f.fract() == 0.0)
}

/// `Number.isInteger(v)` for a JSON value, as the number when it is in `min..=max`.
pub fn int_in(v: Option<&Value>, min: i64, max: i64) -> Option<i64> {
    let f = v?.as_f64()?;
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    (f.fract() == 0.0 && f >= min as f64 && f <= max as f64).then_some(f as i64)
}

/// `String(v)`, with `None` standing for undefined.
pub fn js_string(v: Option<&Value>) -> String {
    v.map_or_else(|| "undefined".to_owned(), sc_core::js::to_display_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_integral_floats_as_integers() {
        assert_eq!(
            to_text(&json!({ "a": 1.0, "b": 0.5, "c": [-0.0, 2.25] })),
            r#"{"a":1,"b":0.5,"c":[0,2.25]}"#
        );
    }

    #[test]
    fn orders_keys_as_object_keys_does() {
        let v: Value = serde_json::from_str(r#"{"b":1,"10":2,"a":3,"2":4,"01":5}"#).unwrap();
        assert_eq!(js_keys(v.as_object().unwrap()), ["2", "10", "b", "a", "01"]);
    }

    #[test]
    fn reads_integers_like_number_is_integer() {
        assert_eq!(int_in(Some(&json!(3.0)), 0, 5), Some(3));
        assert_eq!(int_in(Some(&json!(1.5)), 0, 5), None);
        assert_eq!(int_in(Some(&json!("3")), 0, 5), None);
        assert_eq!(int_in(Some(&json!(6)), 0, 5), None);
        assert_eq!(int_in(None, 0, 5), None);
    }
}
