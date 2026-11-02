//! Edits a `routes.toml` in place for `cedr route thresholds`, `add`, `remove`, `enable` and
//! `disable` (docs/route.md, Editing). Works on the document, not the parsed config, so comments and
//! layout survive; every result is validated as a config before it is written.

use std::io::Write;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, TableLike, Value, table, value};

use crate::config::{Config, Limits, RoutesFrom, load_local, rules_hash};
use crate::error::{Error, Result, io_err, parse_err};
use crate::pick::Size;

/// Which `thresholds` layer a command reads or edits.
pub enum Scope<'a> {
    All,
    Harness(&'a str),
    Route(&'a str),
}

pub struct CandidateEdit<'a> {
    pub target: &'a str,
    pub effort: Option<&'a str>,
    pub tier: Option<i64>,
    pub sizes: Option<Vec<Size>>,
    /// 0-based position; a new candidate defaults to before a final `self`, or last.
    pub at: Option<usize>,
}

pub struct Applied {
    pub path: PathBuf,
    pub old_rules: String,
    pub new_rules: String,
}

/// Replaces the file at `path` with what `edit` makes of its text, when that validates. The file
/// keeps its mode, since it may hold a readings URL with a token; a symlink stays a symlink.
pub fn apply(path: &Path, edit: impl FnOnce(&str) -> Result<String>) -> Result<Applied> {
    let path = std::fs::canonicalize(path).map_err(io_err(path))?;
    if let Some(sync) = load_local(&path)?
        .sync
        .filter(|s| s.routes == RoutesFrom::Org)
    {
        return Err(Error::Usage(format!(
            "{} uses the shared routes of {}: change them where the org keeps them",
            path.display(),
            sync.org
        )));
    }
    let old = std::fs::read_to_string(&path).map_err(io_err(&path))?;
    let new = edit(&old)?;
    Config::parse(&new, &path)?;
    write_keeping_mode(&path, new.as_bytes())?;
    Ok(Applied {
        old_rules: rules_hash(old.as_bytes()),
        new_rules: rules_hash(new.as_bytes()),
        path,
    })
}

fn write_keeping_mode(path: &Path, bytes: &[u8]) -> Result<()> {
    let permissions = std::fs::metadata(path).map_err(io_err(path))?.permissions();
    let tmp = path.with_file_name(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            // Restrict before the contents go in, so a private file is never briefly readable.
            f.set_permissions(permissions)?;
            f.write_all(bytes)
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(path)(e)
    })
}

fn document(text: &str) -> Result<DocumentMut> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| parse_err(Path::new("routes.toml"), e.message()))
}

/// The thresholds in force at a scope, naming an unknown route or harness.
pub fn scope_limits(config: &Config, scope: &Scope) -> Result<Limits> {
    Ok(match scope {
        Scope::All => config.limits(None, None),
        Scope::Harness(h) => {
            if !config.harnesses.contains_key(*h) {
                return Err(unknown("harness", h));
            }
            config.limits(Some(h), None)
        }
        Scope::Route(r) => {
            let route = config.routes.get(*r).ok_or_else(|| unknown("route", r))?;
            config.limits(None, Some(route))
        }
    })
}

fn unknown(kind: &str, id: &str) -> Error {
    Error::Usage(format!("unknown {kind} {id:?}"))
}

fn entry<'a>(
    doc: &'a mut DocumentMut,
    section: &str,
    id: &str,
    kind: &str,
) -> Result<&'a mut Item> {
    doc.get_mut(section)
        .and_then(Item::as_table_like_mut)
        .and_then(|t| t.get_mut(id))
        .ok_or_else(|| unknown(kind, id))
}

fn scope_item<'a>(doc: &'a mut DocumentMut, scope: &Scope) -> Result<&'a mut Item> {
    match scope {
        Scope::All => Ok(doc.as_item_mut()),
        Scope::Harness(h) => entry(doc, "harnesses", h, "harness"),
        Scope::Route(r) => entry(doc, "routes", r, "route"),
    }
}

const NUMBER_KEYS: [&str; 6] = [
    "fresh_seconds",
    "session_conserve",
    "weekly_conserve",
    "out",
    "session_out",
    "weekly_out",
];
const SIZE_KEYS: [&str; 2] = ["conserve_sizes", "unknown_sizes"];

fn known_key(key: &str) -> Result<()> {
    if NUMBER_KEYS.contains(&key) || SIZE_KEYS.contains(&key) {
        return Ok(());
    }
    Err(Error::Usage(format!(
        "unknown threshold {key:?}; thresholds: {}, {}",
        NUMBER_KEYS.join(", "),
        SIZE_KEYS.join(", ")
    )))
}

fn threshold_value(key: &str, raw: &str) -> Result<Item> {
    if SIZE_KEYS.contains(&key) {
        let mut sizes = Array::new();
        for s in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            sizes.push(Size::parse(s)?.as_str());
        }
        return Ok(value(sizes));
    }
    let number = || Error::Usage(format!("{key} {raw:?} is not a number"));
    if key == "fresh_seconds" {
        let seconds = raw.parse::<i64>().map_err(|_| number())?;
        if seconds < 0 {
            return Err(Error::Usage(format!("{key} {raw} must not be negative")));
        }
        return Ok(value(seconds));
    }
    let n = raw.parse::<f64>().map_err(|_| number())?;
    if !(0.0..=100.0).contains(&n) {
        return Err(Error::Usage(format!(
            "{key} {raw} must be between 0 and 100"
        )));
    }
    Ok(if n.fract() == 0.0 && n.abs() < 1e15 {
        value(n as i64)
    } else {
        value(n)
    })
}

/// Applies `--unset`, then `--set`, to one layer; a layer left empty is removed from the file.
pub fn set_thresholds(
    text: &str,
    scope: &Scope,
    set: &[String],
    unset: &[String],
) -> Result<String> {
    let mut doc = document(text)?;
    let parent = scope_item(&mut doc, scope)?;
    let mut changes = Vec::new();
    for pair in set {
        let (key, raw) = pair
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--set {pair:?} is not KEY=VALUE")))?;
        known_key(key)?;
        changes.push((key, threshold_value(key, raw)?));
    }
    for key in unset {
        known_key(key)?;
    }
    let inline = matches!(parent, Item::Value(_));
    let holder = parent
        .as_table_like_mut()
        .ok_or_else(|| Error::Usage("that scope is not a table in the file".to_string()))?;
    if !holder.contains_key("thresholds") {
        if changes.is_empty() {
            return Ok(text.to_string());
        }
        let fresh = if inline {
            value(InlineTable::new())
        } else {
            table()
        };
        holder.insert("thresholds", fresh);
    }
    let layer = holder
        .get_mut("thresholds")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| Error::Usage("thresholds in the file is not a table".to_string()))?;
    for key in unset {
        layer.remove(key);
    }
    for (key, item) in changes {
        replace(layer, key, item);
    }
    if layer.is_empty() {
        holder.remove("thresholds");
    }
    Ok(doc.to_string())
}

/// Sets a key, keeping the comment on the value it replaces.
fn replace(holder: &mut dyn TableLike, key: &str, mut item: Item) {
    if let (Some(old), Some(new)) = (
        holder.get(key).and_then(Item::as_value),
        item.as_value_mut(),
    ) {
        *new.decor_mut() = old.decor().clone();
    }
    holder.insert(key, item);
}

pub fn set_enabled(text: &str, route: &str, on: bool) -> Result<String> {
    let mut doc = document(text)?;
    let holder = entry(&mut doc, "routes", route, "route")?
        .as_table_like_mut()
        .ok_or_else(|| unknown("route", route))?;
    replace(holder, "enabled", value(on));
    Ok(doc.to_string())
}

/// The route's candidates as an inline array, the form `add` and `remove` edit. A
/// `[[routes.<id>.candidates]]` list becomes one, since those tables cannot be reordered.
fn candidates<'a>(doc: &'a mut DocumentMut, route: &str) -> Result<&'a mut Array> {
    let holder = entry(doc, "routes", route, "route")?
        .as_table_like_mut()
        .ok_or_else(|| unknown("route", route))?;
    let item = holder
        .get_mut("candidates")
        .ok_or_else(|| Error::Usage(format!("route {route} has no candidates list")))?;
    if let Some(tables) = item.as_array_of_tables() {
        let mut array = Array::new();
        for t in tables.iter() {
            let mut inline = t.clone().into_inline_table();
            inline.fmt();
            array.push_formatted(Value::InlineTable(inline).decorated("\n  ", ""));
        }
        array.set_trailing("\n");
        array.set_trailing_comma(true);
        *item = value(array);
    }
    item.as_array_mut()
        .ok_or_else(|| Error::Usage(format!("route {route}: candidates is not a list")))
}

fn target_of(v: &Value) -> Option<&str> {
    v.as_inline_table()?.get("target")?.as_str()
}

fn multiline(a: &Array) -> bool {
    a.iter().any(|v| {
        v.decor()
            .prefix()
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.contains('\n'))
    })
}

/// In a multi-line array, the text before an item is the end of the previous item's line, where its
/// trailing comment sits, and then the item's own comment lines and indent. Splits it there.
fn split_lines(text: &str) -> (&str, &str) {
    text.split_at(text.find('\n').map_or(0, |i| i + 1))
}

/// The text between the item before `at` and whatever follows: the item at `at`, or the array's end.
fn gap_at(a: &Array, at: usize) -> String {
    let text = match a.get(at) {
        Some(item) => item.decor().prefix().and_then(|p| p.as_str()),
        None => a.trailing().as_str(),
    };
    text.unwrap_or("").to_string()
}

fn set_gap_at(a: &mut Array, at: usize, text: String) {
    match a.get_mut(at) {
        Some(item) => item.decor_mut().set_prefix(text),
        None => a.set_trailing(text),
    }
}

/// An item out of a multi-line array, with the comments that belong to it.
struct Lifted {
    item: Value,
    /// Its comment lines and indent.
    above: String,
    /// The end of its line, with any trailing comment.
    after: String,
}

/// Removes the item, leaving every other item's comments on that item.
fn lift(a: &mut Array, at: usize) -> Lifted {
    let before = gap_at(a, at);
    let (prev_end, above) = split_lines(&before);
    let next = gap_at(a, at + 1);
    let (after, next_above) = split_lines(&next);
    set_gap_at(a, at + 1, format!("{prev_end}{next_above}"));
    Lifted {
        above: above.to_string(),
        after: after.to_string(),
        item: a.remove(at),
    }
}

/// The reverse of [`lift`], at any position.
fn land(a: &mut Array, at: usize, lifted: Lifted) {
    let before = gap_at(a, at);
    let (prev_end, next_above) = split_lines(&before);
    let (prefix, next) = (
        format!("{prev_end}{}", lifted.above),
        format!("{}{next_above}", lifted.after),
    );
    set_gap_at(a, at, next);
    a.insert_formatted(at, lifted.item.decorated(prefix, ""));
}

/// The indent of the array's items, without any comment lines.
fn item_indent(a: &Array) -> String {
    let first = a.get(0).and_then(|v| v.decor().prefix());
    let text = first.and_then(|p| p.as_str()).unwrap_or("\n  ");
    text.rsplit('\n').next().unwrap_or("  ").to_string()
}

/// Puts a new or moved item at `at`, in the array's own style. A `lifted` item keeps its comments.
fn insert_styled(a: &mut Array, at: usize, item: Value, lifted: Option<(String, String)>) {
    if multiline(a) {
        let (above, after) = lifted.unwrap_or_else(|| (item_indent(a), "\n".to_string()));
        land(a, at, Lifted { item, above, after });
    } else {
        a.insert(at, item);
        a.fmt();
    }
}

fn set_field(t: &mut InlineTable, key: &str, v: impl Into<Value>) {
    t.insert(key, v.into());
    t.fmt();
}

fn apply_fields(item: &mut Value, route: &str, edit: &CandidateEdit) -> Result<()> {
    let t = item
        .as_inline_table_mut()
        .ok_or_else(|| Error::Usage(format!("route {route}: a candidate is not a table")))?;
    if let Some(effort) = edit.effort {
        set_field(t, "effort", effort);
    }
    if let Some(tier) = edit.tier {
        set_field(t, "tier", tier);
    }
    if let Some(sizes) = &edit.sizes {
        let names: Array = sizes.iter().map(|s| s.as_str()).collect();
        set_field(t, "sizes", names);
    }
    Ok(())
}

/// Adds the candidate, or changes only what the edit names on the one with that target. A candidate
/// after a final `self` could never be picked, so `--at` past the end stops before it.
pub fn add_candidate(text: &str, route: &str, edit: &CandidateEdit) -> Result<String> {
    let mut doc = document(text)?;
    let list = candidates(&mut doc, route)?;
    let found = list.iter().position(|v| target_of(v) == Some(edit.target));
    if let Some(i) = found
        && edit.at.is_none_or(|at| at == i)
    {
        let item = list
            .get_mut(i)
            .ok_or_else(|| Error::Usage(format!("route {route}: a candidate is not a table")))?;
        apply_fields(item, route, edit)?;
        return Ok(doc.to_string());
    }
    let (mut item, comments) = match found {
        Some(i) if multiline(list) => {
            let lifted = lift(list, i);
            (lifted.item, Some((lifted.above, lifted.after)))
        }
        Some(i) => (list.remove(i), None),
        None => {
            let mut t = InlineTable::new();
            t.insert("target", edit.target.into());
            (Value::InlineTable(t), None)
        }
    };
    apply_fields(&mut item, route, edit)?;
    let end = match list.iter().last().and_then(target_of) {
        Some("self") if edit.target != "self" => list.len() - 1,
        _ => list.len(),
    };
    insert_styled(list, edit.at.unwrap_or(end).min(end), item, comments);
    Ok(doc.to_string())
}

pub fn remove_candidate(text: &str, route: &str, target: &str) -> Result<String> {
    let mut doc = document(text)?;
    let list = candidates(&mut doc, route)?;
    let i = list
        .iter()
        .position(|v| target_of(v) == Some(target))
        .ok_or_else(|| Error::Usage(format!("route {route} has no candidate {target:?}")))?;
    if multiline(list) {
        lift(list, i);
    } else {
        list.remove(i);
        list.fmt();
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"version = 1

# my plans
[harnesses.codex]  # keep me

[routes.r]
candidates = [
  { target = "codex/sol", effort = "high", tier = 1 },  # first choice
  { target = "self" },
]

[routes.oneline]
candidates = [{ target = "codex/sol" }]
"#;

    fn check(text: &str) -> Config {
        Config::parse(text, Path::new("t.toml")).unwrap()
    }

    fn add(target: &str) -> CandidateEdit<'_> {
        CandidateEdit {
            target,
            effort: None,
            tier: None,
            sizes: None,
            at: None,
        }
    }

    #[test]
    fn thresholds_set_and_unset_per_layer_keeping_the_rest_of_the_file() {
        let set = vec![
            "weekly_conserve=70".to_string(),
            "unknown_sizes=small".to_string(),
        ];
        let text = set_thresholds(FILE, &Scope::Harness("codex"), &set, &[]).unwrap();
        assert!(text.contains("# my plans"), "{text}");
        assert!(text.contains("# first choice"), "{text}");
        let c = check(&text);
        let l = c.limits(Some("codex"), None);
        assert_eq!(l.weekly_conserve, 70.0);
        assert_eq!(l.unknown_sizes, vec![Size::Small]);

        let unset = vec!["weekly_conserve".to_string(), "unknown_sizes".to_string()];
        let back = set_thresholds(&text, &Scope::Harness("codex"), &[], &unset).unwrap();
        assert!(!back.contains("thresholds"), "{back}");
        check(&back);
    }

    const COMMENTED: &str = r#"version = 1

[harnesses.codex]
[harnesses.muse]

[routes.r]
enabled = true # for now
candidates = [
  # primary, for most work
  { target = "codex/sol" }, # first choice
  { target = "muse/spark" }, # second choice
  # last resort
  { target = "self" }, # always there
]

[thresholds]
out = 90 # reserve for interactive work
"#;

    #[test]
    fn edits_keep_each_comment_on_the_line_it_belongs_to() {
        let set = vec!["out=80".to_string()];
        let text = set_thresholds(COMMENTED, &Scope::All, &set, &[]).unwrap();
        assert!(
            text.contains("out = 80 # reserve for interactive work\n"),
            "{text}"
        );
        let text = set_enabled(COMMENTED, "r", false).unwrap();
        assert!(text.contains("enabled = false # for now\n"), "{text}");

        let added = add_candidate(COMMENTED, "r", &add("codex/luna")).unwrap();
        assert_eq!(
            added,
            COMMENTED.replace(
                "  # last resort\n",
                "  { target = \"codex/luna\" },\n  # last resort\n"
            )
        );

        let sized = add_candidate(
            COMMENTED,
            "r",
            &CandidateEdit {
                sizes: Some(vec![Size::Small]),
                ..add("codex/sol")
            },
        )
        .unwrap();
        assert!(
            sized.contains("  # primary, for most work\n  { target = \"codex/sol\", sizes = [\"small\"] }, # first choice\n"),
            "{sized}"
        );

        let removed = remove_candidate(COMMENTED, "r", "self").unwrap();
        assert!(
            removed.contains("{ target = \"muse/spark\" }, # second choice\n]"),
            "{removed}"
        );
        assert!(
            !removed.contains("last resort") && !removed.contains("always there"),
            "{removed}"
        );

        let moved = add_candidate(
            COMMENTED,
            "r",
            &CandidateEdit {
                at: Some(1),
                ..add("codex/sol")
            },
        )
        .unwrap();
        assert!(
            moved.contains(concat!(
                "  { target = \"muse/spark\" }, # second choice\n",
                "  # primary, for most work\n",
                "  { target = \"codex/sol\" }, # first choice\n",
                "  # last resort\n",
            )),
            "{moved}"
        );
    }

    #[test]
    fn a_position_past_the_end_stops_before_a_final_self() {
        let text = add_candidate(
            COMMENTED,
            "r",
            &CandidateEdit {
                at: Some(98),
                ..add("codex/luna")
            },
        )
        .unwrap();
        let targets: Vec<String> = check(&text).routes["r"]
            .candidates
            .iter()
            .map(|c| c.target.clone())
            .collect();
        assert_eq!(targets, ["codex/sol", "muse/spark", "codex/luna", "self"]);
    }

    #[test]
    fn thresholds_reach_routes_with_dots_and_the_top_layer() {
        let file = "version = 1\n[routes.\"a.b\"]\ncandidates = [{ target = \"self\" }]\n";
        let set = vec!["out=95.5".to_string(), "conserve_sizes=".to_string()];
        let text = set_thresholds(file, &Scope::Route("a.b"), &set, &[]).unwrap();
        let c = check(&text);
        let l = c.limits(None, c.routes.get("a.b"));
        assert_eq!(l.session_out, 95.5);
        assert!(l.conserve_sizes.is_empty());
        let top = set_thresholds(file, &Scope::All, &["out=80".to_string()], &[]).unwrap();
        assert_eq!(check(&top).limits(None, None).weekly_out, 80.0);
    }

    #[test]
    fn thresholds_refuse_unknown_scopes_keys_and_values() {
        let err = |scope: &Scope, set: &str| {
            set_thresholds(FILE, scope, &[set.to_string()], &[])
                .unwrap_err()
                .to_string()
        };
        assert!(err(&Scope::Route("nope"), "out=1").contains("unknown route"));
        assert!(err(&Scope::Harness("nope"), "out=1").contains("unknown harness"));
        assert!(err(&Scope::All, "outs=1").contains("unknown threshold"));
        assert!(err(&Scope::All, "out=lots").contains("not a number"));
        assert!(err(&Scope::All, "out").contains("KEY=VALUE"));
        assert!(err(&Scope::All, "unknown_sizes=huge").contains("huge"));
    }

    fn targets(text: &str, route: &str) -> Vec<String> {
        check(text).routes[route]
            .candidates
            .iter()
            .map(|c| c.target.clone())
            .collect()
    }

    #[test]
    fn a_new_candidate_goes_before_a_final_self_or_where_asked() {
        let text = add_candidate(FILE, "r", &add("codex/luna")).unwrap();
        assert_eq!(targets(&text, "r"), ["codex/sol", "codex/luna", "self"]);
        assert!(text.contains("# first choice"), "{text}");
        let first = CandidateEdit {
            at: Some(0),
            ..add("codex/luna")
        };
        let text = add_candidate(FILE, "r", &first).unwrap();
        assert_eq!(targets(&text, "r"), ["codex/luna", "codex/sol", "self"]);
        let text = add_candidate(FILE, "oneline", &add("codex/luna")).unwrap();
        assert_eq!(targets(&text, "oneline"), ["codex/sol", "codex/luna"]);
        assert!(
            text.contains(r#"candidates = [{ target = "codex/sol" }, { target = "codex/luna" }]"#),
            "{text}"
        );
    }

    #[test]
    fn adding_an_existing_target_changes_only_what_is_named() {
        let edit = CandidateEdit {
            effort: Some("low"),
            sizes: Some(vec![Size::Small]),
            ..add("codex/sol")
        };
        let text = add_candidate(FILE, "r", &edit).unwrap();
        let c = check(&text);
        let sol = &c.routes["r"].candidates[0];
        assert_eq!(
            (sol.target.as_str(), sol.effort.as_str(), sol.tier),
            ("codex/sol", "low", Some(1))
        );
        assert_eq!(sol.sizes, Some(vec![Size::Small]));
        assert_eq!(c.routes["r"].candidates.len(), 2);
        let moved = CandidateEdit {
            at: Some(0),
            ..add("self")
        };
        assert_eq!(
            targets(&add_candidate(FILE, "r", &moved).unwrap(), "r"),
            ["self", "codex/sol"]
        );
    }

    #[test]
    fn removal_names_a_missing_target_and_a_table_list_is_rewritten() {
        let text = remove_candidate(FILE, "r", "codex/sol").unwrap();
        assert_eq!(targets(&text, "r"), ["self"]);
        assert!(
            remove_candidate(FILE, "r", "codex/luna")
                .unwrap_err()
                .to_string()
                .contains("no candidate")
        );
        let tables = "version = 1\n[harnesses.codex]\n[routes.t]\n[[routes.t.candidates]]\ntarget = \"codex/sol\"\neffort = \"high\"\n[[routes.t.candidates]]\ntarget = \"self\"\n";
        let text = add_candidate(tables, "t", &add("codex/luna")).unwrap();
        assert_eq!(targets(&text, "t"), ["codex/sol", "codex/luna", "self"]);
        assert_eq!(check(&text).routes["t"].candidates[0].effort, "high");
    }

    #[test]
    fn enabling_flips_the_key_and_refused_edits_leave_the_file() {
        assert!(!check(&set_enabled(FILE, "r", false).unwrap()).routes["r"].enabled);
        assert!(
            check(&set_enabled(&set_enabled(FILE, "r", false).unwrap(), "r", true).unwrap()).routes
                ["r"]
                .enabled
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routes.toml");
        std::fs::write(&path, FILE).unwrap();
        // The last candidate going is only caught when the result validates.
        let err = apply(&path, |t| remove_candidate(t, "oneline", "codex/sol"))
            .err()
            .unwrap();
        assert!(err.to_string().contains("no candidates"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FILE);

        let applied = apply(&path, |t| set_enabled(t, "r", false)).unwrap();
        assert_ne!(applied.old_rules, applied.new_rules);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("enabled = false")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_edit_keeps_the_file_mode_and_a_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("routes.toml");
        std::fs::write(&real, FILE).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&real, &link).unwrap();
        apply(&link, |t| set_enabled(t, "r", false)).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .contains("enabled = false")
        );
    }

    #[test]
    fn a_file_on_org_routes_refuses_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routes.toml");
        let text = "version = 1\n[sync]\norg = \"acme\"\nroutes = \"org\"\n";
        std::fs::write(&path, text).unwrap();
        let err = apply(&path, |t| Ok(t.to_string())).err().unwrap();
        assert!(err.to_string().contains("shared routes of acme"), "{err}");
    }
}
