//! Static surface extraction from JavaScript and TypeScript sources with
//! tree-sitter. Nothing is evaluated: tools come from call sites whose callee
//! ends in `.tool`, `.registerTool` or `.addTool`, and object literals with
//! `name` and `inputSchema`. Same-file constants resolve without evaluation.
//! Egress comes from `http(s)://` string literals, and capabilities from
//! module specifiers, `fetch(` calls, calls on fs bindings and `process.env`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::{Node, Parser};

use super::{Capability, Tool};

#[derive(Debug, Default)]
pub(crate) struct Extraction {
    /// Sorted by name, preferring the first occurrence with a description.
    pub tools: Vec<Tool>,
    pub egress: BTreeSet<String>,
    pub capabilities: BTreeSet<Capability>,
}

const TOOL_METHODS: [&str; 3] = ["tool", "registerTool", "addTool"];
const FS_WRITE_NAMES: [&str; 10] = [
    "writeFile",
    "writeFileSync",
    "appendFile",
    "appendFileSync",
    "createWriteStream",
    "createWriteStreamSync",
    "rm",
    "rmSync",
    "unlink",
    "unlinkSync",
];
const EXCLUDED_HOSTS: [&str; 5] = [
    "localhost",
    "127.0.0.1",
    "::1",
    "json-schema.org",
    "www.w3.org",
];

static URL_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://([^/\s?#\x00]+)").expect("regex"));
static HOSTNAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+(?:[a-z][a-z0-9-]*[a-z0-9]|\d+)$")
        .expect("regex")
});

/// The design's scanned files: JS and TS sources outside `node_modules/`, minus `.d.ts`.
pub(crate) fn is_scanned(path: &str) -> bool {
    if path.split('/').any(|c| c == "node_modules") || path.ends_with(".d.ts") {
        return false;
    }
    [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

pub(crate) fn extract(files: &[(String, String)]) -> Extraction {
    let mut js = Parser::new();
    js.set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("javascript grammar matches tree-sitter");
    let mut ts = Parser::new();
    ts.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("typescript grammar matches tree-sitter");
    let mut out = Extraction::default();
    for (path, text) in files {
        let typescript = [".ts", ".mts", ".cts"].iter().any(|e| path.ends_with(e));
        let parser = if typescript { &mut ts } else { &mut js };
        if let Some(tree) = parser.parse(text, None) {
            let bindings = Bindings::collect(tree.root_node(), text.as_bytes());
            walk(tree.root_node(), |node| {
                inspect(node, text.as_bytes(), &bindings, &mut out)
            });
        }
    }
    let mut tools: BTreeMap<String, Tool> = BTreeMap::new();
    for tool in out.tools {
        match tools.get_mut(&tool.name) {
            Some(first) if first.description.is_none() && tool.description.is_some() => {
                *first = tool
            }
            Some(_) => {}
            None => {
                tools.insert(tool.name.clone(), tool);
            }
        }
    }
    out.tools = tools.into_values().collect();
    out
}

/// Depth-first walk without recursion; minified bundles nest deeply.
fn walk<'t>(root: Node<'t>, mut inspect: impl FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    loop {
        inspect(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Bindings are indexed by lexical scope so repeated local names do not
/// resolve to another function's constants. Non-const bindings block lookup.
#[derive(Default)]
struct Bindings<'t> {
    values: BTreeMap<(usize, String), Option<Node<'t>>>,
    fs: BTreeMap<(usize, String), bool>,
}

fn is_scope(node: Node) -> bool {
    matches!(
        node.kind(),
        "program"
            | "statement_block"
            | "arrow_function"
            | "function_declaration"
            | "function_expression"
            | "generator_function"
            | "generator_function_declaration"
            | "method_definition"
            | "catch_clause"
            | "for_statement"
            | "for_in_statement"
    )
}

fn scope(mut node: Node) -> Node {
    while !is_scope(node) {
        let Some(parent) = node.parent() else { break };
        node = parent;
    }
    node
}

impl<'t> Bindings<'t> {
    fn collect(root: Node<'t>, src: &[u8]) -> Self {
        let mut bindings = Self::default();
        walk(root, |node| {
            if node.kind() == "variable_declarator" {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                let declaration = node.parent().expect("declarator has a declaration");
                let constant = declaration
                    .child_by_field_name("kind")
                    .is_some_and(|kind| text(kind, src) == "const");
                let mut owner = scope(declaration);
                if declaration.kind() == "variable_declaration" {
                    // `var` shadows for the whole function, including outside
                    // the block that contains its declaration.
                    while matches!(
                        owner.kind(),
                        "statement_block" | "for_statement" | "for_in_statement" | "catch_clause"
                    ) {
                        let Some(parent) = owner.parent() else { break };
                        owner = scope(parent);
                    }
                }
                bindings.bind_pattern(
                    name,
                    owner,
                    constant
                        .then(|| node.child_by_field_name("value"))
                        .flatten(),
                    src,
                );
            }
            if matches!(
                node.kind(),
                "function_declaration" | "generator_function_declaration" | "class_declaration"
            ) && let (Some(name), Some(parent)) =
                (node.child_by_field_name("name"), node.parent())
            {
                bindings.bind_pattern(name, scope(parent), None, src);
            }
            if matches!(node.kind(), "function_expression" | "generator_function")
                && let Some(name) = node.child_by_field_name("name")
            {
                bindings.bind_pattern(name, node, None, src);
            }
            if is_scope(node)
                && let Some(params) = node
                    .child_by_field_name("parameters")
                    .or_else(|| node.child_by_field_name("parameter"))
            {
                bindings.bind_pattern(params, node, None, src);
            }
        });
        walk(root, |node| {
            if node.kind() == "import_statement" {
                let fs_module = node
                    .child_by_field_name("source")
                    .and_then(|n| literal(n, src))
                    .is_some_and(|s| is_fs_module(&s));
                walk(node, |part| {
                    let binding = match part.kind() {
                        "import_specifier" => part
                            .child_by_field_name("alias")
                            .or_else(|| part.child_by_field_name("name")),
                        "namespace_import" => part.named_child(0),
                        "import_clause" => part.named_child(0).filter(|n| n.kind() == "identifier"),
                        _ => None,
                    };
                    if let Some(binding) = binding {
                        let key = (scope(node).id(), text(binding, src).to_string());
                        bindings.values.insert(key.clone(), None);
                        let named = part.kind() == "import_specifier";
                        if fs_module
                            && (!named
                                || part
                                    .child_by_field_name("name")
                                    .is_some_and(|n| FS_WRITE_NAMES.contains(&text(n, src))))
                        {
                            bindings.fs.insert(key, named);
                        }
                    }
                });
            }
            if node.kind() == "variable_declarator"
                && let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                )
                && value.kind() == "call_expression"
                && value
                    .child_by_field_name("function")
                    .is_some_and(|n| text(n, src) == "require")
                && value
                    .child_by_field_name("arguments")
                    .and_then(|n| n.named_child(0))
                    .and_then(|n| literal(n, src))
                    .is_some_and(|s| is_fs_module(&s))
            {
                let owner = scope(node);
                if name.kind() == "identifier" {
                    bindings
                        .fs
                        .insert((owner.id(), text(name, src).to_string()), false);
                } else if name.kind() == "object_pattern" {
                    let mut cursor = name.walk();
                    for part in name.named_children(&mut cursor) {
                        let (original, local) = if part.kind() == "pair_pattern" {
                            (
                                part.child_by_field_name("key"),
                                part.child_by_field_name("value"),
                            )
                        } else {
                            (Some(part), Some(part))
                        };
                        if let (Some(original), Some(local)) = (original, local)
                            && FS_WRITE_NAMES.contains(&text(original, src))
                        {
                            bindings
                                .fs
                                .insert((owner.id(), text(local, src).to_string()), true);
                        }
                    }
                }
            }
        });
        bindings
    }

    fn fs_binding(&self, node: Node, src: &[u8]) -> Option<bool> {
        if node.kind() != "identifier" {
            return None;
        }
        let name = text(node, src).to_string();
        let mut ancestor = Some(node);
        while let Some(parent) = ancestor {
            let key = (parent.id(), name.clone());
            if is_scope(parent) {
                if let Some(named) = self.fs.get(&key) {
                    return Some(*named);
                }
                if self.values.contains_key(&key) {
                    return None;
                }
            }
            ancestor = parent.parent();
        }
        None
    }

    fn bind_pattern(
        &mut self,
        pattern: Node<'t>,
        scope: Node,
        value: Option<Node<'t>>,
        src: &[u8],
    ) {
        if pattern.kind() == "identifier" {
            let key = (scope.id(), text(pattern, src).to_string());
            // Ambiguous declarations are unresolved rather than guessed.
            self.values
                .entry(key)
                .and_modify(|v| *v = None)
                .or_insert(value);
        } else {
            walk(pattern, |node| {
                if matches!(
                    node.kind(),
                    "identifier" | "shorthand_property_identifier_pattern"
                ) {
                    self.values
                        .insert((scope.id(), text(node, src).to_string()), None);
                }
            });
        }
    }

    fn initializer(&self, node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
        if !matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
            return Some(node);
        }
        let name = text(node, src).to_string();
        let mut ancestor = Some(node);
        while let Some(parent) = ancestor {
            if is_scope(parent)
                && let Some(value) = self.values.get(&(parent.id(), name.clone()))
            {
                return *value;
            }
            ancestor = parent.parent();
        }
        None
    }

    fn value(&self, node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
        let value = self.initializer(node, src)?;
        if !matches!(node.kind(), "identifier" | "shorthand_property_identifier")
            || matches!(value.kind(), "string" | "template_string" | "object")
        {
            Some(value)
        } else {
            None
        }
    }

    fn string(&self, node: Node<'t>, src: &[u8]) -> Option<String> {
        literal(self.value(node, src)?, src)
    }

    fn schema(&self, node: Node<'t>, src: &[u8]) -> Option<String> {
        self.initializer(node, src)
            .map(|value| text(value, src).to_string())
    }
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or_default()
}

fn inspect<'t>(node: Node<'t>, src: &[u8], bindings: &Bindings<'t>, out: &mut Extraction) {
    match node.kind() {
        "call_expression" => call(node, src, bindings, out),
        "object" => {
            let fields = pairs(node, src);
            let name = fields
                .iter()
                .find(|(key, _)| key == "name")
                .and_then(|(_, value)| bindings.string(*value, src));
            let schema = fields.iter().find(|(key, _)| key == "inputSchema");
            if let (Some(name), Some((_, schema))) = (name, schema) {
                let description = fields
                    .iter()
                    .find(|(key, _)| key == "description")
                    .and_then(|(_, value)| bindings.string(*value, src));
                out.tools.push(Tool {
                    name,
                    description,
                    schema: bindings.schema(*schema, src),
                });
            }
        }
        "import_statement" | "export_statement" => {
            if let Some(spec) = node
                .child_by_field_name("source")
                .and_then(|s| literal(s, src))
            {
                module(&spec, out);
            }
        }
        "member_expression" => {
            let object = node.child_by_field_name("object");
            let property = node.child_by_field_name("property");
            if let (Some(o), Some(p)) = (object, property)
                && text(o, src) == "process"
                && text(p, src) == "env"
            {
                out.capabilities.insert(Capability::Env);
            }
        }
        "string" => {
            if let Some(value) = literal(node, src) {
                egress(&value, out);
            }
        }
        "template_string" => egress(&template_text(node, src), out),
        _ => {}
    }
}

fn call<'t>(node: Node<'t>, src: &[u8], bindings: &Bindings<'t>, out: &mut Extraction) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    let fs_write = if function.kind() == "identifier" {
        bindings.fs_binding(function, src) == Some(true)
    } else if function.kind() == "member_expression" {
        function
            .child_by_field_name("object")
            .is_some_and(|o| bindings.fs_binding(o, src) == Some(false))
            && function
                .child_by_field_name("property")
                .is_some_and(|p| FS_WRITE_NAMES.contains(&text(p, src)))
    } else {
        false
    };
    if fs_write {
        out.capabilities.insert(Capability::FsWrite);
    }
    let args: Vec<Node> = node
        .child_by_field_name("arguments")
        .map(|a| {
            let mut cursor = a.walk();
            a.named_children(&mut cursor)
                .filter(|n| n.kind() != "comment")
                .collect()
        })
        .unwrap_or_default();
    match function.kind() {
        "identifier" => match text(function, src) {
            "fetch" => {
                out.capabilities.insert(Capability::Network);
            }
            "require" => {
                if let Some(spec) = args.first().and_then(|a| literal(*a, src)) {
                    module(&spec, out);
                }
            }
            _ => {}
        },
        "import" => {
            if let Some(spec) = args.first().and_then(|a| literal(*a, src)) {
                module(&spec, out);
            }
        }
        "member_expression" => {
            let Some(property) = function.child_by_field_name("property") else {
                return;
            };
            let method = text(property, src);
            if method == "fetch"
                && function
                    .child_by_field_name("object")
                    .is_some_and(|o| matches!(text(o, src), "globalThis" | "window" | "self"))
            {
                out.capabilities.insert(Capability::Network);
            }
            if TOOL_METHODS.contains(&method)
                && let Some(tool) = tool_call(&args, src, bindings)
            {
                out.tools.push(tool);
            }
        }
        _ => {}
    }
}

/// Reads `(name, description?, schema?, ...)` or `(name, {description, inputSchema}, ...)`.
/// The schema is the raw source of an `inputSchema` property, else of the
/// first later argument that is neither the description nor a function.
fn tool_call<'t>(args: &[Node<'t>], src: &[u8], bindings: &Bindings<'t>) -> Option<Tool> {
    let name = bindings.string(*args.first()?, src)?;
    let mut description = None;
    let mut rest = 1;
    if let Some(d) = args.get(1).and_then(|a| bindings.string(*a, src)) {
        description = Some(d);
        rest = 2;
    }
    let mut schema = None;
    let mut config_arg = None;
    for (index, arg) in args.iter().enumerate().skip(1) {
        let Some(object) = bindings.value(*arg, src).filter(|n| n.kind() == "object") else {
            continue;
        };
        for (key, value) in pairs(object, src) {
            if key == "description" && description.is_none() {
                if let Some(d) = bindings.string(value, src) {
                    description = Some(d);
                    config_arg = Some(index);
                }
            } else if key == "inputSchema" && schema.is_none() {
                schema = bindings.schema(value, src);
                config_arg = Some(index);
            }
        }
    }
    if schema.is_none() {
        schema = args
            .iter()
            .enumerate()
            .skip(rest)
            .find(|(index, arg)| Some(*index) != config_arg && !is_function(**arg))
            .and_then(|(_, arg)| bindings.schema(*arg, src));
    }
    Some(Tool {
        name,
        description,
        schema,
    })
}

fn is_function(node: Node) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "function" | "generator_function"
    )
}

fn pairs<'t>(object: Node<'t>, src: &[u8]) -> Vec<(String, Node<'t>)> {
    let mut cursor = object.walk();
    object
        .named_children(&mut cursor)
        .filter_map(|pair| {
            if pair.kind() == "shorthand_property_identifier" {
                return Some((text(pair, src).to_string(), pair));
            }
            if pair.kind() != "pair" {
                return None;
            }
            let key = pair.child_by_field_name("key")?;
            let value = pair.child_by_field_name("value")?;
            let key = match key.kind() {
                "property_identifier" => text(key, src).to_string(),
                _ => literal(key, src)?,
            };
            Some((key, value))
        })
        .collect()
}

/// The value of a string literal, a template literal without substitutions,
/// or a `+` concatenation of those.
fn literal(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "string" => Some(fragments(node, src)),
        "template_string" => {
            let mut cursor = node.walk();
            let substituted = node
                .named_children(&mut cursor)
                .any(|c| c.kind() == "template_substitution");
            (!substituted).then(|| fragments(node, src))
        }
        "parenthesized_expression" => literal(node.named_child(0)?, src),
        "binary_expression" => {
            let operator = node.child_by_field_name("operator")?;
            if text(operator, src) != "+" {
                return None;
            }
            let left = literal(node.child_by_field_name("left")?, src)?;
            let right = literal(node.child_by_field_name("right")?, src)?;
            Some(left + &right)
        }
        _ => None,
    }
}

fn fragments(node: Node, src: &[u8]) -> String {
    let mut value = String::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "string_fragment" => value.push_str(text(child, src)),
            "escape_sequence" => value.push_str(&unescape(text(child, src))),
            _ => {}
        }
    }
    value
}

/// Template text with each substitution replaced by NUL, which ends a hostname.
fn template_text(node: Node, src: &[u8]) -> String {
    let mut value = String::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "string_fragment" => value.push_str(text(child, src)),
            "escape_sequence" => value.push_str(&unescape(text(child, src))),
            "template_substitution" => value.push('\0'),
            _ => {}
        }
    }
    value
}

/// Decodes one JavaScript escape sequence such as `\n`, `\x41`, `A` or `\u{1F600}`.
fn unescape(seq: &str) -> String {
    let body = &seq[1.min(seq.len())..];
    let from_hex = |h: &str| u32::from_str_radix(h, 16).ok().and_then(char::from_u32);
    let decoded = match body.chars().next() {
        Some('n') => Some('\n'),
        Some('r') => Some('\r'),
        Some('t') => Some('\t'),
        Some('b') => Some('\u{8}'),
        Some('f') => Some('\u{c}'),
        Some('v') => Some('\u{b}'),
        Some('0') if body.len() == 1 => Some('\0'),
        Some('x') => from_hex(&body[1..]),
        Some('u') => from_hex(body[1..].trim_start_matches('{').trim_end_matches('}')),
        Some('\n' | '\r' | '\u{2028}' | '\u{2029}') => return String::new(),
        Some(c) => Some(c),
        None => None,
    };
    decoded.map(String::from).unwrap_or_else(|| seq.to_string())
}

fn is_fs_module(spec: &str) -> bool {
    matches!(
        spec.strip_prefix("node:").unwrap_or(spec),
        "fs" | "fs/promises"
    )
}

fn module(spec: &str, out: &mut Extraction) {
    let bare = spec.strip_prefix("node:").unwrap_or(spec);
    let root = bare.split('/').next().unwrap_or(bare);
    let capability = match root {
        "child_process" | "execa" => Capability::ChildProcess,
        "http" | "https" | "net" | "undici" | "axios" => Capability::Network,
        _ => return,
    };
    out.capabilities.insert(capability);
}

fn authority_host(authority: &str) -> Option<&str> {
    let authority = authority.rsplit('@').next()?;
    if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, _) = ipv6.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        Some(host)
    } else {
        authority.split(':').next()
    }
}

fn egress(value: &str, out: &mut Extraction) {
    for capture in URL_HOST.captures_iter(value) {
        let Some(host) = authority_host(&capture[1]) else {
            continue;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if (HOSTNAME.is_match(&host) || host.parse::<std::net::IpAddr>().is_ok())
            && !excluded_host(&host)
        {
            out.egress.insert(host);
        }
    }
}

fn excluded_host(host: &str) -> bool {
    EXCLUDED_HOSTS.contains(&host)
        || host.ends_with(".localhost")
        || ["example.com", "example.org"]
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, code: &str) -> Extraction {
        extract(&[(path.to_string(), code.to_string())])
    }

    #[test]
    fn mcp_sdk_tool_shapes() {
        let out = run(
            "index.js",
            r#"
            server.tool("echo", "Echo " + 'it', { text: z.string() }, async () => ({}));
            server.registerTool("add", { title: "Add", description: `Add two`, inputSchema: { a: z.number() } }, cb);
            srv?.addTool('ping', async () => 1);
            server.tool(name, "dynamic names are skipped");
            "#,
        );
        let names: Vec<_> = out.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["add", "echo", "ping"]);
        assert_eq!(out.tools[1].description.as_deref(), Some("Echo it"));
        assert_eq!(out.tools[1].schema.as_deref(), Some("{ text: z.string() }"));
        assert_eq!(out.tools[0].description.as_deref(), Some("Add two"));
        assert_eq!(out.tools[0].schema.as_deref(), Some("{ a: z.number() }"));
        assert_eq!(out.tools[2].description, None);
        assert_eq!(out.tools[2].schema, None);
    }

    #[test]
    fn typescript_sources_parse() {
        let out = run(
            "src/x.ts",
            "const s: McpServer = make();\ns.tool(\"t\", \"d\", { q: z.string() as ZodString }, async (a: Args) => a);",
        );
        assert_eq!(
            out.tools[0].schema.as_deref(),
            Some("{ q: z.string() as ZodString }")
        );
    }

    #[test]
    fn egress_and_capabilities() {
        let out = run(
            "a.mjs",
            r#"
            import { execFile } from "node:child_process";
            import { writeFile } from "fs/promises";
            const u = "https://API.GitHub.com/repos";
            const t = `https://hooks.slack.com/${path}`;
            const skipped = [`https://${host}/x`, "http://localhost:3000", "https://json-schema.org/draft",
              "https://docs.example.com/x", "https://www.w3.org/2000/svg"];
            // https://in.a.comment.dev is not a string literal
            const key = process.env.TOKEN;
            await fetch(u);
            await writeFile("x", "y");
            "#,
        );
        assert_eq!(
            out.egress.iter().collect::<Vec<_>>(),
            ["api.github.com", "hooks.slack.com"]
        );
        assert_eq!(
            out.capabilities.iter().collect::<Vec<_>>(),
            [
                &Capability::ChildProcess,
                &Capability::Env,
                &Capability::FsWrite,
                &Capability::Network
            ]
        );
    }

    #[test]
    fn require_and_member_writes() {
        let out = run(
            "a.cjs",
            "const h = require('https'); const fs = require('fs'); fs.unlinkSync(p);",
        );
        assert!(out.capabilities.contains(&Capability::Network));
        assert!(out.capabilities.contains(&Capability::FsWrite));
    }

    #[test]
    fn fs_writes_require_calls_on_fs_bindings() {
        for code in [
            "const schema = {properties: {rm: {type: 'string'}}}; const x = {unlink: 1};",
            "const fs = {rm() {}}; fs.rm();",
            "import {rm} from 'other'; rm();",
            "import {rm} from 'fs'; const x = {rm};",
            "import fs from 'fs'; function f(fs) { fs.rm(); }",
        ] {
            assert!(
                !run("a.js", code)
                    .capabilities
                    .contains(&Capability::FsWrite),
                "{code}"
            );
        }
        for code in [
            "import fs from 'fs'; fs.writeFileSync('x','y');",
            "import {writeFile} from 'node:fs/promises'; writeFile('x','y');",
            "const f = require('fs'); f.rmSync('x');",
            "import * as f from 'node:fs'; f.unlink('x');",
            "import {appendFile as append} from 'fs'; append('x','y');",
            "const {rm: remove} = require('node:fs/promises'); remove('x');",
        ] {
            assert!(
                run("a.js", code)
                    .capabilities
                    .contains(&Capability::FsWrite),
                "{code}"
            );
        }
    }

    #[test]
    fn egress_parses_userinfo_ports_ipv6_and_exact_loopback() {
        assert_eq!(
            authority_host("user:pass@API.host.net:443"),
            Some("API.host.net")
        );
        let out = run(
            "a.js",
            r#"[
            "https://API.host.net:443/path",
            "http://127.evil.net/path", "http://127.0.0.2/",
            "http://127.0.0.1/", "http://[::1]:80/",
            "https://[2001:db8::2]:443/x"
        ]"#,
        );
        assert_eq!(
            out.egress.into_iter().collect::<Vec<_>>(),
            ["127.0.0.2", "127.evil.net", "2001:db8::2", "api.host.net"]
        );
    }

    #[test]
    fn scanned_file_filter() {
        assert!(is_scanned("dist/index.js"));
        assert!(is_scanned("src/a.cts"));
        assert!(!is_scanned("dist/index.d.ts"));
        assert!(!is_scanned("node_modules/x/index.js"));
        assert!(!is_scanned("README.md"));
    }

    #[test]
    fn escapes_decode() {
        assert_eq!(unescape(r"\n"), "\n");
        assert_eq!(unescape(r"\x41"), "A");
        assert_eq!(unescape(r"A"), "A");
        assert_eq!(unescape(r"\u{1F600}"), "\u{1F600}");
        assert_eq!(unescape(r#"\""#), "\"");
    }
}
