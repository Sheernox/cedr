//! SKILL.md frontmatter and the skill content hash recorded in `cedr.lock`.

use std::fs;
use std::path::Path;

use saphyr::{LoadableYamlNode, Yaml};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::error::io_err;

/// A frontmatter field as written, so CG004 can tell "missing" from "not text".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Missing,
    NotText,
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Field,
    pub description: Field,
    pub allowed_tools: Vec<String>,
    /// 1-based lines of the `name` and `description` keys, when present.
    pub name_line: Option<usize>,
    pub description_line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedFrontmatter {
    Absent,
    Invalid(String),
    Valid(Frontmatter),
}

/// Parses the YAML block between a leading `---` line and the next `---` or `...` line.
pub fn frontmatter(text: &str) -> ParsedFrontmatter {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return ParsedFrontmatter::Absent;
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if matches!(line.trim_end(), "---" | "...") {
            closed = true;
            break;
        }
        yaml.push_str(line);
    }
    if !closed {
        return ParsedFrontmatter::Invalid("frontmatter has no closing --- line".into());
    }
    let docs = match Yaml::load_from_str(&yaml) {
        Ok(docs) => docs,
        Err(e) => return ParsedFrontmatter::Invalid(format!("frontmatter is not valid YAML: {e}")),
    };
    let Some(doc) = docs.first().filter(|d| d.is_mapping()) else {
        return ParsedFrontmatter::Invalid("frontmatter is not a YAML mapping".into());
    };
    let field = |key: &str| match doc.as_mapping_get(key) {
        None => Field::Missing,
        Some(v) => match v.as_str() {
            Some(s) => Field::Text(s.to_string()),
            None if v.is_null() => Field::Missing,
            None => Field::NotText,
        },
    };
    let allowed_tools = match doc.as_mapping_get("allowed-tools") {
        Some(v) if v.is_sequence() => v
            .as_sequence()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(v) => v.as_str().map(split_tool_list).unwrap_or_default(),
        None => Vec::new(),
    };
    let key_line = |key: &str| {
        let prefix = format!("{key}:");
        yaml.lines()
            .position(|l| l.starts_with(&prefix))
            .map(|i| i + 2)
    };
    ParsedFrontmatter::Valid(Frontmatter {
        name: field("name"),
        description: field("description"),
        allowed_tools,
        name_line: key_line("name"),
        description_line: key_line("description"),
    })
}

/// Splits `Bash(npm run *), Read Write` into tools at commas and spaces that
/// sit outside parentheses, since a tool pattern can itself contain spaces.
fn split_tool_list(list: &str) -> Vec<String> {
    let mut tools = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for c in list.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && (c == ',' || c.is_whitespace()) {
            if !current.is_empty() {
                tools.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tools.push(current);
    }
    tools
}

/// `sha256-<hex>` over every file in the skill directory, in sorted relative
/// path order, each fed as `<path>\0<byte length>\0<bytes>`. Symlinks are not
/// followed: their target text is hashed in place of content, so a link that
/// is repointed changes the hash.
pub fn content_hash(dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(dir, "", &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for rel in files {
        let path = dir.join(&rel);
        let bytes = file_bytes(&path)?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(bytes.len().to_string().as_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
    }
    Ok(format!("sha256-{}", hex(&hasher.finalize())))
}

/// Reads symlink target text rather than following it, for both hashing and lint.
pub(crate) fn file_bytes(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
    if meta.file_type().is_symlink() {
        Ok(fs::read_link(path)
            .map_err(io_err(path))?
            .to_string_lossy()
            .into_owned()
            .into_bytes())
    } else {
        fs::read(path).map_err(io_err(path))
    }
}

/// Relative paths (with `/`) of every file and symlink under `dir`.
pub(crate) fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let base = if prefix.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(prefix)
    };
    for entry in fs::read_dir(&base).map_err(io_err(&base))? {
        let entry = entry.map_err(io_err(&base))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type().map_err(io_err(&entry.path()))?;
        if kind.is_dir() {
            collect_files(dir, &rel, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(text: &str) -> Frontmatter {
        match frontmatter(text) {
            ParsedFrontmatter::Valid(f) => f,
            other => panic!("expected valid frontmatter, got {other:?}"),
        }
    }

    #[test]
    fn reads_fields_and_key_lines() {
        let f = valid("---\nname: deploy\ndescription: Ship it\n---\nbody\n");
        assert_eq!(f.name, Field::Text("deploy".into()));
        assert_eq!(f.description, Field::Text("Ship it".into()));
        assert_eq!(f.name_line, Some(2));
        assert_eq!(f.description_line, Some(3));
    }

    #[test]
    fn allowed_tools_as_string_or_list() {
        let f = valid("---\nname: a\nallowed-tools: Bash(npm run *), Read Write\n---\n");
        assert_eq!(f.allowed_tools, ["Bash(npm run *)", "Read", "Write"]);
        let f = valid("---\nname: a\nallowed-tools:\n  - Bash(git status)\n  - Read\n---\n");
        assert_eq!(f.allowed_tools, ["Bash(git status)", "Read"]);
    }

    #[test]
    fn missing_unclosed_and_non_text() {
        assert_eq!(frontmatter("# Title\n"), ParsedFrontmatter::Absent);
        assert!(matches!(
            frontmatter("---\nname: a\n"),
            ParsedFrontmatter::Invalid(_)
        ));
        let f = valid("---\nname: [a]\n---\n");
        assert_eq!(f.name, Field::NotText);
        assert_eq!(f.description, Field::Missing);
    }

    #[test]
    fn content_hash_tracks_paths_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/x.sh"), "b").unwrap();
        let first = content_hash(dir.path()).unwrap();
        assert!(first.starts_with("sha256-") && first.len() == 71);
        fs::write(dir.path().join("sub/x.sh"), "c").unwrap();
        let changed = content_hash(dir.path()).unwrap();
        assert_ne!(first, changed);
        fs::rename(dir.path().join("sub/x.sh"), dir.path().join("sub/y.sh")).unwrap();
        assert_ne!(changed, content_hash(dir.path()).unwrap());
    }
}
