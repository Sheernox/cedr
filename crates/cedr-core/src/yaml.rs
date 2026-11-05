//! Bound nesting before constructing recursive YAML values from repository input.

use saphyr_parser::{Event, Parser};

pub(crate) fn check_depth(text: &str) -> Result<(), String> {
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut anchors = std::collections::BTreeMap::new();
    let too_deep = || "YAML nesting exceeds 128 levels".to_string();
    for event in Parser::new_from_str(text) {
        match event.map_err(|e| e.to_string())?.0 {
            Event::DocumentStart(_) => anchors.clear(),
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => {
                if stack.len() >= 128 {
                    return Err(too_deep());
                }
                stack.push((anchor, 1));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (anchor, depth) = stack.pop().ok_or("unbalanced YAML collection")?;
                if anchor != 0 {
                    anchors.insert(anchor, depth);
                }
                if let Some((_, parent_depth)) = stack.last_mut() {
                    *parent_depth = (*parent_depth).max(depth + 1);
                }
            }
            Event::Scalar(_, _, anchor, _) if anchor != 0 => {
                anchors.insert(anchor, 0);
            }
            Event::Alias(anchor) => {
                // Alias expansion can create a deep tree from shallow syntax. Track each
                // anchor's expanded depth without building or cloning its recursive value.
                let depth = *anchors
                    .get(&anchor)
                    .ok_or("recursive or unknown YAML alias")?;
                if stack.len() + depth > 128 {
                    return Err(too_deep());
                }
                if let Some((_, parent_depth)) = stack.last_mut() {
                    *parent_depth = (*parent_depth).max(depth + 1);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_depth;

    #[test]
    fn nesting_limit_is_checked_before_recursive_loading() {
        assert!(check_depth(&format!("{}0", "- ".repeat(128))).is_ok());
        assert!(check_depth(&format!("{}0", "- ".repeat(129))).is_err());
        assert!(check_depth(&format!("{}0", "- ".repeat(500_000))).is_err());
        let nested = format!(
            "a: &a {}0{}\nb: &b [*a]\n",
            "[".repeat(127),
            "]".repeat(127)
        );
        assert!(check_depth(&nested).is_err());
        assert!(check_depth("name: &name test\ndescription: *name\n").is_ok());
    }
}
