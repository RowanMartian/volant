#!/usr/bin/env python3
"""Builds Volant's website: wraps each page in site/ with the shared head,
navigation and footer, and writes it to docs/ (what GitHub Pages serves).

    python3 scripts/build-site.py

A page starts with a comment of `key: value` lines (title, description, nav),
then its body. `{{icon:name}}` inserts one of the icons below and `{{dmg}}`
the download link.
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "site"
OUT = ROOT / "docs"
SITE = "https://rowanmartian.github.io/volant/"
DMG = "https://github.com/RowanMartian/volant/releases/download/latest/Volant.dmg"
REPO = "https://github.com/RowanMartian/volant"

NAV = [
    ("features", "features.html", "Features"),
    ("blocking", "blocking.html", "Blocking"),
    ("performance", "performance.html", "Performance"),
    ("help", "help.html", "Help"),
    ("download", "download.html", "Download"),
]


def svg(d, fill=False):
    style = 'fill="currentColor"' if fill else 'fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"'
    return f'<svg viewBox="0 0 24 24" {style} aria-hidden="true">{d}</svg>'


ICONS = {
    "download": '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 3v10M5.5 8.5 10 13l4.5-4.5M4 17h12"/></svg>',
    "shield": svg('<path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/>'),
    "shield-fill": svg('<path d="M12 2.5 3.5 5.7v6.1c0 5.3 3.7 8.6 8.5 9.7 4.8-1.1 8.5-4.4 8.5-9.7V5.7z"/>', fill=True),
    "reader": svg('<path d="M5 5h14M5 10h14M5 15h9M5 20h6"/>'),
    "moon": svg('<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>'),
    "search": svg('<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>'),
    "update": svg('<path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/>'),
    "lock": svg('<rect x="5" y="10" width="14" height="10" rx="2.5"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>'),
    "sidebar": svg('<rect x="3.5" y="4.5" width="17" height="15" rx="3"/><path d="M9.5 4.5v15"/>'),
    "star": svg('<path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8-4.3-4.1 5.9-.9z"/>'),
    "sun": svg('<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M4.6 4.6 6 6M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4"/>'),
    "find": svg('<path d="M4 6h10M4 12h6M4 18h6"/><circle cx="16" cy="15" r="3.5"/><path d="m18.6 17.6 2.4 2.4"/>'),
    "pin": svg('<path d="M9 4h6l-1 5 3 3v2H7v-2l3-3zM12 14v6"/>'),
    "box": svg('<path d="M12 4v10M7.5 9.5 12 14l4.5-4.5M5 19h14"/>'),
    "bolt": svg('<path d="M13 3 5 13.5h6L10 21l8-10.5h-6z"/>'),
    "picker": svg('<path d="M4 4l6.5 16 2.3-6.7L19.5 11z"/><path d="m13 13 6 6"/>'),
    "list": svg('<path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1"/><circle cx="4.5" cy="12" r="1"/><circle cx="4.5" cy="18" r="1"/>'),
    "code": svg('<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 5l-3 14"/>'),
    "check": svg('<path d="m5 12.5 4.5 4.5L19 7.5"/>'),
    "eye": svg('<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>'),
    "keyboard": svg('<rect x="2.5" y="6" width="19" height="12" rx="3"/><path d="M6.5 10h1M10.5 10h1M14.5 10h1M8 14h8"/>'),
    "github": '<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>',
}

HEAD = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<meta name="description" content="{description}">
<meta name="theme-color" content="#0d0c11">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Volant">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:image" content="{site}og.png">
<meta property="og:url" content="{site}{canonical}">
<link rel="canonical" href="{site}{canonical}">
<link rel="icon" href="logo.svg" type="image/svg+xml">
<link rel="stylesheet" href="site.css">
<link rel="stylesheet" href="motion.css">
<script src="brand.js" defer></script>
<script src="motion.js" defer></script>
</head>
<body>
"""

FOOTER = f"""<footer><div class="wrap">
  <div class="cols">
    <div>
      <div class="brand"><img src="logo.svg" alt="" width="26" height="26">Volant</div>
      <div>The featherweight browser for your Mac.<br>Free and open source, MIT licensed.</div>
    </div>
    <div><h4>Product</h4><ul><li><a href="features.html">Features</a></li><li><a href="blocking.html">Blocking</a></li><li><a href="performance.html">Performance</a></li><li><a href="download.html">Download</a></li></ul></div>
    <div><h4>Help</h4><ul><li><a href="help.html">Shortcuts and FAQ</a></li><li><a href="download.html#install">Installing</a></li><li><a href="privacy.html">Privacy</a></li></ul></div>
    <div><h4>Project</h4><ul><li><a href="{REPO}">Source on GitHub</a></li><li><a href="{REPO}/releases/tag/latest">Release notes</a></li><li><a href="{REPO}/issues">Report a problem</a></li></ul></div>
  </div>
  <p class="legal">Blocking is powered by uBlock: Volant uses uBlock Origin's filter lists and filter syntax. uBlock Origin is by Raymond Hill and contributors; Volant is an independent project, not made or endorsed by them. Mac, macOS, Safari and WebKit are trademarks of Apple Inc.</p>
</div></footer>
"""


def nav(current):
    links = "".join(
        f'<a href="{href}"{" aria-current=\"page\"" if key == current else ""}>{label}</a>' for key, href, label in NAV
    )
    return (
        '<nav><div class="wrap">\n'
        '  <a class="home" href="./"><img src="logo.svg" alt="" width="30" height="30">Volant</a>\n'
        f'  <div class="links">{links}<a href="{REPO}">GitHub</a></div>\n'
        f'  <a class="btn primary" href="{DMG}">Download</a>\n'
        "</div></nav>\n"
    )


def build(path):
    text = path.read_text()
    m = re.match(r"<!--(.*?)-->\n", text, re.S)
    meta = dict(line.split(":", 1) for line in m.group(1).strip().splitlines())
    meta = {k.strip(): v.strip() for k, v in meta.items()}
    body = text[m.end():]
    body = re.sub(r"\{\{icon:([a-z-]+)\}\}", lambda x: ICONS[x.group(1)], body)
    body = body.replace("{{dmg}}", DMG).replace("{{repo}}", REPO)
    name = path.name
    canonical = "" if name == "index.html" else name
    head = HEAD.format(title=meta["title"], description=meta["description"], site=SITE, canonical=canonical)
    scripts = ""
    if "<!-- scripts -->" in body:
        body, scripts = body.split("<!-- scripts -->", 1)
    page = head + nav(meta.get("nav", "")) + body.rstrip() + "\n\n" + FOOTER + scripts.strip() + ("\n" if scripts.strip() else "") + "</body>\n</html>\n"
    (OUT / name).write_text(page)
    return name


if __name__ == "__main__":
    built = [build(p) for p in sorted(SRC.glob("*.html"))]
    (OUT / ".nojekyll").write_text("")
    print("built", ", ".join(built))
