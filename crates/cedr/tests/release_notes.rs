//! Every release's notes and hero follow the format AGENTS.md sets. New banners get copied from the
//! last one, so a drift in one spreads to all that follow unless something notices.

use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
}

fn entries() -> Vec<(String, String)> {
    let changelog = std::fs::read_to_string(root().join("CHANGELOG.md")).unwrap();
    changelog
        .split("\n## v")
        .skip(1)
        .map(|e| {
            let (head, body) = e.split_once('\n').unwrap();
            (
                head.split(' ').next().unwrap().to_string(),
                body.to_string(),
            )
        })
        .collect()
}

#[test]
fn every_hero_is_embedded_at_its_reading_width_and_uses_the_version_ring() {
    for (version, body) in entries() {
        let banner = root().join(format!("banner/releases/v{version}.html"));
        if !banner.exists() {
            continue;
        }
        let image = format!("/assets/releases/v{version}.png\"");
        assert!(
            body.lines().any(|l| l.starts_with("<img ")
                && l.contains(&image)
                && l.contains("width=\"770\"")),
            "v{version} must embed its hero at width 770, or it shows full size"
        );
        let html = std::fs::read_to_string(&banner).unwrap();
        assert!(
            html.contains(".ver::before"),
            "v{version} lost the version ring"
        );
        assert!(
            html.contains(" at 1.3x"),
            "v{version} must render at 1664x832"
        );
        let title = html
            .split("<h1")
            .nth(1)
            .and_then(|h| h.split_once('>'))
            .and_then(|(_, t)| t.split('<').next())
            .unwrap_or_else(|| panic!("v{version} has no title"));
        assert_eq!(
            title,
            title.to_lowercase(),
            "v{version} title must be lowercase"
        );
        assert!(
            html.matches("<span class=\"p\">$</span>").count() <= 3,
            "v{version} may show at most three commands, or the last is cut off"
        );
    }
}

#[test]
fn notes_count_their_changes_and_lead_with_a_bold_highlight() {
    for (version, body) in entries() {
        if !version.starts_with("0.8.") {
            continue;
        }
        let said: usize = body
            .split(&format!("**v{version}** · "))
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("v{version} has no change count"));
        let listed = body.lines().filter(|l| l.starts_with("* ")).count();
        assert_eq!(said, listed, "v{version}");
        let highlight = body
            .split("### Highlights\n")
            .nth(1)
            .unwrap_or_else(|| panic!("v{version} has no Highlights"));
        assert!(
            highlight.starts_with("**"),
            "v{version} must lead with a bold line"
        );
    }
}
