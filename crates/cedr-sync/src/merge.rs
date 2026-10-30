//! Key ownership preserves unrelated values and their insertion order, including TOML comments.
use crate::Result;
use serde_json::{Value, json};
fn parts(key: &str) -> Vec<&str> {
    if key == "chat.tools.terminal.autoApprove" {
        vec![key]
    } else {
        key.split('.').collect()
    }
}
fn get<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    if path.is_empty() {
        Some(v)
    } else {
        get(v.get(path[0])?, &path[1..])
    }
}
fn remove(v: &mut Value, p: &[&str]) {
    if p.len() == 1 {
        if let Some(o) = v.as_object_mut() {
            o.remove(p[0]);
        }
    } else if let Some(child) = v.get_mut(p[0]) {
        remove(child, &p[1..]);
        if child.as_object().is_some_and(|m| m.is_empty()) {
            v.as_object_mut().expect("object").remove(p[0]);
        }
    }
}
fn set(v: &mut Value, p: &[&str], new: Value) {
    if p.len() == 1 {
        v[p[0]] = new;
    } else {
        if v.get(p[0]).is_none() {
            v[p[0]] = json!({});
        }
        set(&mut v[p[0]], &p[1..], new);
    }
}
pub fn value(path: &str, bytes: &[u8]) -> Result<Value> {
    if path.ends_with(".toml") {
        let text = std::str::from_utf8(bytes).map_err(|_| format!("{path}: invalid UTF-8"))?;
        let v: toml::Value = toml::from_str(text).map_err(|_| format!("{path}: invalid TOML"))?;
        serde_json::to_value(v).map_err(|_| format!("{path}: invalid TOML"))
    } else {
        serde_json::from_slice(bytes).map_err(|_| format!("{path}: invalid JSON"))
    }
}
pub fn owned(path: &str, bytes: &[u8], keys: &[String]) -> Result<Vec<u8>> {
    if keys.is_empty() {
        return Ok(bytes.to_vec());
    }
    let v = value(path, bytes)?;
    let mut out = json!({});
    for key in keys {
        if let Some(item) = get(&v, &parts(key)) {
            out[key] = item.clone();
        }
    }
    Ok(cedr_core::canonical::to_string(&out).into_bytes())
}
pub fn merge(
    path: &str,
    old: Option<&[u8]>,
    old_keys: &[String],
    new: Option<&[u8]>,
    new_keys: &[String],
) -> Result<Option<Vec<u8>>> {
    let mut v = match old {
        Some(b) => value(path, b)?,
        None => json!({}),
    };
    if !v.is_object() {
        return Err(format!("{path}: expected object"));
    }
    let desired = match new {
        Some(b) => serde_json::from_slice::<Value>(b)
            .map_err(|_| format!("{path}: invalid generated JSON"))?,
        None => json!({}),
    };
    for k in old_keys {
        if !new_keys.contains(k) {
            remove(&mut v, &parts(k));
        }
    }
    for key in new_keys {
        let parts = parts(key);
        for end in 1..parts.len() {
            if get(&v, &parts[..end]).is_some_and(|parent| !parent.is_object()) {
                return Err(format!("{path}: owned key parent has an incompatible type"));
            }
        }
    }
    for k in new_keys {
        if let Some(item) = get(&desired, &parts(k)) {
            set(&mut v, &parts(k), item.clone());
        }
    }
    if v.as_object().is_some_and(|o| o.is_empty()) {
        return Ok(None);
    }
    if path.ends_with(".toml") {
        let mut doc = match old {
            Some(b) => std::str::from_utf8(b)
                .map_err(|_| format!("{path}: invalid UTF-8"))?
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| format!("{path}: invalid TOML"))?,
            None => toml_edit::DocumentMut::new(),
        };
        for key in old_keys {
            if !new_keys.contains(key) {
                doc.remove(key);
            }
        }
        for key in new_keys {
            let item = desired.get(key).ok_or("missing generated key")?;
            let tv: toml::Value =
                serde_json::from_value(item.clone()).map_err(|_| "invalid TOML value")?;
            let wrapped = toml::to_string(&std::collections::BTreeMap::from([(key, tv)]))
                .map_err(|_| "invalid TOML value")?;
            let parsed = wrapped
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| "invalid generated TOML")?;
            doc[key] = parsed[key].clone();
        }
        Ok(Some(doc.to_string().into_bytes()))
    } else {
        Ok(Some(
            format!("{}\n", serde_json::to_string_pretty(&v).expect("json")).into_bytes(),
        ))
    }
}
