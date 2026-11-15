//! Local patch verification and registered-secret masking. Secrets exist only in memory.
use crate::{
    Result,
    adapter::Harness,
    container::{self, Credential},
    report::TaskResult,
    suite,
};
use base64::Engine;
use std::{
    cell::Cell,
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

/// Exclusive random allocation happens before any billable probe or agent call.
pub fn pending(root: &Path) -> Result<tempfile::TempDir> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    tempfile::Builder::new()
        .prefix("pending-")
        .tempdir_in(root)
        .map_err(|e| e.to_string())
}

/// Register sign-in strings with a secret-bearing key or ancestor (token, secret,
/// password, credential, apikey, api_key, or exactly key; ASCII case-insensitive)
/// at eight characters, or opaque non-URL strings at 40 characters. Other file
/// strings are metadata that can occur in honest output. Credential environment
/// values are declared secrets, so register every nonempty value regardless of length.
pub fn credential_values(credential: Option<&Credential>, harness: Harness) -> Result<Vec<String>> {
    fn collect(value: &serde_json::Value, secret_key: bool, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => {
                let length = s.chars().count();
                if (secret_key && length >= 8)
                    || (length >= 40
                        && !s.contains(char::is_whitespace)
                        && !s.starts_with("http://")
                        && !s.starts_with("https://"))
                {
                    out.push(s.clone());
                }
            }
            serde_json::Value::Array(items) => {
                items.iter().for_each(|v| collect(v, secret_key, out));
            }
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    let key = key.to_ascii_lowercase();
                    let secret_key = secret_key
                        || key == "key"
                        || [
                            "token",
                            "secret",
                            "password",
                            "credential",
                            "apikey",
                            "api_key",
                        ]
                        .iter()
                        .any(|part| key.contains(part));
                    collect(value, secret_key, out);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    if let Some(credential) = credential {
        let bytes = std::fs::read(&credential.source)
            .map_err(|_| "cannot read sign-in for evidence guard")?;
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| "cannot parse sign-in for evidence guard")?;
        collect(&value, false, &mut values);
    }
    for name in container::token_env(harness) {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            values.push(value);
        }
    }
    Ok(values)
}

/// In-memory registered secrets and their fixed encodings. Never format this type.
pub struct Masker {
    forms: BTreeSet<String>,
    elapsed: Cell<Duration>,
}
impl Masker {
    pub fn new(values: Vec<String>) -> Self {
        let start = Instant::now();
        let mut forms = BTreeSet::new();
        for value in values {
            for value in std::iter::once(value.as_str())
                .chain(
                    value
                        .split(['\r', '\n'])
                        .map(str::trim)
                        .filter(|s| s.chars().count() >= 8),
                )
                .filter(|s| !s.is_empty())
            {
                forms.insert(value.to_owned());
                forms.extend(encoded_forms(value).into_iter().filter(|s| !s.is_empty()));
            }
        }
        Self {
            forms,
            elapsed: Cell::new(start.elapsed()),
        }
    }
    pub fn exposed(&self, files: &[&[u8]]) -> bool {
        let start = Instant::now();
        let found = files.iter().any(|bytes| {
            // Every registered form is UTF-8, so a match cannot cross invalid
            // bytes. Search valid spans with the standard linear substring search
            // instead of comparing a whole secret at every byte position.
            let mut remaining = *bytes;
            loop {
                match std::str::from_utf8(remaining) {
                    Ok(text) => return self.forms.iter().any(|form| text.contains(form)),
                    Err(error) => {
                        let valid = error.valid_up_to();
                        let text = std::str::from_utf8(&remaining[..valid])
                            .expect("UTF-8 error identifies a valid prefix");
                        if self.forms.iter().any(|form| text.contains(form)) {
                            return true;
                        }
                        let skip = error.error_len().unwrap_or(remaining.len() - valid);
                        remaining = &remaining[valid + skip..];
                    }
                }
            }
        });
        self.elapsed.set(self.elapsed.get() + start.elapsed());
        found
    }
    pub fn elapsed(&self) -> Duration {
        self.elapsed.get()
    }
}

// Adapted from actions/runner at 8cfa1049f86781d141056082d73802d56cd2b433:
// src/Sdk/DTLogging/Logging/ValueEncoders.cs and
// src/Sdk/DTExpressions2/Expressions2/Sdk/ExpressionUtility.cs; registered in
// src/Runner.Common/HostContext.cs. Copyright (c) 2019 GitHub, MIT (see NOTICE).
// AddValue has no general minimum. Keep the >8 quote and >=6 PowerShell rules.
fn encoded_forms(value: &str) -> [String; 11] {
    let base64 = |shift| {
        base64::engine::general_purpose::STANDARD.encode(
            value
                .as_bytes()
                .get(shift..)
                .filter(|s| !s.is_empty())
                .unwrap_or(value.as_bytes()),
        )
    };
    let json = serde_json::to_string(value).expect("string serialization cannot fail");
    let json = json[1..json.len() - 1]
        .replace('\u{85}', "\\u0085")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    const URI_ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    let uri = percent_encoding::utf8_percent_encode(value, URI_ESCAPE).to_string();
    let xml = value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    let units: Vec<u16> = value.encode_utf16().collect();
    let trimmed = if units.len() > 8 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_owned()
    } else {
        String::new()
    };
    let mut pre = String::new();
    let mut post = String::new();
    if let Some(amp) = units.iter().rposition(|u| *u == u16::from(b'&')) {
        let (end, start) = if let Some(plus) = units.windows(2).position(|w| w == [38, 43]) {
            (plus + 2, (plus + 3).min(units.len()))
        } else {
            (amp + 1, amp + 1)
        };
        if end >= 6 {
            pre = String::from_utf16_lossy(&units[..end]);
        }
        if units.len() - start >= 6 {
            post = String::from_utf16_lossy(&units[start..]);
        }
    }
    [
        base64(0),
        base64(1),
        base64(2),
        value.replace('"', "\\\""),
        value.replace('\'', "''"),
        json,
        uri,
        xml,
        trimmed,
        pre,
        post,
    ]
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot create evidence {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot save evidence {}: {e}", path.display()))
}

/// A clean review binds each flagged task to its retained patch, never to agent logs.
pub fn verify_tasks(root: &Path, tasks: &[TaskResult]) -> Result<()> {
    for task in tasks.iter().filter(|task| !task.tamper_signals.is_empty()) {
        let relative = format!("{}/submission.patch", task.id);
        let hash = task.patch_sha256.as_deref().ok_or_else(|| {
            format!("missing patch digest: {relative}; the run must be run again")
        })?;
        suite::artifact(root, &relative).map_err(|_| format!("cannot read patch {relative}"))?;
        let bytes = std::fs::read(root.join(&relative))
            .map_err(|_| format!("cannot read patch {relative}"))?;
        if crate::sha(&bytes) != hash {
            return Err(format!("patch digest mismatch: {relative}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_artifacts_never_reuse_a_stale_or_live_directory() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join(format!("pending-{}", std::process::id()));
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(stale.join("submission.patch"), "stale evidence").unwrap();
        let first = pending(root.path()).unwrap();
        let second = pending(root.path()).unwrap();
        assert_ne!(first.path(), second.path());
        assert_ne!(first.path(), stale);
        assert_eq!(std::fs::read_dir(first.path()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_to_string(stale.join("submission.patch")).unwrap(),
            "stale evidence"
        );
    }
    #[test]
    fn mounted_sign_in_values_are_collected_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("sign-in.json");
        std::fs::write(
            &source,
            r#"{"short":"ignored","nested":[{"token":"fake-credential-for-regression-only"}]}"#,
        )
        .unwrap();
        let credential = Credential {
            source,
            target: "/test-sign-in",
        };
        let mut values = credential_values(Some(&credential), Harness::Command).unwrap();
        values.sort();
        assert_eq!(values, ["fake-credential-for-regression-only"]);
    }
    #[test]
    fn vendor_metadata_is_not_registered_or_withheld() {
        for sign_in in [
            serde_json::json!({"claudeAiOauth": {
                "accessToken": "synthetic-claude-access-token",
                "subscriptionType": "pro", "subscriptionTypeSource": "billing"
            }}),
            serde_json::json!({"access_token": "synthetic-grok-access-token",
                "principal_type": "user", "auth_mode": "oauth", "first_name": "Alexandria"}),
            serde_json::json!({"credentials": {"value": "synthetic-muse-access-token"},
                "mechanism": "oauth", "user_full_name": "Test User",
                "api_base_url": "https://api.example.com"}),
            serde_json::json!({"tokens": {"access_token": "synthetic-codex-access-token"},
                "auth_mode": "chatgpt"}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("sign-in.json");
            std::fs::write(&source, sign_in.to_string()).unwrap();
            let values = credential_values(
                Some(&Credential {
                    source,
                    target: "/test-sign-in",
                }),
                Harness::Command,
            )
            .unwrap();
            assert_eq!(values.len(), 1);
            assert!(values[0].starts_with("synthetic-"));
            let secret = values[0].clone();
            let masker = Masker::new(values);
            let honest = br#"{"output":"pro billing user oauth Alexandria Test User chatgpt https://api.example.com"}"#;
            assert!(!masker.exposed(&[honest]));
            assert!(masker.exposed(&[secret.as_bytes()]));
            let encoded = base64::engine::general_purpose::STANDARD.encode(secret);
            assert!(masker.exposed(&[encoded.as_bytes()]));
        }
    }
    #[test]
    fn sign_in_registration_respects_key_and_opaque_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("sign-in.json");
        let opaque = "a".repeat(40);
        let sign_in = serde_json::json!({
            "unknown": opaque,
            "short_opaque": "b".repeat(39),
            "url": format!("https://example.com/{}", "c".repeat(40)),
            "other_url": format!("http://example.com/{}", "d".repeat(40)),
            "words": "ordinary metadata separated by whitespace ".repeat(2),
            "token": "1234567",
            "secret": "12345678",
            "PASSWORD": "password-eight",
            "myApiKey": "apikey-eight",
            "my_api_key": "api_key-eight",
            "key": "key-eight",
            "monkey": "not-a-secret",
            "CrEdEnTiAlS": [{"nested": {"value": "ancestor-eight"}}],
            "unicode_token": "界".repeat(7),
            "unknown_unicode": "界".repeat(39),
            "empty_token": ""
        });
        std::fs::write(&source, sign_in.to_string()).unwrap();
        let mut values = credential_values(
            Some(&Credential {
                source,
                target: "/test-sign-in",
            }),
            Harness::Command,
        )
        .unwrap();
        values.sort();
        let mut expected = vec![
            opaque.clone(),
            "12345678".into(),
            "password-eight".into(),
            "apikey-eight".into(),
            "api_key-eight".into(),
            "key-eight".into(),
            "ancestor-eight".into(),
        ];
        expected.sort();
        assert_eq!(values, expected);
        assert!(Masker::new(values).exposed(&[opaque.as_bytes()]));
    }
    #[test]
    fn credentials_in_either_file_are_detected_without_persisting() {
        let secret = "fake-credential-for-regression-only";
        let masker = Masker::new(vec![secret.into()]);
        assert!(masker.exposed(&[b"ordinary event", secret.as_bytes()]));
        assert!(masker.exposed(&[secret.as_bytes(), b"ordinary patch"]));
        assert!(!masker.exposed(&[b"ordinary event", b"ordinary patch"]));
        let binary = [b"\xff\xfe".as_slice(), secret.as_bytes(), b"\xff"].concat();
        assert!(masker.exposed(&[&binary]));
    }
    #[test]
    fn github_encodings_are_matched_with_upstream_thresholds() {
        // Each expected spelling is independent of the encoder implementation.
        for (index, secret, expected) in [
            (0, "abcdef", "YWJjZGVm"),
            (1, "abcdef", "YmNkZWY="),
            (2, "abcdef", "Y2RlZg=="),
            (3, "some\"quoted\"value", r#"some\"quoted\"value"#),
            (4, "some'quoted'value", "some''quoted''value"),
            (5, "some\\value\n", r#"some\\value\n"#),
            (6, "some/value+more", "some%2Fvalue%2Bmore"),
            (7, "some<value>&'\"", "some&lt;value&gt;&amp;&apos;&quot;"),
            (8, "\"quotedvalue\"", "quotedvalue"),
            (9, "before&+afterword", "before&+"),
            (10, "before&+afterword", "fterword"),
        ] {
            assert_eq!(encoded_forms(secret)[index], expected, "encoder {index}");
            assert!(
                Masker::new(vec![secret.into()]).exposed(&[expected.as_bytes()]),
                "encoder {index}"
            );
        }
        assert!(Masker::new(vec!["x".into()]).exposed(&[b"x"]));
        assert!(!Masker::new(vec!["".into()]).exposed(&[b"ordinary"]));
        let masker = Masker::new(vec!["  line-one\r\n line-two  ".into()]);
        assert!(masker.exposed(&[b"line-two"]));
        let masker = Masker::new(vec!["abc\r\n metadata\r\nabcdefgh".into()]);
        assert!(!masker.exposed(&[b"abc"]));
        assert!(masker.exposed(&[b"metadata"]));
        assert!(masker.exposed(&[b"abcdefgh"]));
        let forms = encoded_forms("short&tail");
        assert_eq!(&forms[9..], ["short&", ""]);
        assert_eq!(&encoded_forms("tiny&tail")[9..], ["", ""]);
        assert_eq!(encoded_forms("\"abcdef\"")[8], "");
        assert_eq!(encoded_forms("\"abcdefg\"")[8], "abcdefg");
        assert_eq!(
            &encoded_forms("before&middle&afterword")[9..],
            ["before&middle&", "afterword"]
        );
        assert_eq!(
            encoded_forms("a\u{85}b\u{2028}c\u{2029}")[5],
            "a\\u0085b\\u2028c\\u2029"
        );
    }
}
