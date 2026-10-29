//! Where a server comes from, written as `npm:<pkg>@<ver>`, `pypi:<pkg>==<ver>`,
//! `oci:<image>@<digest>`, `command:<command>`, `url:<url>` or (for tests and
//! fixtures) `file:<path-to.tgz>`.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Error, Result};

static EXACT_SEMVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$").expect("regex")
});
// Names end up in registry URLs, so only npm's own name alphabet is accepted.
static NPM_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:@[A-Za-z0-9][A-Za-z0-9._~-]*/)?[A-Za-z0-9][A-Za-z0-9._~-]*$").expect("regex")
});
static NPM_VERSION_SPEC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._+~^<>=| -]+$").expect("regex"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `version` is whatever was configured: an exact version, a range, a
    /// dist-tag, or nothing.
    Npm {
        name: String,
        version: Option<String>,
    },
    Pypi {
        name: String,
        version: Option<String>,
    },
    Oci {
        image: String,
        digest: Option<String>,
    },
    Command(String),
    Url(String),
    File(String),
}

impl Source {
    /// False only where the design calls a source unpinned: npm without an
    /// exact version, PyPI without `==<ver>`, OCI without a digest. Local
    /// commands and remote URLs have no version to pin.
    pub fn pinned(&self) -> bool {
        match self {
            Source::Npm { version, .. } => version.as_deref().is_some_and(is_exact_version),
            Source::Pypi { version, .. } => version.is_some(),
            Source::Oci { digest, .. } => digest.is_some(),
            Source::Command(_) | Source::Url(_) | Source::File(_) => true,
        }
    }

    /// The source without its version or digest, used to match an unpinned
    /// lock entry (recorded at the version it resolved to) against a config
    /// that still names no version.
    pub fn identity(&self) -> String {
        match self {
            Source::Npm { name, .. } => format!("npm:{name}"),
            Source::Pypi { name, .. } => format!("pypi:{name}"),
            Source::Oci { image, .. } => format!("oci:{image}"),
            other => other.to_string(),
        }
    }

    /// Resolves a stdio server's launch command per the design's Source resolution.
    pub fn from_command(command: &str, args: &[String]) -> Source {
        let program = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(command);
        let program = program.strip_suffix(".cmd").unwrap_or(program);
        let resolved = match program {
            "npx" | "bunx" => npm_from_args(args),
            "pnpm" if args.first().map(String::as_str) == Some("dlx") => npm_from_args(&args[1..]),
            "uvx" => pypi_from_args(args),
            "docker" | "podman" if args.first().map(String::as_str) == Some("run") => {
                oci_from_args(&args[1..])
            }
            _ => None,
        };
        resolved.unwrap_or_else(|| Source::Command(command.to_string()))
    }

    /// Splits an npm spec such as `@scope/name@1.2.3` at the version separator.
    pub fn npm_spec(spec: &str) -> Source {
        let split = spec
            .char_indices()
            .skip(1)
            .find(|&(_, c)| c == '@')
            .map(|(i, _)| i);
        match split {
            Some(i) => Source::Npm {
                name: spec[..i].to_string(),
                version: Some(spec[i + 1..].to_string()).filter(|v| !v.is_empty()),
            },
            None => Source::Npm {
                name: spec.to_string(),
                version: None,
            },
        }
    }

    /// The npm package name and exact version, or an error naming what is missing.
    pub fn npm_exact(&self) -> Result<(&str, &str)> {
        match self {
            Source::Npm {
                name,
                version: Some(version),
            } if is_exact_version(version) => Ok((name, version)),
            Source::Npm { .. } => Err(Error::Source(format!(
                "{self} has no exact version; use npm:<pkg>@<x.y.z>"
            ))),
            _ => Err(Error::Source(format!("{self} is not an npm source"))),
        }
    }
}

/// Whether `name` uses only npm's package name alphabet, so it is safe in a registry URL.
pub(crate) fn is_npm_name(name: &str) -> bool {
    NPM_NAME.is_match(name)
}

/// Tags use npm's unescaped URI-component alphabet and cannot be version
/// selectors. Keeping them to a single component prevents registry URL injection.
pub(crate) fn is_dist_tag(tag: &str) -> bool {
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z0-9.!~*'()_-]+$").expect("regex"));
    static VERSION_SELECTOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[v~])?(?:(?:[0-9]+|[xX*])(?:\.(?:[0-9]+|[xX*])){0,2}|[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.-]+)$").expect("regex")
    });
    !matches!(tag, "." | "..") && TAG.is_match(tag) && !VERSION_SELECTOR.is_match(tag)
}

pub fn is_exact_version(version: &str) -> bool {
    EXACT_SEMVER.is_match(version)
}

fn npm_from_args(args: &[String]) -> Option<Source> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(pkg) = arg.strip_prefix("--package=") {
            return Some(Source::npm_spec(pkg));
        }
        if arg == "-p" || arg == "--package" {
            return iter.next().map(|pkg| Source::npm_spec(pkg));
        }
        if arg == "--" {
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(Source::npm_spec(arg));
    }
    None
}

fn pypi_from_args(args: &[String]) -> Option<Source> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(spec) = arg.strip_prefix("--from=") {
            return Some(pypi_spec(spec));
        }
        if arg == "--from" {
            return iter.next().map(|spec| pypi_spec(spec));
        }
        if UVX_VALUE_FLAGS.contains(&arg.as_str()) {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(pypi_spec(arg));
    }
    None
}

const UVX_VALUE_FLAGS: &[&str] = &["--with", "--python", "-p", "--index", "--index-url"];

fn pypi_spec(spec: &str) -> Source {
    match spec.split_once("==") {
        Some((name, version)) if !version.is_empty() => Source::Pypi {
            name: name.to_string(),
            version: Some(version.to_string()),
        },
        _ => Source::Pypi {
            name: spec.to_string(),
            version: None,
        },
    }
}

// `docker run` flags that take a separate value; the image is the first
// argument that is neither a flag nor such a value.
const DOCKER_VALUE_FLAGS: &[&str] = &[
    "-e",
    "--env",
    "--env-file",
    "-v",
    "--volume",
    "--mount",
    "-p",
    "--publish",
    "--name",
    "--network",
    "--net",
    "-w",
    "--workdir",
    "-u",
    "--user",
    "--entrypoint",
    "-l",
    "--label",
    "--platform",
    "--pull",
    "-m",
    "--memory",
    "--cpus",
    "--add-host",
    "--cap-add",
    "--cap-drop",
    "--security-opt",
    "-h",
    "--hostname",
    "--device",
    "--tmpfs",
    "--ulimit",
    "--runtime",
    "--restart",
    "--log-driver",
    "--log-opt",
];

fn oci_from_args(args: &[String]) -> Option<Source> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if DOCKER_VALUE_FLAGS.contains(&arg.as_str()) {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(match arg.split_once("@sha256:") {
            Some((image, digest)) => Source::Oci {
                image: image.to_string(),
                digest: Some(format!("sha256:{digest}")),
            },
            None => Source::Oci {
                image: arg.to_string(),
                digest: None,
            },
        });
    }
    None
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Npm {
                name,
                version: Some(v),
            } => write!(f, "npm:{name}@{v}"),
            Source::Npm {
                name,
                version: None,
            } => write!(f, "npm:{name}"),
            Source::Pypi {
                name,
                version: Some(v),
            } => write!(f, "pypi:{name}=={v}"),
            Source::Pypi {
                name,
                version: None,
            } => write!(f, "pypi:{name}"),
            Source::Oci {
                image,
                digest: Some(d),
            } => write!(f, "oci:{image}@{d}"),
            Source::Oci {
                image,
                digest: None,
            } => write!(f, "oci:{image}"),
            Source::Command(c) => write!(f, "command:{c}"),
            Source::Url(u) => write!(f, "url:{u}"),
            Source::File(p) => write!(f, "file:{p}"),
        }
    }
}

impl FromStr for Source {
    type Err = Error;

    fn from_str(s: &str) -> Result<Source> {
        let invalid = || Error::Source(format!("invalid source {s:?}"));
        let (scheme, rest) = s.split_once(':').ok_or_else(invalid)?;
        if rest.is_empty() {
            return Err(invalid());
        }
        let source = match scheme {
            "npm" => {
                let source = Source::npm_spec(rest);
                if let Source::Npm { name, version } = &source {
                    let version_ok = version
                        .as_deref()
                        .is_none_or(|v| NPM_VERSION_SPEC.is_match(v) || is_dist_tag(v));
                    if !is_npm_name(name) || !version_ok {
                        return Err(invalid());
                    }
                }
                source
            }
            "pypi" => pypi_spec(rest),
            "oci" => match rest.split_once("@sha256:") {
                Some((image, digest)) => Source::Oci {
                    image: image.to_string(),
                    digest: Some(format!("sha256:{digest}")),
                },
                None => Source::Oci {
                    image: rest.to_string(),
                    digest: None,
                },
            },
            "command" => Source::Command(rest.to_string()),
            "url" => Source::Url(rest.to_string()),
            "file" => Source::File(rest.to_string()),
            _ => return Err(invalid()),
        };
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dist_tag_names_are_single_components_and_not_version_selectors() {
        for tag in [
            "latest", "next", "beta-2", "canary.1", "123abc", "-next", "vNext", "tag!", "~beta",
            "1-beta",
        ] {
            assert!(is_dist_tag(tag), "{tag}");
            assert!(format!("npm:pkg@{tag}").parse::<Source>().is_ok());
        }
        for selector in [
            "", "1", "v1", "1.2", "1.x", "*", "1.2.3", "^1.2", "../next", "next?q=1", "next#x",
            "next/tag", ".", "..",
        ] {
            assert!(!is_dist_tag(selector), "{selector}");
        }
    }

    #[test]
    fn npx_scoped_package_with_version_is_pinned() {
        let s = Source::from_command("npx", &args(&["-y", "@acme/mcp-github@1.4.2", "--stdio"]));
        assert_eq!(s.to_string(), "npm:@acme/mcp-github@1.4.2");
        assert!(s.pinned());
    }

    #[test]
    fn dist_tags_ranges_and_missing_versions_are_unpinned() {
        for spec in ["pkg", "pkg@latest", "pkg@next", "pkg@^1.2.0", "@s/pkg"] {
            let s = Source::from_command("npx", &args(&["-y", spec]));
            assert!(!s.pinned(), "{spec} should be unpinned");
        }
    }

    #[test]
    fn pnpm_dlx_bunx_and_package_flag_resolve_to_npm() {
        let s = Source::from_command("pnpm", &args(&["dlx", "srv@2.0.0"]));
        assert_eq!(s.to_string(), "npm:srv@2.0.0");
        let s = Source::from_command("/usr/bin/bunx", &args(&["srv"]));
        assert_eq!(s.to_string(), "npm:srv");
        let s = Source::from_command("npx", &args(&["-p", "@a/b@1.0.0", "b-server"]));
        assert_eq!(s.to_string(), "npm:@a/b@1.0.0");
    }

    #[test]
    fn uvx_and_docker_sources() {
        let s = Source::from_command("uvx", &args(&["mcp-server-git==0.6.2"]));
        assert_eq!(s.to_string(), "pypi:mcp-server-git==0.6.2");
        assert!(s.pinned());
        assert!(!Source::from_command("uvx", &args(&["mcp-server-git"])).pinned());

        let s = Source::from_command(
            "docker",
            &args(&["run", "-i", "--rm", "-e", "TOKEN", "ghcr.io/x/y@sha256:abc"]),
        );
        assert_eq!(s.to_string(), "oci:ghcr.io/x/y@sha256:abc");
        assert!(s.pinned());
        let s = Source::from_command("docker", &args(&["run", "-i", "ghcr.io/x/y:1.0"]));
        assert_eq!(s.to_string(), "oci:ghcr.io/x/y:1.0");
        assert!(!s.pinned());
    }

    #[test]
    fn other_commands_are_command_sources() {
        let s = Source::from_command("node", &args(&["server.js"]));
        assert_eq!(s.to_string(), "command:node");
    }

    #[test]
    fn parse_round_trips_and_rejects_url_unsafe_names() {
        for text in [
            "npm:@a/b@1.0.0",
            "npm:x",
            "pypi:x==1",
            "oci:img@sha256:00",
            "url:https://x",
        ] {
            assert_eq!(text.parse::<Source>().unwrap().to_string(), text);
        }
        assert!("npm:../etc@1.0.0".parse::<Source>().is_err());
        assert!("npm:x@1.0.0/../../y".parse::<Source>().is_err());
        assert!("ftp:x".parse::<Source>().is_err());
    }
}
