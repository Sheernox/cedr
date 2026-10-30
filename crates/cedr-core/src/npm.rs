//! npm registry access and tarball reading. Tarballs are checked against the
//! registry's `dist.integrity` (SRI sha512) before any entry is read, and are
//! read in memory: nothing from a package is written to disk or executed.

use std::io::Read;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::read::MultiGzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha512};

use crate::source::{is_dist_tag, is_exact_version, is_npm_name};
use crate::{Error, Result};

const REGISTRY: &str = "https://registry.npmjs.org";
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024;
/// Limits on what is read out of a tarball, against decompression bombs.
const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub(crate) struct VersionDoc {
    pub version: String,
    pub dist: Dist,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Dist {
    pub tarball: String,
    pub integrity: Option<String>,
}

pub(crate) struct Registry {
    client: reqwest::blocking::Client,
    base: String,
}

impl Registry {
    pub fn new() -> Result<Registry> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .https_only(true)
            .redirect(redirect_policy())
            .user_agent(concat!("cedr/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Registry(format!("could not start the HTTP client: {e}")))?;
        Ok(Registry {
            client,
            base: REGISTRY.into(),
        })
    }

    // Only unit tests can opt into a loopback registry; production remains HTTPS-only.
    #[cfg(test)]
    pub(crate) fn mock(base: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            base,
        }
    }

    fn local_tarball(&self, url: &str) -> bool {
        cfg!(test)
            && self.base.starts_with("http://127.0.0.1:")
            && url.starts_with(&format!("{}/", self.base))
    }

    /// The registry document for one version or dist-tag of a package.
    pub fn version_doc(&self, name: &str, version: &str) -> Result<VersionDoc> {
        let tag_or_version = is_dist_tag(version) || is_exact_version(version);
        if !is_npm_name(name) || !tag_or_version {
            return Err(Error::Source(format!(
                "not a valid npm package and version: {name}@{version}"
            )));
        }
        let url = format!("{}/{}/{version}", self.base, name.replacen('/', "%2F", 1));
        let body = self.get(&url, MAX_METADATA_BYTES)?;
        serde_json::from_slice(&body).map_err(|e| {
            Error::Registry(format!(
                "{}: unexpected registry response: {e}",
                url_label(&url)
            ))
        })
    }

    /// Downloads a version's tarball and checks it against `dist.integrity`.
    pub fn tarball(&self, doc: &VersionDoc) -> Result<(Vec<u8>, String)> {
        let integrity = doc.dist.integrity.clone().ok_or_else(|| {
            Error::Registry(format!(
                "registry gives no dist.integrity for version {}; refusing to read an unverified tarball",
                doc.version
            ))
        })?;
        if !doc.dist.tarball.starts_with("https://") && !self.local_tarball(&doc.dist.tarball) {
            return Err(Error::Registry(format!(
                "tarball URL is not https: {}",
                url_label(&doc.dist.tarball)
            )));
        }
        let bytes = self.get(&doc.dist.tarball, MAX_TARBALL_BYTES)?;
        verify_integrity(&bytes, &integrity)?;
        Ok((bytes, integrity))
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        // Signed tarball URLs carry credentials in their query: messages name scheme and host only.
        let label = url_label(url);
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|e| Error::Registry(format!("{label}: {}", e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Registry(format!("{label}: HTTP {status}")));
        }
        let mut body = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut body)
            .map_err(|e| Error::Registry(format!("{label}: {e}")))?;
        if body.len() as u64 > limit {
            return Err(Error::Registry(format!(
                "{label}: response is larger than {limit} bytes"
            )));
        }
        Ok(body)
    }
}

/// Reject downgrade redirects before any request is sent to the target.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            attempt.error("redirect target is not https")
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

pub(crate) fn sri_sha512(bytes: &[u8]) -> String {
    format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
}

/// Accepts the bytes when any sha512 entry of the SRI string matches them.
pub(crate) fn verify_integrity(bytes: &[u8], sri: &str) -> Result<()> {
    let expected: Vec<&str> = sri
        .split_whitespace()
        .filter(|s| s.starts_with("sha512-"))
        .collect();
    if expected.is_empty() {
        return Err(Error::Registry(format!(
            "integrity {sri:?} has no sha512 hash; refusing to read an unverified tarball"
        )));
    }
    let actual = sri_sha512(bytes);
    if expected
        .iter()
        .any(|e| e.split('?').next() == Some(actual.as_str()))
    {
        Ok(())
    } else {
        Err(Error::Registry(format!(
            "tarball integrity mismatch: expected {sri}, got {actual}"
        )))
    }
}

/// Source files of a package tarball as (path without the leading
/// `package/` directory, text), filtered by `keep`, sorted by path.
pub(crate) fn source_files(
    tgz: &[u8],
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<(String, String)>> {
    source_files_limited(tgz, keep, MAX_UNPACKED_BYTES)
}

fn source_files_limited(
    tgz: &[u8],
    keep: impl Fn(&str) -> bool,
    limit: u64,
) -> Result<Vec<(String, String)>> {
    let decoder = MultiGzDecoder::new(tgz).take(limit + 1);
    let mut archive = tar::Archive::new(decoder);
    let mut files = Vec::new();
    let mut unpacked = 0u64;
    let bad = |e: std::io::Error| Error::Package(format!("unreadable package tarball: {e}"));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let size = entry.header().size().map_err(bad)?;
        unpacked = unpacked
            .checked_add(size)
            .ok_or_else(|| Error::Package("package size overflow".into()))?;
        if unpacked > limit {
            return Err(Error::Package(format!(
                "package is larger than {limit} bytes"
            )));
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let raw = entry
            .path()
            .map_err(bad)?
            .to_string_lossy()
            .replace('\\', "/");
        let path = match raw.split_once('/') {
            Some((_, rest)) => rest.to_string(),
            None => raw,
        };
        if !keep(&path) || size > MAX_SOURCE_FILE_BYTES {
            continue;
        }
        let mut bytes = Vec::with_capacity(size as usize);
        entry
            .by_ref()
            .take(MAX_SOURCE_FILE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(bad)?;
        files.push((path, String::from_utf8_lossy(&bytes).into_owned()));
    }
    // Drain trailing tar padding and gzip members through the same byte budget.
    let mut decoder = archive.into_inner();
    std::io::copy(&mut decoder, &mut std::io::sink()).map_err(bad)?;
    if decoder.limit() == 0 {
        return Err(Error::Package(format!(
            "decompressed package is larger than {limit} bytes"
        )));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// A URL as error messages may name it: scheme and host only, never its path or query.
fn url_label(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")),
        Err(_) => "an invalid URL".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_labels_keep_scheme_and_host_only() {
        assert_eq!(
            url_label("https://cdn.example.net/pkg.tgz?X-Sig=s3cret"),
            "https://cdn.example.net"
        );
        assert_eq!(url_label("not a url"), "an invalid URL");
    }

    fn tarball(entries: &[(&str, u8, usize)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, size) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(*size as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::new(*kind));
            header.set_cksum();
            builder
                .append_data(&mut header, path, vec![0; *size].as_slice())
                .unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn unpacked_budget_counts_unscanned_and_non_file_entries() {
        for kind in *b"02" {
            let tgz = tarball(&[("package/a.bin", kind, 2048), ("package/b.bin", kind, 2048)]);
            assert!(source_files_limited(&tgz, |_| false, 3000).is_err());
        }
    }

    #[test]
    fn decoder_budget_counts_headers_and_padding() {
        let tgz = tarball(&[("package/a.js", b'0', 1)]);
        assert!(source_files_limited(&tgz, |_| true, 512).is_err());
        assert!(source_files_limited(&tgz, |_| true, 8192).is_ok());
    }

    #[test]
    fn registry_client_rejects_plain_http_before_connecting() {
        let registry = Registry::new().unwrap();
        let error = registry
            .get("http://127.0.0.1:9/package", 100)
            .unwrap_err()
            .to_string();
        assert!(error.contains("builder error"), "{error}");
    }

    #[test]
    fn redirect_policy_blocks_http_target_before_connection() {
        use std::io::Write;
        use std::net::TcpListener;
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let origin_addr = origin.local_addr().unwrap();
        let target_addr = target.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = origin.accept().unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            write!(socket, "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(redirect_policy())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let error = client
            .get(format!("http://{origin_addr}/"))
            .send()
            .unwrap_err();
        server.join().unwrap();
        assert!(error.is_redirect(), "{error}");
        assert!(matches!(target.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn integrity_accepts_match_and_rejects_others() {
        let sri = sri_sha512(b"abc");
        assert!(verify_integrity(b"abc", &sri).is_ok());
        assert!(verify_integrity(b"abd", &sri).is_err());
        assert!(verify_integrity(b"abc", "sha1-AAAA").is_err());
        assert!(verify_integrity(b"abc", &format!("sha256-x {sri}")).is_ok());
    }
}
