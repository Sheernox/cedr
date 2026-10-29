//! Canonical JSON: object keys sorted, no whitespace. The surface hash and the
//! introspected schema text both depend on it being byte-for-byte stable, so it
//! does not rely on serde_json's map ordering features.

use serde_json::Value;

pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, value)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("string serializes"));
                out.push(':');
                write(value, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).expect("scalar serializes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_keys_at_every_depth_without_whitespace() {
        let value: Value =
            serde_json::from_str(r#"{ "b": [ {"z": 1, "a": null} ], "a": "x\"y" }"#).unwrap();
        assert_eq!(to_string(&value), r#"{"a":"x\"y","b":[{"a":null,"z":1}]}"#);
    }
}
