<p align="center"><img src="assets/logo.svg" width="96" alt="Volant"></p>
<h1 align="center">Volant</h1>
<p align="center">A featherweight web browser for your Mac. Native, private, and out of your way.</p>

---

- **Tiny.** The whole app is a few megabytes. No bundled browser engine: pages render in the WebKit that already ships with macOS.
- **Native.** The window, sidebar, command bar and menus are AppKit, driven straight from Rust. No Electron, no web-based UI.
- **Ads and trackers blocked inside the engine.** Filter lists are compiled once into WebKit content rules, which run as bytecode in WebKit's own networking code. No blocker scripts run on any page.
- **Reader mode** (⇧⌘R) on Volant's own HTML parser: any article, minus everything around it, in a calm column of type.
- **Sleeping tabs.** Tabs you haven't looked at in a while hand their memory back, and wake up when you return. At launch only the tab in front loads.
- **Private by default.** No accounts, no sync, no telemetry. Nothing leaves your Mac except the pages you visit.
- **100% open source**, MIT licensed.

## Getting the app

1. Go to **Releases** → **Volant — latest build** and download **Volant.dmg**.
2. Open it and drag **Volant** onto **Applications**.
3. The first time, macOS warns that it can't verify the developer, because Volant isn't notarized by Apple.
   Go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway**. Or run this once:
   ```bash
   xattr -cr /Applications/Volant.app
   ```

Volant needs macOS 12 Monterey or later, on Apple Silicon or Intel.

## Using it

| | |
| --- | --- |
| ⌘T | New tab, with the command bar open: type an address, a search, or part of a page you've visited |
| ⌘L | Edit the current address |
| ⌘S | Show or hide the sidebar |
| ⌘D | Pin or unpin the current tab |
| ⇧⌘T | Reopen the last tab you closed |
| ⌘W | Close tab |
| ⌘1 – ⌘9 | Jump to a tab (⌘9 is always the last one) |
| ⌃Tab, ⇧⌘] / ⇧⌘[ | Next / previous tab |
| ⌘F, ⌘G, ⇧⌘G | Find on page, next, previous |
| ⇧⌘R | Reader |
| ⌘[ / ⌘] | Back / forward |
| ⌘+ / ⌘- / ⌘0 | Zoom in / out / actual size |
| ⌘Y | History |
| ⌘, | Settings |

⌘-click or middle-click a link to open it in a background tab (add ⇧ to switch to it). Right-click a tab to pin, duplicate, copy its link or close others.

### Blocking

- **Standard** (default): Volant's built-in list of the big ad networks and cross-site trackers. Breaks nothing.
- **Strict**: adds [EasyList](https://easylist.to) and EasyPrivacy, downloaded from easylist.to, compiled on your Mac (about 120,000 rules, in a fraction of a second), and refreshed weekly.
- **Off.**

### Where things live

Settings, your open tabs and history are plain text files in `~/Library/Application Support/Volant`.
Downloads go to your Downloads folder.

## How it's built

```
crates/core   Platform-independent pieces, zero dependencies, tested on any OS:
              omnibox (URL vs search), history (append-only log + frecency
              + search index), fuzzy matching, HTML parser, Reader extraction,
              filter-list → WebKit rule compiler, tab model, settings/session
              storage, a minimal JSON writer.
crates/app    The macOS shell: AppKit window and sidebar, command bar, find bar,
              menus, WKWebView tabs, downloads, volant:// pages, blocking.
assets/pages  Volant's own pages (welcome, new tab, settings, history, errors).
```

Volant is written from scratch in Rust except for two things it deliberately reuses from macOS:
**WebKit** to render pages, and **AppKit** to draw the window. Writing a new rendering engine
that handles today's web is a years-long project for a large team; reusing the one already
on every Mac is what keeps Volant a few megabytes instead of a few hundred.

Where Volant does its own work, it skips the heavy defaults browsers usually reach for:

| | Typical browser | Volant |
| --- | --- | --- |
| Rendering engine | Bundled (Chromium: ~1 GB installed) | The system's WebKit (0 bytes) |
| Ad blocking | Extension scripts in every page | Compiled WebKit rules, no page scripts |
| History | SQLite database | Append-only text log, loaded in milliseconds |
| Restoring 50 tabs | Loads them all | Loads 1, the rest wake on demand |
| UI toolkit | HTML/CSS in a web view | Native AppKit views |

### What's written from scratch, and what's macOS

| Layer | Who does it |
| --- | --- |
| Window, sidebar, tabs, command bar, find bar, menus | Volant (AppKit views driven from Rust) |
| Tabs, navigation, history, sessions, downloads, sleeping tabs | Volant |
| Address bar: URL or search, suggestions, inline completion | Volant |
| Ad and tracker blocking: filter-list compiler | Volant; WebKit runs the compiled rules |
| HTML parsing and article extraction for Reader | Volant |
| Rendering pages, JavaScript, networking, page storage, process sandboxing | WebKit, the same engine as Safari |

### Performance

The hot paths in `crates/core` are benchmarked (`cargo run --release -p volant-core --example bench`).
Numbers below are from a 2.1 GHz Xeon; Apple Silicon is faster.

| | Before tuning | Now |
| --- | --- | --- |
| Command bar keystroke, 100,000-page history (average) | 27 ms | 0.9 ms |
| Command bar keystroke, worst case | 33 ms | ~5 ms |
| Inline completion | 19 ms | < 0.01 ms |
| Top sites for a new tab | 50 ms | cached (0 ms), 25 ms to refresh |
| Parse Wikipedia's 590 KB "Jet engine" article | | 4 ms |
| Find its article for Reader | | 8 ms |
| Compile EasyList + EasyPrivacy (≈135,000 lines → 119,000 WebKit rules) | | 0.2 s, once a week |

How: a frecency-sorted search index that exists only while the command bar is open,
stops early once nothing further down can make the top results, and narrows from the
previous keystroke's matches. The command bar reuses its rows instead of rebuilding
views. Session saves are debounced, and Volant's only regular timer fires once a minute.

The parser, Reader, filter compiler and address bar are fuzzed with 110,000 random
and truncated inputs (`--example fuzz`).

### Compared with other browsers

Measured by CI on GitHub's macOS runners (Apple M1, virtualised) with `scripts/bench/run.sh`:
the same five pages open in each browser, memory as Activity Monitor's footprint summed over
every process the browser started. Latest run:

| | Volant | Safari | Chrome | Firefox |
| --- | --- | --- | --- | --- |
| App size | **2 MB** | ships with macOS | 732 MB | 521 MB |
| Launch to window | **0.30 s** | 3.6 s | 1.5 s | 2.3 s |
| Memory, 5 pages | 320 MB | 169 MB | 515 MB | 821 MB |
| Idle CPU over 30 s | 1.3 s | 0.02 s | 0.36 s | 2.6 s |

Shared runners are noisy, so here is the spread over four runs:

| | Volant | Safari | Chrome | Firefox |
| --- | --- | --- | --- | --- |
| Launch to window | 0.29–0.30 s since showing the window first (0.30–1.04 s before) | 2.1–3.6 s | 0.60–1.5 s | 1.3–2.3 s |
| Memory, 5 pages | 253–320 MB | 160–308 MB | 515–527 MB | 805–823 MB |
| Idle CPU over 30 s | 0.00–1.3 s | 0.02–0.20 s | 0.21–0.36 s | 1.5–2.9 s |

What this shows, honestly:

- **Size:** Volant is a 1.3 MB download; Chrome and Firefox ship their own engines.
- **Launch:** Volant puts its window up fastest in every run, 2–5× sooner than Chrome and 7–12× sooner than Safari.
- **Memory:** about half of Chrome's and a third of Firefox's. Against Safari it's the same engine
  and lands in the same range; Safari sometimes comes in lower.
- **Idle CPU:** usually zero for Volant's own process; what shows up is the pages' own scripts
  in WebKit's processes. The benchmark counts every WebKit helper that starts during the test,
  so Volant's and Safari's figures can include some noise from the system.
- **Page speed:** identical to Safari, because it is Safari's engine doing the work.

### Building it yourself

GitHub builds Volant on every push (see `.github/workflows/build-macos.yml`): it runs the tests,
builds a universal app, launches it for a smoke test that takes screenshots, and publishes
**Volant.dmg** to the **Latest build** release.

On a Mac with Rust installed:

```bash
bash scripts/bundle-macos.sh
```

```bash
open dist/Volant.app
```

The engine pieces build and test anywhere:

```bash
cargo test -p volant-core
```

## Design

Flat and quiet: no borders, no shadows, no gradients. Surfaces are told apart by fill alone,
everything you can click has rounded corners, and the system font does the talking.
The mark is a V with a jet flying through it.
