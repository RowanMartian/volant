//! Settings and the saved session, as small human-readable text files.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocking {
    Off,
    /// Volant's built-in tracker and ad list.
    Standard,
    /// Built-in list plus EasyList and EasyPrivacy, downloaded and compiled on your Mac.
    Strict,
}

impl Blocking {
    pub fn as_str(self) -> &'static str {
        match self {
            Blocking::Off => "off",
            Blocking::Standard => "standard",
            Blocking::Strict => "strict",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "off" => Blocking::Off,
            "strict" => Blocking::Strict,
            _ => Blocking::Standard,
        }
    }
}

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
    pub blocking: Blocking,
    pub appearance: Appearance,
    pub onboarded: bool,
    pub restore_session: bool,
    pub sidebar: bool,
    /// Background tabs untouched for this long are put to sleep to free memory. 0 = never.
    pub suspend_minutes: u32,
    /// When the strict lists were last downloaded.
    pub lists_updated: u64,
    /// Sites removed from the new tab page.
    pub hidden: Vec<String>,
    /// Reader text size in points, and "serif" or "sans".
    pub reader_size: u32,
    pub reader_font: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: crate::search::DEFAULT_ENGINE.into(),
            blocking: Blocking::Standard,
            appearance: Appearance::System,
            onboarded: false,
            restore_session: true,
            sidebar: true,
            suspend_minutes: 20,
            lists_updated: 0,
            hidden: Vec::new(),
            reader_size: 20,
            reader_font: "serif".into(),
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
                "blocking" => s.blocking = Blocking::parse(v),
                "appearance" => s.appearance = Appearance::parse(v),
                "onboarded" => s.onboarded = v == "1",
                "restore_session" => s.restore_session = v == "1",
                "sidebar" => s.sidebar = v == "1",
                "suspend_minutes" => s.suspend_minutes = v.parse().unwrap_or(s.suspend_minutes),
                "lists_updated" => s.lists_updated = v.parse().unwrap_or(0),
                "reader_size" => s.reader_size = v.parse::<u32>().map(|n| n.clamp(14, 30)).unwrap_or(s.reader_size),
                "reader_font" => s.reader_font = if v == "sans" { "sans".into() } else { "serif".into() },
                "hidden" => s.hidden = v.split(',').map(str::trim).filter(|h| !h.is_empty()).map(String::from).collect(),
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let b = |x: bool| if x { "1" } else { "0" };
        format!(
            "engine={}\nblocking={}\nappearance={}\nonboarded={}\nrestore_session={}\nsidebar={}\nsuspend_minutes={}\nlists_updated={}\nhidden={}\nreader_size={}\nreader_font={}\n",
            self.engine,
            self.blocking.as_str(),
            self.appearance.as_str(),
            b(self.onboarded),
            b(self.restore_session),
            b(self.sidebar),
            self.suspend_minutes,
            self.lists_updated,
            self.hidden.join(","),
            self.reader_size,
            self.reader_font
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
            || matches!(key, "engine" | "blocking" | "appearance" | "onboarded" | "restore_session" | "sidebar" | "suspend_minutes" | "reader_size" | "reader_font")
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
        assert!(s.set("blocking", "strict"));
        assert!(s.set("onboarded", "1"));
        let t = Settings::parse(&s.serialize());
        assert_eq!(t, s);
        assert_eq!(t.engine, "kagi");
        s.hidden = vec!["a.com".into(), "b.org".into()];
        assert_eq!(Settings::parse(&s.serialize()).hidden, s.hidden);
        assert!(!s.set("nonsense", "1"));
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
