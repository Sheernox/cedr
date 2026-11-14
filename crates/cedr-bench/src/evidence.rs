//! Guarded evidence capture and export. Credential values exist only in memory.
use crate::{
    Result,
    adapter::Harness,
    container::{self, Credential},
    report::{BenchResult, TaskResult},
    suite,
};
use std::path::Path;

pub fn credential_values(credential: Option<&Credential>, harness: Harness) -> Result<Vec<String>> {
    fn collect(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) if s.chars().count() >= 20 => out.push(s.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            serde_json::Value::Object(fields) => fields.values().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut values = Vec::new();
    if let Some(credential) = credential {
        let bytes = std::fs::read(&credential.source)
            .map_err(|_| "cannot read sign-in for evidence guard")?;
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| "cannot parse sign-in for evidence guard")?;
        collect(&value, &mut values);
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

pub fn exposed(credentials: &[String], files: &[&[u8]]) -> bool {
    credentials.iter().any(|value| {
        !value.is_empty()
            && files.iter().any(|file| {
                file.windows(value.len())
                    .any(|window| window == value.as_bytes())
            })
    })
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

/// Offline exports cannot know old credentials, so refuse common serialized token shapes.
pub fn credential_shape(bytes: &[u8]) -> bool {
    fn token_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
    }
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        if line.contains("sk-ant-") || line.contains("sk-proj-") {
            return true;
        }
        // Event streams can contain JSON encoded as a string inside JSON.
        let unescaped = line.replace("\\\"", "\"");
        for key in [
            "refresh_token",
            "access_token",
            "refreshToken",
            "accessToken",
        ] {
            let pattern = format!("\"{key}\"");
            for tail in unescaped.split(&pattern).skip(1) {
                if let Some(value) = tail.trim_start().strip_prefix(':') {
                    let value = value.trim_start();
                    if let Some(Ok(value)) = serde_json::Deserializer::from_str(value)
                        .into_iter::<serde_json::Value>()
                        .next()
                        && !value.is_null()
                        && value.as_str() != Some("")
                    {
                        return true;
                    }
                }
            }
        }
        if line
            .split(|ch: char| !token_char(ch) && ch != '.')
            .any(|part| {
                let segments: Vec<_> = part.split('.').collect();
                segments.windows(3).any(|parts| {
                    parts.iter().all(|s| !s.is_empty())
                        && parts.iter().map(|s| s.len()).sum::<usize>() + 2 >= 100
                })
            })
        {
            return true;
        }
    }
    false
}

/// Read and validate the exact bytes to be exported, including path and digest checks.
pub fn read_verified(
    root: &Path,
    relative: &str,
    hash: &str,
    size: Option<u64>,
) -> Result<Vec<u8>> {
    suite::artifact(root, relative).map_err(|_| format!("cannot read evidence {relative}"))?;
    let bytes = std::fs::read(root.join(relative))
        .map_err(|_| format!("cannot read evidence {relative}"))?;
    if crate::sha(&bytes) != hash || size.is_some_and(|n| n != bytes.len() as u64) {
        return Err(format!("evidence digest mismatch: {relative}"));
    }
    if credential_shape(&bytes) {
        return Err(format!("credential guard refused evidence: {relative}"));
    }
    Ok(bytes)
}

pub fn export_task(root: &Path, task: &TaskResult, dest: &Path) -> Result<()> {
    let patch = format!("{}/submission.patch", task.id);
    let transcript = format!("{}/transcript.jsonl", task.id);
    let patch_hash = task
        .patch_sha256
        .as_deref()
        .ok_or_else(|| format!("missing evidence digest: {patch}"))?;
    let transcript_hash = task
        .transcript_sha256
        .as_deref()
        .ok_or_else(|| format!("missing evidence digest: {transcript}"))?;
    let size = task
        .transcript_bytes
        .ok_or_else(|| format!("missing evidence length: {transcript}"))?;
    let patch_bytes = read_verified(root, &patch, patch_hash, None)?;
    let transcript_bytes = read_verified(root, &transcript, transcript_hash, Some(size))?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    write_private(&dest.join("submission.patch"), &patch_bytes)?;
    write_private(&dest.join("transcript.jsonl"), &transcript_bytes)
}

/// Validate into private staging before the CLI creates any export destination.
pub fn stage(records: &[BenchResult], results: &Path) -> Result<tempfile::TempDir> {
    let staged = tempfile::tempdir().map_err(|e| e.to_string())?;
    for record in records {
        for task in &record.tasks {
            export_task(
                &results.join(&record.id).join("artifacts"),
                task,
                &staged
                    .path()
                    .join("evidence")
                    .join(&record.id)
                    .join(&task.id),
            )?;
        }
    }
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let values = credential_values(Some(&credential), Harness::Command).unwrap();
        assert_eq!(values, ["fake-credential-for-regression-only"]);
    }
    #[test]
    fn credentials_in_either_file_are_detected_without_persisting() {
        let secret = "fake-credential-for-regression-only".to_string();
        assert!(exposed(
            std::slice::from_ref(&secret),
            &[b"ordinary event", secret.as_bytes()]
        ));
        assert!(exposed(
            std::slice::from_ref(&secret),
            &[secret.as_bytes(), b"ordinary patch"]
        ));
        assert!(!exposed(&[secret], &[b"ordinary event", b"ordinary patch"]));
    }
    #[test]
    fn export_checks_hashes_lengths_and_credential_shapes() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("task")).unwrap();
        let mut task = TaskResult::pending("task", suite::Kind::Code);
        let patch = b"diff --git a/file b/file\n";
        let transcript = b"{\"type\":\"complete\"}\n";
        std::fs::write(root.path().join("task/submission.patch"), patch).unwrap();
        std::fs::write(root.path().join("task/transcript.jsonl"), transcript).unwrap();
        task.patch_sha256 = Some(crate::sha(patch));
        task.transcript_sha256 = Some(crate::sha(transcript));
        task.transcript_bytes = Some(transcript.len() as u64);
        export_task(root.path(), &task, &out.path().join("good")).unwrap();
        assert_eq!(
            std::fs::read(out.path().join("good/transcript.jsonl")).unwrap(),
            transcript
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(out.path().join("good/transcript.jsonl"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::write(root.path().join("task/submission.patch"), b"changed").unwrap();
        assert!(
            export_task(root.path(), &task, &out.path().join("bad-hash"))
                .unwrap_err()
                .contains("submission.patch")
        );
        let secret = b"+{\"refresh_token\":\"fake-secret\"}\n";
        std::fs::write(root.path().join("task/submission.patch"), secret).unwrap();
        task.patch_sha256 = Some(crate::sha(secret));
        assert!(
            export_task(root.path(), &task, &out.path().join("bad-key"))
                .unwrap_err()
                .contains("submission.patch")
        );
        assert!(!out.path().join("bad-key").exists());
        std::fs::write(root.path().join("task/submission.patch"), patch).unwrap();
        task.patch_sha256 = Some(crate::sha(patch));
        std::fs::write(root.path().join("task/transcript.jsonl"), secret).unwrap();
        task.transcript_sha256 = Some(crate::sha(secret));
        task.transcript_bytes = Some(secret.len() as u64);
        assert!(
            export_task(root.path(), &task, &out.path().join("bad-transcript"))
                .unwrap_err()
                .contains("transcript.jsonl")
        );
        assert!(!out.path().join("bad-transcript").exists());
        for value in [
            "sk-ant-fake",
            "sk-proj-fake",
            "{\"accessToken\":\"fake\"}",
            &format!("{}.{}.{}", "a".repeat(40), "b".repeat(40), "c".repeat(40)),
        ] {
            assert!(credential_shape(value.as_bytes()));
        }
        assert!(!credential_shape(b"{\"access_token\":\"\"}"));
    }
}
