# Volant

A native macOS browser in Rust. AppKit for the interface, the system WebKit for pages, and everything in
between written from scratch. Ad and tracker blocking is built in, powered by uBlock Origin's filter lists.

[![Build](https://github.com/RowanMartian/volant/actions/workflows/build-macos.yml/badge.svg)](https://github.com/RowanMartian/volant/actions/workflows/build-macos.yml)
&nbsp;[Website](https://rowanmartian.github.io/volant/) · [Download](https://github.com/RowanMartian/volant/releases/download/latest/Volant.dmg) · macOS 12+, universal (arm64 + x86_64) · MIT

| | Volant | Safari | Chrome | Firefox |
| --- | --- | --- | --- | --- |
| Launch to window | **0.30 s** | 3.64 s | 1.47 s | 2.31 s |
| App size | **2 MB** (1.3 MB download) | 29 MB | 732 MB | 521 MB |
| Memory, 5 pages | 320 MB | 169 MB | 515 MB | 821 MB |

<sub>Latest CI benchmark on GitHub's Apple Silicon runners (`scripts/bench/run.sh`): median of 3 launches after a warm-up,
memory footprint summed over every process each browser starts. Shared runners vary; across runs Volant launched in 0.29–1.04 s.</sub>

## Architecture

```
crates/core   volant-core: platform-independent, zero dependencies, builds and tests on any OS
crates/app    volant: the macOS shell (objc2 bindings to AppKit and WebKit)
assets/       Volant's own pages (volant://) and the scripts the blocker injects
```

**volant-core** (no dependencies)
- `omnibox`, `search`, `fuzzy`: address/search classification and the command bar's ranking.
- `history`: an append-only log plus an in-memory frecency index; under 1 ms per keystroke at 100k pages.
- `html`, `reader`: an HTML tokenizer/tree builder and article extraction (590 KB page: 4 ms parse, 8 ms extract).
- `ublock`: the filter compiler described below.
- `tabs`, `store`, `json`: the tab model with sleep and restore, settings and favourites as plain text, and a JSON writer.

**volant** (objc2, objc2-app-kit, objc2-web-kit, block2, dispatch2)
- A single `Browser` state on the main thread. AppKit and WebKit callbacks reach it through a re-entrancy-safe
  `with()`, which runs the closure immediately, or on the next run-loop turn if the state is already borrowed.
- The window is ordered front before WebKit is touched; the first tab's `WKWebView` is created on the next turn.
- One `WKWebViewConfiguration` is copied per tab, each tab with its own `WKUserContentController`.
- Background tabs idle past a threshold (20 min by default) drop their web view and keep only URL, title and history.
- In-place updater: polls the `latest` release, downloads `Volant-mac.zip`, `ditto -x`, `codesign --verify --deep --strict`,
  then swaps the bundle after quit and relaunches with the session.

## Blocking engine

Volant compiles uBlock Origin's lists (the uBO catalogue and default selection) into WebKit's native content blocker,
with no extension runtime.

1. **Preprocess** each list: `!#if` evaluated as uBO on Safari (`env_safari`, `ext_ublock`, `cap_user_stylesheet`).
2. **Network filters** → `WKContentRuleList` JSON: anchors, `$domain`, `$to`, resource types, 1p/3p, `match-case`,
   `$badfilter`, and regexes translated into WebKit's subset. Rules are sharded at 60k per list. Every shard gets all
   `ignore-previous-rules` exceptions, and `$important` blocks are placed after the exceptions, so they win as in uBO.
   `$redirect` becomes a block; `$removeparam`, `$csp`, `$header` and HTML filters are skipped.
3. **Cosmetic filters**: selectors WebKit accepts become `css-display-none` rules (generic ones batched, site ones
   grouped per domain). The rest go into a `SiteIndex` keyed by host and entity (`example.*`), together with
   procedural filters, scriptlets and `$generichide`/`$elemhide`/`$specifichide`/`$document` exceptions.
4. **Per navigation**, `webView:decidePolicyForNavigationAction:preferences:` sets the tab's rule lists (none on
   trusted sites), a document-start main-frame `WKUserScript` with that host's scriptlets and procedural filters, and
   `allowsContentJavaScript` for the no-script option.

uBO defaults (9 lists) produce 239,704 network rules, 30,201 cosmetic filters, 10,362 scriptlets and 1,332 procedural
filters across ~26.8k sites. On the CI Mac that takes about 1 s in Rust and about 19 s in WebKit, once; compiled
lists are then looked up by key at launch. `assets/ublock/` holds the page-side code: 33 scriptlets, a procedural
engine (`:has-text`, `:upward`, `:matches-css`, `:xpath`, `:remove`, `:style`, …) and the element picker.

uBlock Origin is by Raymond Hill and contributors (GPLv3); Volant is independent and not endorsed by them.

## Build

```bash
bash scripts/bundle-macos.sh
```

This builds a universal release binary (`lto = "fat"`, `codegen-units = 1`, `panic = "abort"`), compiles the Liquid
Glass icon (`macos/AppIcon.icon`) with `actool`, ad-hoc signs, and writes `dist/Volant.app`, `Volant.dmg` and
`Volant-mac.zip`.

```bash
cargo test -p volant-core
```

```bash
cargo run --release -p volant-core --example compile_lists -- easylist.txt easyprivacy.txt
```

`compile_lists` takes `OUT=dir` to write the rule lists and `LOOKUP=host` to print what a page on that host gets.
`bench` and `fuzz` are the other examples.

## CI

`.github/workflows/build-macos.yml`, on every push to `main`:

1. Run core tests, build the app, and render its icon as macOS draws it.
2. Smoke test: launch, browse, screenshot ten views, and record memory.
3. uBlock self-test: download the default lists, compile them into WebKit, then check network, generic and site
   cosmetic, procedural and scriptlet filtering on a live page, and check that trusting the site disables all of it.
4. Updater test: install the current release over a copy of this build and verify it relaunches.
5. Benchmark against Safari, Chrome and Firefox, then publish the `latest` release if this commit is still the tip.

The website is `site/` → `python3 scripts/build-site.py` → `docs/`, deployed by `.github/workflows/pages.yml`.

## Install

Open `Volant.dmg` and drag Volant to Applications. Volant isn't notarized, so the first launch needs
**System Settings → Privacy & Security → Open Anyway**, or:

```bash
xattr -cr /Applications/Volant.app
```

Data lives in `~/Library/Application Support/Volant` as plain text.
