//! The shared price catalog. A model is priced by the row for its own version, else by its
//! family's nearest older version, so a new release of a known family is priced before the table
//! lists it (docs/monitor.md, Pricing).
use serde::Deserialize;
use std::sync::OnceLock;
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    #[serde(rename = "match")]
    pub prefix: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write5m: f64,
    pub cache_write1h: f64,
}

/// How a model found its price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// A row for the model's own version.
    Exact,
    /// Another version of the model's family, an estimate until the table lists the model.
    Family,
}

/// A model id split around its first version: "gpt-6.1-sol" is ("gpt-", [6, 1], "-sol") and
/// "claude-fable-5-1-20261101" is ("claude-fable-", [5, 1], "-20261101"). Versions are digit runs
/// joined by `.` or `-` that start a `-` separated word; a run of eight digits or more is a date,
/// not part of the version. An id without one is (id, [], "").
fn split(id: &str) -> (&str, Vec<u64>, &str) {
    let b = id.as_bytes();
    let Some(start) = (0..b.len()).find(|&i| b[i].is_ascii_digit() && (i == 0 || b[i - 1] == b'-'))
    else {
        return (id, Vec::new(), "");
    };
    let (mut version, mut end, mut i) = (Vec::new(), start, start);
    loop {
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits >= 8 {
            break;
        }
        version.push(id[i..i + digits].parse().unwrap_or(u64::MAX));
        end = i + digits;
        match (b.get(end), b.get(end + 1)) {
            (Some(b'.' | b'-'), Some(c)) if c.is_ascii_digit() => i = end + 1,
            _ => break,
        }
    }
    (&id[..start], version, &id[end..])
}

/// The price of `model`, and whether it is the model's own row. An exact row matches the id as a
/// prefix and names the same version (or none), and the longest wins. Otherwise a row of the same
/// family (the id's words around the version, with any suffix the row has) and an older version
/// stands in: the most specific suffix first, then the newest version. A version older than every
/// row, or of an unknown family, is unpriced.
pub fn price(model: &str) -> Option<(&'static Price, Basis)> {
    #[derive(Deserialize)]
    struct Catalog {
        models: Vec<Price>,
    }
    static C: OnceLock<Catalog> = OnceLock::new();
    let rows = &C
        .get_or_init(|| {
            serde_json::from_str(include_str!("../prices.json")).expect("embedded price catalog")
        })
        .models;
    let (stem, version, rest) = split(model);
    let exact = rows
        .iter()
        .filter(|p| model.starts_with(&p.prefix))
        .filter(|p| {
            let v = split(&p.prefix).1;
            v.is_empty() || v == version
        })
        .max_by_key(|p| p.prefix.len());
    if let Some(p) = exact {
        return Some((p, Basis::Exact));
    }
    if version.is_empty() {
        return None;
    }
    rows.iter()
        .filter_map(|p| {
            let (s, v, r) = split(&p.prefix);
            (s == stem && !v.is_empty() && v < version && rest.starts_with(r)).then_some((
                r.len(),
                v,
                p,
            ))
        })
        .max_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)))
        .map(|(_, _, p)| (p, Basis::Family))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn priced(model: &str) -> Option<(&str, Basis)> {
        price(model).map(|(p, basis)| (p.prefix.as_str(), basis))
    }
    #[test]
    fn exact_rows_win_by_longest_prefix_of_the_same_version() {
        assert_eq!(
            priced("muse-spark-1.3-contributor-trial"),
            Some(("muse-spark-1.3-contributor", Basis::Exact))
        );
        assert_eq!(
            priced("claude-opus-5-5-20261101"),
            Some(("claude-opus-5-5", Basis::Exact)),
            "a date suffix is the same version"
        );
        assert_eq!(priced("grok-4.7-build"), Some(("grok-4.7", Basis::Exact)));
        assert_eq!(priced("unknown"), None);
    }
    #[test]
    fn a_new_version_of_a_known_family_takes_its_newest_older_row() {
        // A shorter version's row ("claude-opus-5") is not the price of a later point release.
        assert_eq!(
            priced("claude-opus-5-6"),
            Some(("claude-opus-5-5", Basis::Family))
        );
        assert_eq!(
            priced("claude-fable-5-3[1m]"),
            Some(("claude-fable-5-2", Basis::Family))
        );
        assert_eq!(priced("gpt-6.2-sol"), Some(("gpt-6.1-sol", Basis::Family)));
        assert_eq!(
            priced("muse-spark-1.4-contributor"),
            Some(("muse-spark-1.3-contributor", Basis::Family)),
            "the most specific suffix first"
        );
        assert_eq!(priced("gpt-7-nova"), None, "an unknown family");
        assert_eq!(priced("claude-opus-3"), None, "older than every row");
    }
}
