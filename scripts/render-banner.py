#!/usr/bin/env python3
"""Render banner/banner.html to assets/banner.png (2560x800, transparent rounded corners), or
with a version (`render-banner.py v0.5.0`) banner/releases/<version>.html to
assets/releases/<version>.png (1664x832), the hero image of that CHANGELOG entry, shown 770 wide.

Needs the Python playwright package and its Chromium. Commit the HTML and the PNG together.
"""

import sys
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    if len(sys.argv) > 1:
        version = sys.argv[1]
        source = ROOT / "banner" / "releases" / f"{version}.html"
        target = ROOT / "assets" / "releases" / f"{version}.png"
        height = 640
        scale = 1.3
    else:
        source = ROOT / "banner" / "banner.html"
        target = ROOT / "assets" / "banner.png"
        height = 400
        scale = 2
    if not source.is_file():
        sys.exit(f"render-banner: no {source.relative_to(ROOT)}")
    target.parent.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(
            viewport={"width": 1280, "height": height}, device_scale_factor=scale
        )
        page.goto(source.as_uri())
        page.wait_for_load_state("networkidle")
        page.locator(".bn").screenshot(path=str(target), omit_background=True)
        browser.close()
    print(f"wrote {target.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
