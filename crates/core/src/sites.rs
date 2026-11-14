//! Which engine draws a site: Volant's own (Canard) or WebKit.
//!
//! Canard is the default for web pages. WebKit takes over for sites that
//! are whole applications (video, maps, office suites, chat), for sites the
//! person moved to WebKit themselves, and, for a while, for sites where Simple
//! Page was tried and the page came out empty or broken.
//!
//! Saved as `sites.txt`, one `host choice source until` line per site.

use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Simple,
    WebKit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The person chose (View → Canard on or off); kept until they change it.
    Person,
    /// Canard couldn't show the site; tried again after `until`.
    Learned,
}

/// How long a site that didn't work in Canard stays in WebKit before it's tried again.
pub const RETRY_AFTER: u64 = 14 * 24 * 3600;

/// Sites that are applications more than pages: they need WebKit's media,
/// WebGL, WebSockets and the full speed of a JIT.
const APPS: &[&str] = &[
    "youtube.com", "youtu.be", "netflix.com", "twitch.tv", "vimeo.com", "spotify.com", "music.apple.com", "tv.apple.com", "primevideo.com", "disneyplus.com", "hulu.com",
    "maps.google.com", "docs.google.com", "drive.google.com", "mail.google.com", "meet.google.com", "calendar.google.com", "sheets.google.com", "slides.google.com",
    "figma.com", "canva.com", "miro.com", "notion.so", "notion.site", "slack.com", "app.slack.com", "discord.com", "web.whatsapp.com", "teams.microsoft.com",
    "outlook.live.com", "outlook.office.com", "office.com", "onedrive.live.com", "icloud.com", "x.com", "twitter.com", "facebook.com", "instagram.com", "threads.net",
    "tiktok.com", "linkedin.com", "messenger.com", "zoom.us", "web.telegram.org", "chatgpt.com", "claude.ai", "colab.research.google.com", "codepen.io", "codesandbox.io",
    "stackblitz.com", "vscode.dev", "replit.com", "shadertoy.com", "earth.google.com", "photopea.com", "excalidraw.com",
];

#[derive(Default, Debug)]
pub struct Sites {
    map: HashMap<String, (Engine, Source, u64)>,
    pub dirty: bool,
}

/// The site a URL belongs to, for these choices: its host without `www.` (and without `m.`).
pub fn site(url: &str) -> String {
    let h = crate::omnibox::host(url).to_ascii_lowercase();
    h.strip_prefix("m.").map(str::to_string).unwrap_or(h)
}

/// Is this URL's site one that WebKit always draws (an application more than a page)?
pub fn is_app_url(url: &str) -> bool {
    is_app(&site(url))
}

fn is_app(site: &str) -> bool {
    APPS.iter().any(|a| site == *a || site.strip_suffix(a).map(|p| p.ends_with('.')).unwrap_or(false))
}

impl Sites {
    /// Which engine should draw `url`, if Canard is on by default (`default_simple`).
    pub fn choose(&self, url: &str, default_simple: bool, now: u64) -> Engine {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Engine::WebKit;
        }
        let s = site(url);
        match self.map.get(&s) {
            Some(&(e, Source::Person, _)) => return e,
            Some(&(e, Source::Learned, until)) if now < until => return e,
            _ => {}
        }
        if !default_simple || is_app(&s) {
            Engine::WebKit
        } else {
            Engine::Simple
        }
    }

    /// Did the person choose this site's engine themselves?
    pub fn chosen(&self, url: &str) -> Option<Engine> {
        match self.map.get(&site(url)) {
            Some(&(e, Source::Person, _)) => Some(e),
            _ => None,
        }
    }

    /// The person switched this site to `engine`.
    pub fn choose_for(&mut self, url: &str, engine: Engine) {
        let s = site(url);
        if s.is_empty() {
            return;
        }
        self.map.insert(s, (engine, Source::Person, 0));
        self.dirty = true;
    }

    /// Canard couldn't show this site: WebKit draws it for a while.
    pub fn failed(&mut self, url: &str, now: u64) {
        let s = site(url);
        if s.is_empty() || matches!(self.map.get(&s), Some((_, Source::Person, _))) {
            return;
        }
        self.map.insert(s, (Engine::WebKit, Source::Learned, now + RETRY_AFTER));
        self.dirty = true;
    }

    pub fn from_text(text: &str, now: u64) -> Sites {
        let mut map = HashMap::new();
        for line in text.lines() {
            let mut w = line.split_whitespace();
            let (Some(host), Some(engine), Some(source)) = (w.next(), w.next(), w.next()) else { continue };
            let until = w.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let engine = match engine {
                "simple" => Engine::Simple,
                "webkit" => Engine::WebKit,
                _ => continue,
            };
            let source = match source {
                "person" => Source::Person,
                "learned" if until > now => Source::Learned,
                _ => continue,
            };
            map.insert(host.to_string(), (engine, source, until));
        }
        Sites { map, dirty: false }
    }

    pub fn to_text(&self) -> String {
        let mut lines: Vec<String> = self
            .map
            .iter()
            .map(|(h, (e, s, until))| {
                let e = if *e == Engine::Simple { "simple" } else { "webkit" };
                match s {
                    Source::Person => format!("{h} {e} person"),
                    Source::Learned => format!("{h} {e} learned {until}"),
                }
            })
            .collect();
        lines.sort();
        lines.into_iter().map(|l| l + "\n").collect()
    }

    pub fn load(path: &Path) -> Sites {
        Sites::from_text(&std::fs::read_to_string(path).unwrap_or_default(), crate::now())
    }

    pub fn save(&mut self, path: &Path) {
        if self.dirty && std::fs::write(path, self.to_text()).is_ok() {
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices() {
        let mut s = Sites::default();
        assert_eq!(s.choose("https://en.wikipedia.org/wiki/Jet", true, 0), Engine::Simple);
        assert_eq!(s.choose("https://en.wikipedia.org/wiki/Jet", false, 0), Engine::WebKit);
        assert_eq!(s.choose("https://www.youtube.com/watch?v=1", true, 0), Engine::WebKit);
        assert_eq!(s.choose("https://m.youtube.com/", true, 0), Engine::WebKit);
        assert_eq!(s.choose("https://notyoutube.com/", true, 0), Engine::Simple);
        assert_eq!(s.choose("volant://home", true, 0), Engine::WebKit);
        // A failure sends the site to WebKit, until it's worth trying again.
        s.failed("https://app.example.com/x", 100);
        assert_eq!(s.choose("https://app.example.com/y", true, 200), Engine::WebKit);
        assert_eq!(s.choose("https://app.example.com/y", true, 100 + RETRY_AFTER), Engine::Simple);
        // The person's own choice wins, and isn't overruled by a failure.
        s.choose_for("https://www.youtube.com/", Engine::Simple);
        s.failed("https://youtube.com/", 0);
        assert_eq!(s.choose("https://youtube.com/feed", true, 0), Engine::Simple);
        assert_eq!(s.chosen("https://youtube.com/"), Some(Engine::Simple));
        let back = Sites::from_text(&s.to_text(), 200);
        assert_eq!(back.choose("https://app.example.com/", true, 200), Engine::WebKit);
        assert_eq!(back.choose("https://youtube.com/", true, 200), Engine::Simple);
        assert!(Sites::from_text(&s.to_text(), 100 + RETRY_AFTER).map.get("app.example.com").is_none());
    }
}
