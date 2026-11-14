//! Settings and the saved session, as small human-readable text files.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub engine: String,
    /// Block ads and trackers (powered by uBlock).
    pub ublock: bool,
    /// The uBlock Origin filter lists that are on, by list id.
    pub ublock_lists: Vec<String>,
    /// uBlock Origin's switches: cosmetic filtering, remote fonts, JavaScript,
    /// hyperlink auditing (pings), WebRTC local IP leaks.
    pub ublock_cosmetic: bool,
    pub ublock_fonts: bool,
    pub ublock_noscript: bool,
    pub ublock_pings: bool,
    pub ublock_webrtc: bool,
    pub appearance: Appearance,
    pub onboarded: bool,
    pub restore_session: bool,
    pub sidebar: bool,
    /// Background tabs untouched for this long are put to sleep to free memory. 0 = never.
    pub suspend_minutes: u32,
    /// When the strict lists were last downloaded.
    pub lists_updated: u64,
    /// The weather widget on the new tab page, and where it's for.
    pub weather: bool,
    pub weather_place: String,
    pub weather_at: Option<(f64, f64)>,
    /// "c" or "f".
    pub weather_unit: String,
    /// Reader text size in points, and "serif" or "sans".
    pub reader_size: u32,
    pub reader_font: String,
    /// Experimental: Reader drawn by Volant's own engine (volant-engine) instead of WebKit.
    pub engine_reader: bool,
    /// Web pages open in Canard (Volant's own engine) when it handles the site well; WebKit otherwise.
    pub simple_default: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: crate::search::DEFAULT_ENGINE.into(),
            ublock: true,
            ublock_lists: crate::ublock::default_lists(),
            ublock_cosmetic: true,
            ublock_fonts: false,
            ublock_noscript: false,
            ublock_pings: true,
            ublock_webrtc: false,
            appearance: Appearance::System,
            onboarded: false,
            restore_session: true,
            sidebar: true,
            suspend_minutes: 20,
            lists_updated: 0,
            weather: false,
            weather_place: String::new(),
            weather_at: None,
            weather_unit: "c".into(),
            reader_size: 20,
            reader_font: "serif".into(),
            engine_reader: false,
            simple_default: true,
        }
    }
}

fn kv(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v.trim()))
}

/// Writes through a temporary file so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for (k, v) in kv(text) {
            match k {
                "engine" => s.engine = v.to_string(),
                // Volant before uBlock: "off" / "standard" / "strict".
                "blocking" => s.ublock = v != "off",
                "ublock" => s.ublock = v == "1",
                "ublock_lists" => s.ublock_lists = v.split(',').map(str::trim).filter(|x| crate::ublock::valid_list_id(x)).map(String::from).collect(),
                "ublock_cosmetic" => s.ublock_cosmetic = v == "1",
                "ublock_fonts" => s.ublock_fonts = v == "1",
                "ublock_noscript" => s.ublock_noscript = v == "1",
                "ublock_pings" => s.ublock_pings = v == "1",
                "ublock_webrtc" => s.ublock_webrtc = v == "1",
                "appearance" => s.appearance = Appearance::parse(v),
                "onboarded" => s.onboarded = v == "1",
                "restore_session" => s.restore_session = v == "1",
                "sidebar" => s.sidebar = v == "1",
                "suspend_minutes" => s.suspend_minutes = v.parse().unwrap_or(s.suspend_minutes),
                "lists_updated" => s.lists_updated = v.parse().unwrap_or(0),
                "reader_size" => s.reader_size = v.parse::<u32>().map(|n| n.clamp(14, 30)).unwrap_or(s.reader_size),
                "engine_reader" => s.engine_reader = v == "1",
                "simple_default" => s.simple_default = v == "1",
                "reader_font" => s.reader_font = if v == "sans" { "sans".into() } else { "serif".into() },
                "weather" => s.weather = v == "1",
                "weather_place" => s.weather_place = v.to_string(),
                "weather_at" => {
                    s.weather_at = v.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))).filter(|(a, b)| a.abs() <= 90.0 && b.abs() <= 180.0)
                }
                "weather_unit" => s.weather_unit = if v == "f" { "f".into() } else { "c".into() },
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let b = |x: bool| if x { "1" } else { "0" };
        format!(
            "engine={}\nublock={}\nublock_lists={}\nublock_cosmetic={}\nublock_fonts={}\nublock_noscript={}\nublock_pings={}\nublock_webrtc={}\nappearance={}\nonboarded={}\nrestore_session={}\nsidebar={}\nsuspend_minutes={}\nlists_updated={}\nreader_size={}\nreader_font={}\nengine_reader={}\nsimple_default={}\nweather={}\nweather_place={}\nweather_at={}\nweather_unit={}\n",
            self.engine,
            b(self.ublock),
            self.ublock_lists.join(","),
            b(self.ublock_cosmetic),
            b(self.ublock_fonts),
            b(self.ublock_noscript),
            b(self.ublock_pings),
            b(self.ublock_webrtc),
            self.appearance.as_str(),
            b(self.onboarded),
            b(self.restore_session),
            b(self.sidebar),
            self.suspend_minutes,
            self.lists_updated,
            self.reader_size,
            self.reader_font,
            b(self.engine_reader),
            b(self.simple_default),
            b(self.weather),
            self.weather_place,
            self.weather_at.map(|(a, o)| format!("{a},{o}")).unwrap_or_default(),
            self.weather_unit
        )
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let _ = write_atomic(path, &self.serialize());
    }

    /// Applies one `key=value` change coming from the Settings page. Returns false for unknown keys.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let before = self.clone();
        *self = Self::parse(&format!("{}{key}={value}\n", self.serialize()));
        *self != before
            || key.starts_with("ublock")
            || matches!(key, "engine" | "blocking" | "appearance" | "onboarded" | "restore_session" | "sidebar" | "suspend_minutes" | "reader_size" | "reader_font" | "engine_reader" | "simple_default" | "weather" | "weather_place" | "weather_at" | "weather_unit")
    }
}

/// A site pinned to the new tab page.
#[derive(Clone, Debug, PartialEq)]
pub struct Favorite {
    pub url: String,
    pub title: String,
}

/// The new tab page's favourite sites, in the order you arranged them.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Favorites {
    pub list: Vec<Favorite>,
}

pub const MAX_FAVORITES: usize = 24;

impl Favorites {
    pub fn parse(text: &str) -> Self {
        let list = text
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(u, t)| (u, t)).or(Some((l, ""))))
            .filter(|(u, _)| u.starts_with("http://") || u.starts_with("https://"))
            .take(MAX_FAVORITES)
            .map(|(u, t)| Favorite { url: u.to_string(), title: t.to_string() })
            .collect();
        Self { list }
    }

    pub fn serialize(&self) -> String {
        self.list.iter().map(|f| format!("{}\t{}\n", clean(&f.url), clean(&f.title))).collect()
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let _ = write_atomic(path, &self.serialize());
    }

    /// Adds a site (typed as `example.com` or a full address). Returns false if it isn't one, it's there already, or the list is full.
    pub fn add(&mut self, typed: &str, title: &str) -> bool {
        let Some(url) = crate::omnibox::as_url(typed).filter(|u| u.starts_with("http")) else { return false };
        let key = |u: &str| u.trim_end_matches('/').to_ascii_lowercase();
        if self.list.len() >= MAX_FAVORITES || self.list.iter().any(|f| key(&f.url) == key(&url)) {
            return false;
        }
        let title = clean(title.trim());
        let title = if title.is_empty() { crate::omnibox::host(&url).to_string() } else { title };
        self.list.push(Favorite { url, title });
        true
    }

    pub fn remove(&mut self, url: &str) {
        self.list.retain(|f| f.url != url);
    }

    /// Moves the favourite at `from` to position `to`.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from < self.list.len() {
            let f = self.list.remove(from);
            self.list.insert(to.min(self.list.len()), f);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

impl Session {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let mut f = line.split('\t');
            match f.next() {
                Some("tab") => {
                    let pinned = f.next() == Some("1");
                    let url = f.next().unwrap_or("").to_string();
                    let title = f.next().unwrap_or("").to_string();
                    if !url.is_empty() {
                        s.tabs.push(SavedTab { url, title, pinned });
                    }
                }
                Some("active") => s.active = f.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                _ => {}
            }
        }
        if s.active >= s.tabs.len() {
            s.active = 0;
        }
        s
    }

    pub fn serialize(&self) -> String {
        let mut o = String::new();
        for t in &self.tabs {
            o.push_str(&format!("tab\t{}\t{}\t{}\n", if t.pinned { 1 } else { 0 }, clean(&t.url), clean(&t.title)));
        }
        o.push_str(&format!("active\t{}\n", self.active));
        o
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let _ = write_atomic(path, &self.serialize());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_round_trip() {
        let mut s = Settings::default();
        assert!(s.set("engine", "kagi"));
        assert!(s.set("ublock", "0"));
        assert!(s.set("ublock_lists", "easylist,not a list!,easyprivacy"));
        assert!(s.set("onboarded", "1"));
        let t = Settings::parse(&s.serialize());
        assert_eq!(t, s);
        assert_eq!(t.engine, "kagi");
        assert!(!t.ublock);
        assert_eq!(t.ublock_lists, ["easylist", "easyprivacy"]);
        assert!(Settings::parse("blocking=off\n").ublock == false && Settings::parse("blocking=strict\n").ublock, "old settings carry over");
        assert_eq!(Settings::parse("").ublock_lists, crate::ublock::default_lists());
        assert!(s.set("weather_at", "43.65,-79.38"));
        assert!(s.set("weather_place", "Toronto, Ontario"));
        let t = Settings::parse(&s.serialize());
        assert_eq!((t.weather_at, t.weather_place.as_str()), (Some((43.65, -79.38)), "Toronto, Ontario"));
        s.set("weather_at", "999,1");
        assert_eq!(s.weather_at, None, "out-of-range coordinates are dropped");
        assert!(!s.set("nonsense", "1"));
    }
    #[test]
    fn favorites() {
        let mut f = Favorites::default();
        assert!(f.add("github.com", ""));
        assert!(f.add("https://news.ycombinator.com/", "HN"));
        assert!(!f.add("github.com/", ""), "no duplicates");
        assert!(!f.add("not a site", ""));
        assert_eq!(f.list[0].title, "github.com");
        f.reorder(1, 0);
        assert_eq!(f.list[0].title, "HN");
        let g = Favorites::parse(&f.serialize());
        assert_eq!(g, f);
        f.remove("https://github.com");
        assert_eq!(f.list.len(), 1);
    }

    #[test]
    fn session_round_trip() {
        let s = Session {
            tabs: vec![
                SavedTab { url: "https://a.com/".into(), title: "A\tB".into(), pinned: true },
                SavedTab { url: "https://b.com/".into(), title: "".into(), pinned: false },
            ],
            active: 1,
        };
        let t = Session::parse(&s.serialize());
        assert_eq!(t.tabs.len(), 2);
        assert_eq!(t.tabs[0].title, "A B");
        assert!(t.tabs[0].pinned);
        assert_eq!(t.active, 1);
    }
}
