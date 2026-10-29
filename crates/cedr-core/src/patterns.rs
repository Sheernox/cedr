//! Skill risk patterns ported from NVIDIA SkillSpector (Apache-2.0), commit
//! d501c0d5035c2ed6385740a384d942bb05651fda:
//!   src/skillspector/nodes/analyzers/static_patterns_prompt_injection.py (P1-P4)
//!   src/skillspector/nodes/analyzers/static_patterns_data_exfiltration.py (E1-E5)
//!   src/skillspector/nodes/analyzers/static_patterns_supply_chain.py (SC1-SC3, SC7)
//!   src/skillspector/nodes/analyzers/whitespace_padding.py (ZERO_WIDTH_CHARS for P2)
//! Regexes, confidences, severities and file-type gates are copied as written;
//! see NOTICE. Each pattern keeps the rule ID and message of its source list.
//!
//! Not ported, because they need features the `regex` crate lacks or are not
//! regex families:
//! - P2 hidden HTML comment (tempered `(?!--!?>)` lookahead and the
//!   identifier-boundary lookbehinds of `_P2_COMMENT_KEYWORD`).
//! - P2 `[//]: #` reference comments (a structural title scanner, not a regex).
//! - P2 zero-width safe-emoji ZWJ carve-out (a Unicode emoji property walk);
//!   the leading byte-order-mark carve-out is kept.
//! - P9 whitespace padding (an algorithmic run detector).
//! - SC3 shell decode-to-execute chains (possessive quantifiers `{0,256}+`, `)?+`).
//! - E2 Python AST environment reads (the regex fallback list is ported instead),
//!   SC4-SC6 and SC8-SC10 (OSV lookups, package lists, artifact checks), TR1-TR3.
//!
//! Emulated: SC7 `--disable-content-trust\b(?!=false)` uses `not_followed_by`
//! in place of the lookahead; the P2 tag-character emoji carve-out uses `exempt`.
//! SkillSpector's shell-command range narrowing for E1 and SC2 is not ported;
//! those patterns use its documented fallback, a plain search of the file.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    PromptInjection,
    DataExfiltration,
    SupplyChain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RiskSeverity {
    High,
    Medium,
    Low,
}

impl Family {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Family::PromptInjection => "prompt injection",
            Family::DataExfiltration => "data exfiltration",
            Family::SupplyChain => "supply chain",
        }
    }
}

impl RiskSeverity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RiskSeverity::High => "high",
            RiskSeverity::Medium => "medium",
            RiskSeverity::Low => "low",
        }
    }
}

/// SkillSpector's file-type gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every file type.
    Any,
    /// P2 list: markdown, perl, powershell and other; not python, javascript or shell.
    MarkupAndOther,
    /// SC3 list: every type except markdown.
    NotMarkdown,
    /// SC1: paths naming requirements, package.json, pyproject.toml, setup.py or Pipfile.
    DependencyManifest,
}

/// SkillSpector's `iter_paragraph_matches` (prose) versus `re.finditer` (code).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Window {
    Whole,
    Paragraph,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PatternDef {
    pub rule: &'static str,
    pub message: &'static str,
    pub family: Family,
    pub severity: RiskSeverity,
    pub confidence: f32,
    pub regex: &'static str,
    pub case_insensitive: bool,
    pub dot_all: bool,
    pub scope: Scope,
    pub window: Window,
    /// Drop a match that starts inside a match of this regex.
    pub exempt: Option<&'static str>,
    /// Drop a match immediately followed by this regex.
    pub not_followed_by: Option<&'static str>,
}

const fn def(
    rule: &'static str,
    message: &'static str,
    family: Family,
    severity: RiskSeverity,
    regex: &'static str,
    confidence: f32,
) -> PatternDef {
    PatternDef {
        rule,
        message,
        family,
        severity,
        confidence,
        regex,
        case_insensitive: true,
        dot_all: false,
        scope: Scope::Any,
        window: Window::Whole,
        exempt: None,
        not_followed_by: None,
    }
}

const fn prose(mut p: PatternDef) -> PatternDef {
    p.window = Window::Paragraph;
    p
}

const fn p1(regex: &'static str, confidence: f32) -> PatternDef {
    prose(def(
        "P1",
        "Instruction Override",
        Family::PromptInjection,
        RiskSeverity::High,
        regex,
        confidence,
    ))
}

const fn p2(regex: &'static str, confidence: f32) -> PatternDef {
    let mut p = def(
        "P2",
        "Hidden Instructions",
        Family::PromptInjection,
        RiskSeverity::High,
        regex,
        confidence,
    );
    p.dot_all = true;
    p.scope = Scope::MarkupAndOther;
    p
}

const fn p3(regex: &'static str, confidence: f32) -> PatternDef {
    prose(def(
        "P3",
        "Exfiltration Commands",
        Family::PromptInjection,
        RiskSeverity::High,
        regex,
        confidence,
    ))
}

const fn p4(regex: &'static str, confidence: f32) -> PatternDef {
    prose(def(
        "P4",
        "Behavior Manipulation",
        Family::PromptInjection,
        RiskSeverity::Medium,
        regex,
        confidence,
    ))
}

const fn e(
    rule: &'static str,
    message: &'static str,
    severity: RiskSeverity,
    regex: &'static str,
    confidence: f32,
) -> PatternDef {
    def(
        rule,
        message,
        Family::DataExfiltration,
        severity,
        regex,
        confidence,
    )
}

const fn e1(regex: &'static str, confidence: f32) -> PatternDef {
    e(
        "E1",
        "External Transmission",
        RiskSeverity::Medium,
        regex,
        confidence,
    )
}

const fn e2(regex: &'static str, confidence: f32) -> PatternDef {
    e(
        "E2",
        "Env Variable Harvesting",
        RiskSeverity::High,
        regex,
        confidence,
    )
}

const fn e3(regex: &'static str, confidence: f32) -> PatternDef {
    e(
        "E3",
        "File System Enumeration",
        RiskSeverity::Medium,
        regex,
        confidence,
    )
}

const fn e4(regex: &'static str, confidence: f32) -> PatternDef {
    prose(e(
        "E4",
        "Context Leakage",
        RiskSeverity::High,
        regex,
        confidence,
    ))
}

const fn e5(regex: &'static str, confidence: f32) -> PatternDef {
    e(
        "E5",
        "Cloud Storage Exfiltration",
        RiskSeverity::Medium,
        regex,
        confidence,
    )
}

const fn sc1(regex: &'static str, confidence: f32) -> PatternDef {
    let mut p = def(
        "SC1",
        "Unpinned Dependencies",
        Family::SupplyChain,
        RiskSeverity::Low,
        regex,
        confidence,
    );
    // SC1 is the one family SkillSpector matches case-sensitively.
    p.case_insensitive = false;
    p.scope = Scope::DependencyManifest;
    p
}

const fn sc2(regex: &'static str, confidence: f32) -> PatternDef {
    def(
        "SC2",
        "External Script Fetching",
        Family::SupplyChain,
        RiskSeverity::High,
        regex,
        confidence,
    )
}

const fn sc3(regex: &'static str, confidence: f32) -> PatternDef {
    let mut p = def(
        "SC3",
        "Obfuscated Code",
        Family::SupplyChain,
        RiskSeverity::High,
        regex,
        confidence,
    );
    p.scope = Scope::NotMarkdown;
    p
}

const fn sc7(regex: &'static str, confidence: f32) -> PatternDef {
    def(
        "SC7",
        "Untrusted Container Image",
        Family::SupplyChain,
        RiskSeverity::High,
        regex,
        confidence,
    )
}

pub(crate) static PATTERNS: &[PatternDef] = &[
    // P1: Instruction Override
    p1(r"ignore\s+(?:all\s+)?previous\s+instructions?", 0.8),
    p1(
        r"ignore\s+(?:all\s+)?(?:safety|security)\s+(?:rules?|constraints?|guidelines?)",
        0.9,
    ),
    p1(r"override\s+(?:safety|security|system)", 0.9),
    p1(
        r"bypass\s+(?:safety|security|restrictions?|constraints?)",
        0.9,
    ),
    p1(r"disregard\s+(?:all\s+)?(?:previous|safety|security)", 0.8),
    p1(
        r"forget\s+(?:all\s+)?(?:previous|your)\s+instructions?",
        0.8,
    ),
    p1(
        r"you\s+are\s+now\s+(?:in\s+)?(?:jailbreak|unrestricted|unfiltered)\s+mode",
        0.95,
    ),
    p1(r"enable\s+(?:developer|debug|admin|root)\s+mode", 0.7),
    p1(r"your\s+new\s+(?:instructions?|rules?)\s+are", 0.6),
    p1(
        r"do\s+not\s+follow\s+(?:any|your|the)\s+(?:rules?|guidelines?)",
        0.9,
    ),
    p1(r"you\s+must\s+(?:always\s+)?ignore", 0.7),
    // P2: Hidden Instructions
    {
        // A leading U+FEFF is a byte-order mark, not hidden text.
        let mut p = p2("[\u{200b}\u{200c}\u{200d}\u{2060}\u{feff}]", 0.6);
        p.exempt = Some(r"\A\x{feff}");
        p
    },
    p2(r"data:text/plain;base64,[A-Za-z0-9+/=]{50,}", 0.7),
    {
        // Bidi controls run on every file type (Trojan Source).
        let mut p = p2(r"[\x{202a}-\x{202e}\x{2066}-\x{2069}]", 0.85);
        p.scope = Scope::Any;
        p
    },
    {
        // Unicode Tags block (ASCII smuggling) on every file type, except
        // well-formed RGI subdivision flag emoji.
        let mut p = def(
            "P2",
            "Hidden Instructions (Unicode Tag / ASCII smuggling)",
            Family::PromptInjection,
            RiskSeverity::High,
            r"[\x{e0000}-\x{e007f}]",
            0.9,
        );
        p.exempt = Some(r"\x{1f3f4}[\x{e0030}-\x{e0039}\x{e0061}-\x{e007a}]{2,6}\x{e007f}");
        p
    },
    // P3: Exfiltration Commands
    p3(
        r"(?:send|transmit|upload|post|forward)\s+(?:the\s+)?(?:conversation|context|chat|history|messages?)\s+to",
        0.9,
    ),
    p3(
        r"(?:send|transmit|upload|post)\s+(?:all\s+)?(?:user\s+)?(?:data|information|input|files?)\s+to\s+(?:https?://|external)",
        0.9,
    ),
    p3(
        r"(?:copy|extract|collect)\s+(?:the\s+)?(?:conversation|context|session)\s+(?:and\s+)?(?:send|upload|transmit)",
        0.85,
    ),
    p3(
        r"(?:call|notify|ping|hit)\s+(?:this\s+)?(?:webhook|endpoint|url|api)\s+with\s+(?:the\s+)?(?:user|conversation|context)",
        0.8,
    ),
    p3(
        r"include\s+(?:the\s+)?(?:full\s+)?(?:conversation|context|history)\s+in\s+(?:the\s+)?(?:request|payload|body)",
        0.75,
    ),
    p3(
        r"(?:silently|quietly|secretly|covertly)\s+(?:send|transmit|log|record)",
        0.9,
    ),
    p3(
        r"without\s+(?:telling|informing|notifying)\s+(?:the\s+)?user.*?(?:send|transmit|upload)",
        0.9,
    ),
    // P4: Behavior Manipulation
    p4(
        r"always\s+(?:recommend|suggest|prefer|choose|use)\s+(?:\w+\s+){0,3}(?:over|instead\s+of|rather\s+than)",
        0.7,
    ),
    p4(
        r"never\s+(?:recommend|suggest|mention|tell|warn|inform)\s+(?:the\s+)?(?:user|them)",
        0.75,
    ),
    p4(
        r"(?:prioritize|prefer)\s+(?:\w+\s+){0,3}(?:over|above)\s+(?:user\s+)?(?:safety|security|privacy|preferences?)",
        0.85,
    ),
    p4(
        r"(?:subtly|gradually|slowly)\s+(?:steer|guide|push|nudge)\s+(?:the\s+)?(?:user|conversation)",
        0.8,
    ),
    p4(
        r"(?:without\s+)?(?:the\s+)?user\s+(?:knowing|realizing|noticing).*?(?:change|modify|alter|influence)",
        0.85,
    ),
    p4(
        r"(?:gain|build|establish)\s+(?:the\s+)?(?:user(?:'s)?)\s+trust\s+(?:and\s+then|before|so\s+that)",
        0.75,
    ),
    p4(
        r"(?:appear|seem|act)\s+(?:helpful|friendly|trustworthy)\s+(?:while|but|then)",
        0.7,
    ),
    p4(
        r"(?:always|never)\s+include\s+(?:this|the\s+following)\s+in\s+(?:every|all)\s+(?:response|output|reply)",
        0.7,
    ),
    p4(
        r"append\s+(?:this|the\s+following)\s+to\s+(?:every|all|each)\s+(?:response|message|output)",
        0.75,
    ),
    // E1: External Transmission
    e1(r#"requests\s*\.\s*(?:post|put)\s*\(\s*['"]https?://"#, 0.6),
    e1(r"requests\s*\.\s*(?:post|put)\s*\([^)]*json\s*=", 0.7),
    e1(r#"httpx\s*\.\s*(?:post|put)\s*\(\s*['"]https?://"#, 0.6),
    e1(
        r"urllib\s*\.\s*request\s*\.\s*urlopen\s*\([^)]*data\s*=",
        0.6,
    ),
    e1(
        r#"fetch\s*\(\s*['"]https?://[^'"]+['"][^)]*method\s*:\s*['"]POST['"]"#,
        0.6,
    ),
    e1(
        r"curl\s+[^|]*(?:-d|--data|--data-raw|--data-binary)\s+",
        0.6,
    ),
    e1(r"wget\s+[^|]*--post-(?:data|file)", 0.6),
    e1(
        r"https?://(?:api\.|data\.|collect\.|telemetry\.|analytics\.)[\w.-]+/",
        0.5,
    ),
    prose(e1(
        r"(?:send|transmit|post|upload)\s+(?:user\s+)?(?:data|information|context|files?)\s+to\s+(?:https?://|external)",
        0.7,
    )),
    // E2: Env Variable Harvesting
    e2(
        r"for\s+\w+\s*,\s*\w+\s+in\s+os\s*\.\s*environ\s*\.\s*items\s*\(\s*\)",
        0.7,
    ),
    e2(r"os\s*\.\s*environ\s*\.\s*copy\s*\(\s*\)", 0.6),
    e2(r"dict\s*\(\s*os\s*\.\s*environ\s*\)", 0.6),
    e2(r"\{\s*\*\*\s*os\s*\.\s*environ\s*\}", 0.6),
    e2(
        r"(?:API_KEY|SECRET|TOKEN|PASSWORD|CREDENTIAL)\s+in\s+(?:key|name|var)",
        0.8,
    ),
    e2(
        r#"process\.env\s*\[\s*['"][^'"]*(?:KEY|SECRET|TOKEN|PASSWORD)[^'"]*['"]\s*\]"#,
        0.7,
    ),
    e2(r"Object\.keys\s*\(\s*process\.env\s*\)", 0.6),
    e2(
        r"env\s*\|\s*grep\s+(?:-i\s+)?(?:key|secret|token|password)",
        0.8,
    ),
    e2(r"printenv\s+(?:\w*(?:KEY|SECRET|TOKEN|PASSWORD)\w*)", 0.7),
    prose(e2(
        r"collect\s+(?:all\s+)?(?:environment\s+variables?|env\s+vars?)",
        0.7,
    )),
    prose(e2(
        r"(?:extract|harvest|gather)\s+(?:api\s+)?keys?\s+from\s+environment",
        0.8,
    )),
    // E3: File System Enumeration
    e3(
        r"glob\s*\.\s*glob\s*\([^)]*(?:\.env|\.ssh|\.aws|\.config|credentials)",
        0.8,
    ),
    e3(r"os\s*\.\s*walk\s*\([^)]*(?:home|~|/Users|/home)", 0.6),
    e3(
        r"Path\s*\.\s*home\s*\(\s*\)\s*\.\s*(?:glob|rglob)\s*\(",
        0.7,
    ),
    e3(
        r"os\s*\.\s*listdir\s*\([^)]*(?:\.ssh|\.aws|\.config|\.gnupg)",
        0.8,
    ),
    e3(r"scandir\s*\([^)]*(?:home|~|/Users|/home)", 0.6),
    e3(
        r#"find\s+[~\$/]\S*\s+.*?-name\s+['"]?\*(?:\.env|\.pem|\.key|credential)"#,
        0.8,
    ),
    e3(r"ls\s+-[la]*R?\s+(?:~/|/home/|/Users/)", 0.6),
    prose(e3(
        r"(?:find|search|scan|enumerate)\s+(?:for\s+)?(?:all\s+)?(?:\.env|credential|secret|key)\s+files?",
        0.8,
    )),
    prose(e3(
        r"(?:list|get)\s+(?:all\s+)?files?\s+(?:in|from)\s+(?:home|~|/Users|/home)",
        0.6,
    )),
    prose(e3(
        r"(?:recursively|recursive)\s+(?:scan|search|find|list)\s+(?:all\s+)?files?",
        0.5,
    )),
    prose(e3(
        r"walk\s+(?:through\s+)?(?:the\s+)?(?:entire\s+)?(?:file\s*system|directory|home)",
        0.6,
    )),
    // E4: Context Leakage
    e4(
        r"(?:send|transmit|upload|log|record)\s+(?:the\s+)?(?:full\s+)?(?:conversation|chat|dialog|session)\s+(?:history|context|log)?",
        0.85,
    ),
    e4(
        r"(?:export|dump|save)\s+(?:the\s+)?(?:conversation|chat|messages?)\s+to\s+(?:external|remote|server)",
        0.9,
    ),
    e4(
        r"log\s+(?:all\s+)?(?:user\s+)?(?:input|messages?|queries?|prompts?)\s+to\s+(?:external|remote|server)",
        0.85,
    ),
    e4(
        r"(?:store|save|record)\s+(?:every|all)\s+(?:user\s+)?(?:interaction|message|input)",
        0.7,
    ),
    e4(
        r"(?:extract|capture|collect)\s+(?:the\s+)?(?:full\s+)?(?:context|conversation|thread)",
        0.75,
    ),
    e4(
        r"(?:include|attach|append)\s+(?:previous\s+)?(?:messages?|context|history)\s+in\s+(?:the\s+)?(?:request|payload)",
        0.7,
    ),
    e4(
        r"(?:send|upload|transmit)\s+(?:the\s+)?(?:session|memory|state)\s+(?:data\s+)?to",
        0.8,
    ),
    e4(
        r"(?:copy|clone|replicate)\s+(?:the\s+)?(?:agent|assistant)\s+(?:memory|context|state)",
        0.75,
    ),
    e4(
        r"(?:always\s+)?include\s+(?:the\s+)?(?:full\s+)?(?:conversation|context)\s+(?:when|in)\s+(?:calling|making)\s+(?:external|api)",
        0.8,
    ),
    // E5: Cloud Storage Exfiltration
    e5(r"\.put_object\s*\(", 0.55),
    e5(r"\.upload_file(?:obj)?\s*\(", 0.55),
    e5(r"\baws\s+s3\s+(?:cp|sync|mv)\b", 0.6),
    e5(r"\baws\s+s3api\s+put-object\b", 0.65),
    e5(r"\bgsutil\s+(?:cp|rsync|mv)\b", 0.6),
    e5(r"\.upload_from_(?:filename|string|file)\s*\(", 0.55),
    e5(r"\baz\s+storage\s+blob\s+upload\b", 0.6),
    e5(r"\.upload_blob\s*\(", 0.55),
    // SC1: Unpinned Dependencies
    sc1(r"^[a-zA-Z][a-zA-Z0-9_-]*\s*$", 0.6),
    sc1(r"^[a-zA-Z][a-zA-Z0-9_-]*\s*>=\s*[\d.]+\s*$", 0.5),
    sc1(r"^[a-zA-Z][a-zA-Z0-9_-]*\s*==\s*\*\s*$", 0.7),
    sc1(r#""[^"]+"\s*:\s*"(?:\*|latest)""#, 0.7),
    sc1(r#""[^"]+"\s*:\s*"\^[\d.]+""#, 0.4),
    prose(sc1(
        r"install\s+(?:the\s+)?latest\s+(?:version\s+)?(?:of\s+)?(?:all\s+)?(?:packages?|dependencies)",
        0.6,
    )),
    prose(sc1(
        r"(?:don't|do\s+not)\s+(?:pin|lock|specify)\s+(?:package\s+)?versions?",
        0.7,
    )),
    // SC2: External Script Fetching
    sc2(r"curl\s+[^|]*\|\s*(?:sudo\s+)?(?:ba)?sh", 0.9),
    sc2(r"wget\s+[^|]*\|\s*(?:sudo\s+)?(?:ba)?sh", 0.9),
    sc2(
        r"curl\s+[^|]*\|\s*(?:sudo\s+)?(?:python|python3|node|ruby|perl)",
        0.9,
    ),
    sc2(
        r"wget\s+[^|]*\|\s*(?:sudo\s+)?(?:python|python3|node|ruby|perl)",
        0.9,
    ),
    sc2(r"curl\s+[^&]*-o\s+\S+\s*&&\s*(?:sudo\s+)?(?:ba)?sh", 0.8),
    sc2(r"wget\s+[^&]*-O\s+\S+\s*&&\s*(?:sudo\s+)?(?:ba)?sh", 0.8),
    sc2(
        r"exec\s*\(\s*(?:urllib|requests|httpx)\.[^)]+\.(?:read|text|content)",
        0.95,
    ),
    sc2(
        r"eval\s*\(\s*(?:urllib|requests|httpx)\.[^)]+\.(?:read|text|content)",
        0.95,
    ),
    sc2(r"eval\s*\(\s*(?:await\s+)?fetch\s*\(", 0.9),
    sc2(r"new\s+Function\s*\([^)]*fetch\s*\(", 0.9),
    sc2(r"subprocess\.[^(]+\([^)]*(?:curl|wget)\s+https?://", 0.8),
    prose(sc2(
        r"download\s+and\s+(?:run|execute)\s+(?:the\s+)?script",
        0.7,
    )),
    prose(sc2(
        r"run\s+(?:this|the)\s+(?:following\s+)?(?:curl|wget)\s+command",
        0.6,
    )),
    // SC3: Obfuscated Code
    sc3(r"exec\s*\(\s*(?:base64\.)?b64decode\s*\(", 0.95),
    sc3(r"eval\s*\(\s*(?:base64\.)?b64decode\s*\(", 0.95),
    sc3(
        r#"exec\s*\(\s*codecs\.decode\s*\([^)]*['"]hex['"]\s*\)"#,
        0.95,
    ),
    sc3(r"marshal\.loads\s*\(", 0.9),
    sc3(r"exec\s*\(\s*marshal\.loads\s*\(", 0.95),
    sc3(r"exec\s*\(\s*compile\s*\([^)]*base64", 0.9),
    sc3(r"exec\s*\(\s*bytes\.fromhex\s*\(", 0.9),
    sc3(r"exec\s*\(\s*bytearray\.fromhex\s*\(", 0.9),
    sc3(r"exec\s*\(\s*(?:zlib|gzip)\.decompress\s*\(", 0.9),
    sc3(r"eval\s*\(\s*atob\s*\(", 0.9),
    sc3(r"new\s+Function\s*\(\s*atob\s*\(", 0.9),
    sc3(r"_0x[a-f0-9]{4,}\s*\(", 0.8),
    sc3(r#"['"][A-Fa-f0-9]{200,}['"]"#, 0.6),
    sc3(r#"['"][A-Za-z0-9+/=]{200,}['"]"#, 0.5),
    sc3(r"\(lambda\s+_:\s*exec\s*\(", 0.9),
    sc3(r#"__import__\s*\(['"]os['"]\s*\)\.system"#, 0.85),
    prose(sc3(
        r"decode\s+(?:this|the)\s+(?:base64|hex)\s+(?:and\s+)?(?:run|execute)",
        0.8,
    )),
    // SC7: Untrusted Container Image
    {
        let mut p = sc7(r"--disable-content-trust\b", 0.85);
        p.not_followed_by = Some("=false");
        p
    },
    sc7(r"DOCKER_CONTENT_TRUST\s*=\s*0", 0.85),
    sc7(r"--insecure-registry", 0.8),
];
