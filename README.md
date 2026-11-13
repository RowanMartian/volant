# Volant

A native macOS browser in Rust. AppKit for the interface, its own engine for the pages it handles well
(Simple Page), the system WebKit for web apps and as the fallback, and everything in between written from scratch. Ad and tracker blocking is built in, powered by uBlock Origin's filter lists.

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
crates/engine volant-engine: Volant's own web engine (CSS, style, layout, paint), built chunk by chunk
crates/app    volant: the macOS shell (objc2 bindings to AppKit and WebKit)
crates/tools  volant-tools: website builder, lipo and zip, used by the build (depends only on volant-core)
assets/       Volant's own pages (volant://) and the scripts the blocker injects
```

**volant-core** (no dependencies)
- `omnibox`, `search`, `fuzzy`: address/search classification and the command bar's ranking.
- `history`: an append-only log plus an in-memory frecency index; under 1 ms per keystroke at 100k pages.
- `html`, `reader`: an HTML tokenizer/tree builder and article extraction (590 KB page: 4 ms parse, 8 ms extract).
- `ublock`: the filter compiler described below.
- `tabs`, `store`, `json`: the tab model with sleep and restore, settings and favourites as plain text, JSON in and out.
- `http`: an HTTP/1.1 client over a pluggable transport: redirects (never HTTPS→HTTP), chunked, gzip, 1xx, size limits,
  deadlines, conditional requests.
- `deflate`, `hash`, `zip`, `macho`: inflate and deflate (LZ77 with lazy matching, package-merge length-limited
  Huffman), gzip, SHA-256, CRC-32, zip read/write with path-escape checks, and universal Mach-O assembly.
- `favicon`, `scriptlets`: icon discovery for favourites, and uBO's scriptlets assembled per page.

**volant** (objc2, objc2-app-kit, objc2-web-kit, block2, dispatch2)
- A single `Browser` state on the main thread. AppKit and WebKit callbacks reach it through a re-entrancy-safe
  `with()`, which runs the closure immediately, or on the next run-loop turn if the state is already borrowed.
- The window is ordered front before WebKit is touched; the first tab's `WKWebView` is created on the next turn.
- One `WKWebViewConfiguration` is copied per tab, each tab with its own `WKUserContentController`.
- Background tabs idle past a threshold (20 min by default) drop their web view and keep only URL, title and history.
- Networking (`net.rs`): volant-core's HTTP over TLS connections from the Network framework (`nw_connection`, called
  through hand-written C bindings), so certificate validation and encryption stay the system's.
- In-place updater: reads the `latest` release from GitHub's API, downloads `Volant-mac.zip`, checks it against the
  SHA-256 GitHub publishes for the asset, unpacks it in Rust, then re-runs its own binary as `--finish-update` to swap
  the bundle after quit and relaunch with the session. No `ditto`, `codesign` or shell.

## Volant's own engine

`crates/engine` is a web engine written from scratch, growing in chunks. It draws ordinary web pages by default;
WebKit draws web apps and anything it can't show yet.
Today: CSS parsing (selectors with descendant/child/sibling combinators, attribute operators, `:nth-*`, `:not()`,
`:is()`, `::before`/`::after` with `content`, `!important`, `@media` width/colour-scheme queries, `@supports`), custom
properties and `var()`, `calc()`/`min()`/`max()`/`clamp()`, the cascade with a user-agent sheet and a rule index
(by id, class and tag), inheritance and `em`/`rem`/`%` units; flexbox (direction, wrap, grow/shrink/basis, justify,
align, gaps, auto margins, order) and grid (track lists, `fr`, `repeat()`, auto-fill, areas, spans, explicit columns);
`position` relative/absolute/fixed against positioned ancestors, and the visually-hidden pattern; block layout
(margins with sibling collapsing, padding, borders, `width`/`max-width`, auto-margin centring) and inline layout
(whitespace collapsing, `pre`/`pre-wrap`/`nowrap`, wrapping with long-word breaking, baselines from font metrics,
`vertical-align`, `text-align`, list markers, links) with real inline boxes (padding, backgrounds, radius); images as
replaced elements (CSS size, attributes, intrinsic ratio, `max-width`); floats with text wrapping around them and
`clear`; tables (row groups, `colspan`, min/max-content column sizing, equal-height rows, captions); a display list
of rectangles, text and images. The app measures and draws with the system fonts, loads pictures over Volant's own
HTTP, searches the laid-out text for Find on page, and selects and copies text (drag, double/triple-click, shift-click,
⌘A, ⌘C, keyboard scrolling) through `select.rs`, which maps points to characters and rebuilds plain text with the right
line and paragraph breaks (`engine_view.rs`). `volant-tools render page.html out.svg
[--reader URL]` draws to SVG anywhere.

Since the latest chunk it also has `opacity`, `overflow` clipping, `transform: translate`, real inline-blocks
(shrink-to-fit, on the text baseline), drawn form controls (text fields, checkboxes, radios, selects, buttons), the
old presentational attributes (`bgcolor`, `width="85%"`, `cellpadding`) and hit boxes for every element.

**Page scripts** (`script.rs`) connect the engine to Volant's JavaScript engine: a live DOM with wrapper objects
(traversal and mutation, attributes, `innerHTML`/`outerHTML`, `querySelector` with the engine's own selector matching,
`classList`, `style`, `dataset`), events with capture, bubbling, `once` and inline handlers, focus, typing, checkboxes,
native `<select>` menus, a text caret with arrow keys, `:hover`/`:focus`/`:focus-within`/`:target`, same-page
`#anchor` scrolling, GET and POST forms (URL-encoded, multipart, text) and link activation, `<canvas>` 2D drawing
(paths, arcs, curves, text, images, transforms) recorded as a display list the app draws natively, scripts inserted by scripts, `document.write`, and a `getComputedStyle`
backed by the real cascade. `prelude.js` builds the rest of the platform in JavaScript on a few native hooks: `URL`,
`URLSearchParams`, `fetch`, `XMLHttpRequest`, `Headers`/`Response`, storage, observers, `FormData`, `atob`/`btoa`,
`TextEncoder` and more. jQuery and MediaWiki's module loader run on it.

**Simple Page is the default** for web pages (Settings → Experimental → Open pages in Simple Page). `sites.rs` in
volant-core picks the engine per site: WebKit for web apps (video, maps, office suites, chat, social apps), for
sites the person switched with **View → Simple Page (⌥⌘E)** (remembered in `sites.txt`), and for two weeks after
Simple Page couldn't show a site. A page falls back to WebKit when the fetch fails, when it isn't HTML, when it
redirects to a web app, or when `script::assess` finds it came out empty, framed, plug-in only or broken by script
errors. Only one Simple Page runs at a time; other Simple Page tabs keep their history and reload when brought to
the front. Cookies (`cookies.rs`) and history are Volant's own, and WebKit doesn't load the page as well.
It's also used for an opt-in Reader. Simple Page shows a page with nothing but Volant: a session thread fetches the HTML, scripts and style sheets over Volant's own HTTP,
runs the page's JavaScript, and sends snapshots to be laid out and drawn natively (a first paint before the scripts,
then after them and after every change). Clicks are hit-tested and sent to the page's scripts, typing goes to its
focused field, and links and forms navigate the tab, so history works and the next page opens in the engine too.
`volant-tools render page.html out.svg [--scripts URL] [--eval JS]` does the same anywhere.

## JavaScript engine

`crates/js` (volant-js) is Volant's own JavaScript engine, from scratch with no dependencies beyond volant-core. A
lexer with automatic semicolon insertion, template and regex literals feeds a recursive-descent parser, and a
compiler to bytecode (variables resolved to slots ahead of time, property lookups cached) feeds a stack VM: closures and hoisting, prototypes, `this`, `new`, classes (`extends`,
`super`, getters and setters, instance and static fields), destructuring, spread and rest, optional chaining,
labelled loops, `switch`, exceptions, generators, `async`/`await` and `new Function`/`eval`. Its own **regular
expression engine** (`regex.rs`: a backtracking matcher with groups, named groups, lookaround, back-references,
classes, `\p{…}` and the `gimsuy` flags) backs `RegExp` and the string methods. **Promises** run on a microtask
queue, and timers on a virtual clock; `await` runs the event loop in place until its promise settles. The standard
library covers Object, Function, Array, String, Number, Boolean, Math, JSON, the Error types, Map, Set, Date,
Symbol and iterators. Scripts run under a step budget and a recursion limit, so a page can't hang the browser.
ES modules (static `import`/`export` and dynamic `import()`) load through the page's host. `volant-tools js
file.js` runs a script. Not yet: Proxy, typed arrays (plain arrays stand in), and binary data.

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
lists are then looked up by key at launch. `assets/ublock/` holds the page-side code: a procedural engine
(`:has-text`, `:upward`, `:matches-css`, `:xpath`, `:remove`, `:style`, …), the element picker, and 33 fallback scriptlets.

**Staying current with uBO.** Lists refresh when the soonest `! Expires:` among them passes (clamped to 12 h–4 days),
trying each list's uBO CDN mirrors before its origin. Each refresh first asks jsDelivr for uBO's latest release; when it
changes, a hidden `WKWebView` (non-persistent store) imports that release's `src/js/resources/scriptlets.js` and
`assets/assets.json` and returns them flattened (`crates/app/src/mac/ubo.rs`). Volant then runs uBO's own scriptlets,
assembled per page as uBO does (function plus its `.fn` dependencies, `scriptletGlobals`, one `try` per call), with
`ISOLATED` ones in a separate `WKContentWorld` and `trusted-*` ones only from `ublock-*` lists. The list catalogue,
groups and mirrors come from the same release.

uBlock Origin is by Raymond Hill and contributors (GPLv3); Volant is independent and not endorsed by them.

## Build

```bash
bash scripts/bundle-macos.sh
```

This builds a universal release binary (`lto = "fat"`, `codegen-units = 1`, `panic = "abort"`; joined by
`volant-tools lipo`), compiles the Liquid Glass icon (`macos/AppIcon.icon`) with `actool`, ad-hoc signs, and writes
`dist/Volant.app`, `Volant.dmg` (`hdiutil`) and `Volant-mac.zip` (`volant-tools zip`, then checked with Apple's
`ditto` and `codesign`).

```bash
cargo test -p volant-core -p volant-tools
```

```bash
cargo run -p volant-tools -- site
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
3. uBlock self-test: sync with uBO's latest release, download the default lists, compile them into WebKit, then check
   network, generic and site cosmetic, procedural, and uBO scriptlets in the page and isolated worlds on a live page,
   and check that trusting the site disables all of it.
4. Updater test: install the current release over a copy of this build and verify it relaunches.
5. Benchmark against Safari, Chrome and Firefox, then publish the `latest` release if this commit is still the tip.

The website is `site/` → `cargo run -p volant-tools -- site` → `docs/`, deployed by `.github/workflows/pages.yml`.

## Install

Open `Volant.dmg` and drag Volant to Applications. Volant isn't notarized, so the first launch needs
**System Settings → Privacy & Security → Open Anyway**, or:

```bash
xattr -cr /Applications/Volant.app
```

Data lives in `~/Library/Application Support/Volant` as plain text.
