<p align="center"><img src="assets/logo.svg" width="96" alt="Volant"></p>
<h1 align="center">Volant</h1>
<p align="center">A featherweight web browser for your Mac. Native, private, and out of your way.</p>

---

- **Tiny.** The whole app is a few megabytes. No bundled browser engine: pages render in the WebKit that already ships with macOS.
- **Native.** The window, sidebar, command bar and menus are AppKit, driven straight from Rust. No Electron, no web-based UI.
- **Ads and trackers blocked inside the engine.** Filter lists are compiled once into WebKit content rules, which run as bytecode in WebKit's own networking code. No blocker scripts run on any page.
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
              omnibox (URL vs search), history (append-only log + frecency),
              fuzzy matching, filter-list → WebKit rule compiler, tab model,
              settings/session storage, a minimal JSON writer.
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
