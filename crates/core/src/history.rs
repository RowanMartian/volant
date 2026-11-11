//! Browsing history as an append-only text log.
//!
//! Browsers usually keep history in SQLite. Volant writes one short line per
//! visit to a log file and keeps everything in memory, which makes recording a
//! visit a single `write` and lets the whole history load in a few milliseconds.
//! The log is compacted on startup once it has grown well past the number of pages.

use crate::fuzzy;
use crate::omnibox;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub visits: u32,
    pub last: u64,
}

impl Page {
    /// Frecency: how often and how recently you've been here.
    pub fn frecency(&self, now: u64) -> u32 {
        let age_days = now.saturating_sub(self.last) / 86_400;
        let recency = match age_days {
            0 => 100,
            1..=3 => 70,
            4..=13 => 50,
            14..=30 => 30,
            31..=90 => 10,
            _ => 4,
        };
        recency * (1 + self.visits.min(40))
    }
}

#[derive(Default)]
pub struct History {
    pages: Vec<Page>,
    index: HashMap<String, usize>,
    log: Option<File>,
    path: Option<PathBuf>,
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

fn recordable(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() < 4096
}

impl History {
    /// An in-memory history that is never written anywhere (private windows, tests).
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// Loads history from `path`, compacting the log when it has grown large.
    pub fn open(path: &Path) -> Self {
        let mut h = Self { path: Some(path.to_path_buf()), ..Default::default() };
        let mut lines = 0usize;
        if let Ok(f) = File::open(path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                lines += 1;
                h.apply(&line);
            }
        }
        if lines > h.pages.len() * 2 + 200 {
            h.compact();
        }
        h.log = OpenOptions::new().create(true).append(true).open(path).ok();
        h
    }

    fn apply(&mut self, line: &str) {
        let mut f = line.split('\t');
        match f.next() {
            Some("p") => {
                let (Some(visits), Some(last), Some(url)) = (f.next(), f.next(), f.next()) else { return };
                let title = f.next().unwrap_or("");
                let i = self.slot(url);
                let p = &mut self.pages[i];
                p.visits = visits.parse().unwrap_or(1);
                p.last = last.parse().unwrap_or(0);
                p.title = title.to_string();
            }
            Some("v") => {
                let (Some(ts), Some(url)) = (f.next(), f.next()) else { return };
                let title = f.next();
                let i = self.slot(url);
                let p = &mut self.pages[i];
                p.visits += 1;
                p.last = ts.parse().unwrap_or(p.last);
                if let Some(t) = title.filter(|t| !t.is_empty()) {
                    p.title = t.to_string();
                }
            }
            Some("t") => {
                let (Some(url), Some(title)) = (f.next(), f.next()) else { return };
                if let Some(&i) = self.index.get(url) {
                    self.pages[i].title = title.to_string();
                }
            }
            Some("d") => {
                if let Some(url) = f.next() {
                    self.remove_mem(url);
                }
            }
            Some("x") => {
                self.pages.clear();
                self.index.clear();
            }
            _ => {}
        }
    }

    fn slot(&mut self, url: &str) -> usize {
        if let Some(&i) = self.index.get(url) {
            return i;
        }
        self.pages.push(Page { url: url.to_string(), title: String::new(), visits: 0, last: 0 });
        self.index.insert(url.to_string(), self.pages.len() - 1);
        self.pages.len() - 1
    }

    fn remove_mem(&mut self, url: &str) {
        if let Some(i) = self.index.remove(url) {
            self.pages.swap_remove(i);
            if i < self.pages.len() {
                let moved = self.pages[i].url.clone();
                self.index.insert(moved, i);
            }
        }
    }

    fn write(&mut self, line: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn compact(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let tmp = path.with_extension("tmp");
        let mut out = String::with_capacity(self.pages.len() * 96);
        for p in &self.pages {
            out.push_str(&format!("p\t{}\t{}\t{}\t{}\n", p.visits, p.last, p.url, p.title));
        }
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Records a visit.
    pub fn visit(&mut self, url: &str, title: &str, now: u64) {
        if !recordable(url) {
            return;
        }
        let url = clean(url);
        let title = clean(title);
        self.apply(&format!("v\t{now}\t{url}\t{title}"));
        self.write(&format!("v\t{now}\t{url}\t{title}\n"));
    }

    /// Updates the title of a page you've visited (titles arrive after the visit).
    pub fn set_title(&mut self, url: &str, title: &str) {
        let Some(&i) = self.index.get(url) else { return };
        let title = clean(title);
        if title.is_empty() || self.pages[i].title == title {
            return;
        }
        self.pages[i].title = title.clone();
        self.write(&format!("t\t{url}\t{title}\n"));
    }

    pub fn remove(&mut self, url: &str) {
        self.remove_mem(url);
        self.write(&format!("d\t{}\n", clean(url)));
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.index.clear();
        if let Some(path) = &self.path {
            let _ = std::fs::write(path, "");
            self.log = OpenOptions::new().create(true).append(true).open(path).ok();
        }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn get(&self, url: &str) -> Option<&Page> {
        self.index.get(url).map(|&i| &self.pages[i])
    }

    /// The best matches for what's been typed so far.
    pub fn query(&self, q: &str, limit: usize, now: u64) -> Vec<&Page> {
        let q = fuzzy::fold(q.trim());
        let words: Vec<&str> = q.split_whitespace().collect();
        let mut hits: Vec<(u32, &Page)> = self
            .pages
            .iter()
            .filter_map(|p| {
                let addr = fuzzy::fold(&omnibox::display(&p.url));
                let title = fuzzy::fold(&p.title);
                fuzzy::score(&words, &addr, &title).map(|s| (s * 30 + p.frecency(now), p))
            })
            .collect();
        hits.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        hits.truncate(limit);
        hits.into_iter().map(|(_, p)| p).collect()
    }

    /// Inline completion: typing `git` offers `github.com`.
    pub fn complete(&self, typed: &str, now: u64) -> Option<String> {
        let t = fuzzy::fold(typed.trim());
        if t.is_empty() || t.contains(' ') {
            return None;
        }
        let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(&t);
        let t = t.strip_prefix("www.").unwrap_or(t);
        let mut best: Option<(u32, String)> = None;
        for p in &self.pages {
            let addr = fuzzy::fold(&omnibox::display(&p.url));
            if !addr.starts_with(t) || crate::search::query_of(&p.url).is_some() {
                continue;
            }
            let host = omnibox::host(&p.url).to_ascii_lowercase();
            let cand = if host.starts_with(t) { host } else { addr };
            let f = p.frecency(now);
            if best.as_ref().map(|(bf, _)| f > *bf).unwrap_or(true) {
                best = Some((f, cand));
            }
        }
        best.map(|(_, s)| s)
    }

    /// Most-visited sites, one per host, for the new tab page.
    pub fn top_sites(&self, n: usize, now: u64) -> Vec<(String, String, String)> {
        let mut by_host: HashMap<&str, (u32, &Page)> = HashMap::new();
        for p in &self.pages {
            if crate::search::query_of(&p.url).is_some() {
                continue;
            }
            let h = omnibox::host(&p.url);
            let f = p.frecency(now);
            let e = by_host.entry(h).or_insert((0, p));
            e.0 += f;
            // Prefer the shortest URL on a host: usually its front page.
            if p.url.len() < e.1.url.len() {
                e.1 = p;
            }
        }
        let mut v: Vec<(u32, &str, &Page)> = by_host.into_iter().map(|(h, (f, p))| (f, h, p)).collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        v.into_iter().take(n).map(|(_, h, p)| (h.to_string(), p.url.clone(), p.title.clone())).collect()
    }

    /// Everything, most recent first (for the History page).
    pub fn recent(&self, limit: usize) -> Vec<&Page> {
        let mut v: Vec<&Page> = self.pages.iter().collect();
        v.sort_unstable_by(|a, b| b.last.cmp(&a.last));
        v.truncate(limit);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_round_trip_and_rank() {
        let dir = std::env::temp_dir().join(format!("volant-hist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.log");
        let _ = std::fs::remove_file(&path);
        let now = 1_700_000_000;
        {
            let mut h = History::open(&path);
            h.visit("https://github.com/", "GitHub", now);
            h.visit("https://github.com/", "", now);
            h.visit("https://example.com/legit", "Example\tpage", now - 86_400 * 40);
            h.visit("volant://newtab", "New Tab", now);
            h.set_title("https://example.com/legit", "Legit");
            h.visit("https://gone.com/", "Gone", now);
            h.remove("https://gone.com/");
        }
        let h = History::open(&path);
        assert_eq!(h.len(), 2);
        let gh = h.get("https://github.com/").unwrap();
        assert_eq!((gh.visits, gh.title.as_str()), (2, "GitHub"));
        assert_eq!(h.get("https://example.com/legit").unwrap().title, "Legit");
        let q = h.query("git", 5, now);
        assert_eq!(q[0].url, "https://github.com/");
        assert_eq!(h.complete("gi", now).as_deref(), Some("github.com"));
        assert_eq!(h.complete("example.com/l", now).as_deref(), Some("example.com/legit"));
        assert_eq!(h.top_sites(8, now)[0].0, "github.com");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
