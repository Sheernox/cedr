//! The shared price catalog uses the most specific model prefix.
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
pub fn price(model: &str) -> Option<&'static Price> {
    #[derive(Deserialize)]
    struct Catalog {
        models: Vec<Price>,
    }
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str(include_str!("../prices.json")).expect("embedded price catalog")
    })
    .models
    .iter()
    .filter(|p| model.starts_with(&p.prefix))
    .max_by_key(|p| p.prefix.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn longest_prefix() {
        assert_eq!(
            price("muse-spark-1.3-contributor-trial").unwrap().input,
            0.1
        );
        assert!(price("unknown").is_none());
    }
}
