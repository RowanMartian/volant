//! Browsing history as an append-only text log.
//!
//! Browsers usually keep history in SQLite. Volant writes one short line per
//! visit to a log file and keeps everything in memory, which makes recording a
//! visit a single `write` and lets the whole history load in a few milliseconds.
//! The log is compacted on startup once it has grown well past the number of pages.

use crate::fuzzy;
use crate::omnibox;
use std::cell::RefCell;
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
    by_url: HashMap<String, usize, FxBuild>,
    log: Option<File>,
    path: Option<PathBuf>,
    /// Built while the command bar is open, then dropped.
    index: RefCell<Option<Index>>,
    /// Top sites change slowly; recomputing them for every new tab is waste.
    top_cache: RefCell<Option<(u64, usize, TopSites)>>,
}

type TopSites = Vec<(String, String, String)>;

/// How long the new tab page may show the same top sites.
const TOP_TTL: u64 = 300;

/// A lowercased "address<TAB>title" for every page, so matching as you type is
/// a plain substring scan with no allocation per page. Entries are sorted by
/// frecency, which lets a query stop as soon as nothing further down can make
/// the top results. Pages first seen while the index exists are appended after
/// the sorted part and always checked.
struct Index {
    /// Every entry's text back to back, one allocation for the whole index.
    buf: String,
    /// Where each entry's text sits in `buf`.
    span: Vec<(u32, u32)>,
    addr_len: Vec<u32>,
    search: Vec<bool>,
    /// Which page each entry is.
    page: Vec<u32>,
    /// Frecency when the index was built (non-increasing up to `sorted`).
    fre: Vec<u32>,
    sorted: usize,
    /// Where each page sits in the index (by page number).
    slot: Vec<u32>,
    /// The previous query and every entry it matched, to narrow from.
    last: String,
    hits: Vec<u32>,
}

impl Index {
    /// Appends a page's text to `buf`; returns its span, address length and search flag.
    fn write(buf: &mut String, p: &Page) -> ((u32, u32), u32, bool) {
        let start = buf.len();
        let addr = omnibox::display(&p.url);
        buf.push_str(&addr);
        let addr_len = buf.len() - start;
        buf.push('\t');
        buf.push_str(&p.title);
        buf[start..].make_ascii_lowercase();
        ((start as u32, buf.len() as u32), addr_len as u32, crate::search::query_of(&p.url).is_some())
    }

    fn hay(&self, j: usize) -> &str {
        let (a, b) = self.span[j];
        &self.buf[a as usize..b as usize]
    }

    fn set(&mut self, j: usize, p: &Page) {
        let (span, a, s) = Self::write(&mut self.buf, p);
        self.span[j] = span;
        self.addr_len[j] = a;
        self.search[j] = s;
    }

    fn build(pages: &[Page], now: u64) -> Self {
        let n = pages.len();
        let mut order: Vec<(u32, u32)> = pages.iter().enumerate().map(|(i, p)| (p.frecency(now), i as u32)).collect();
        order.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        let bytes: usize = pages.iter().map(|p| p.url.len() + p.title.len() + 1).sum();
        let mut ix = Index {
            buf: String::with_capacity(bytes),
            span: Vec::with_capacity(n),
            addr_len: Vec::with_capacity(n),
            search: Vec::with_capacity(n),
            page: Vec::with_capacity(n),
            fre: Vec::with_capacity(n),
            sorted: n,
            slot: vec![0; n],
            last: String::new(),
            hits: Vec::new(),
        };
        for (j, &(f, i)) in order.iter().enumerate() {
            ix.push(&pages[i as usize], i, f);
            ix.slot[i as usize] = j as u32;
        }
        ix
    }

    fn push(&mut self, p: &Page, page: u32, fre: u32) {
        let (h, a, s) = Self::write(&mut self.buf, p);
        self.span.push(h);
        self.addr_len.push(a);
        self.search.push(s);
        self.page.push(page);
        self.fre.push(fre);
    }
}

/// FxHash: a fast non-cryptographic hash for short keys (URLs and hosts).
#[derive(Default, Clone, Copy)]
pub struct FxBuild;
#[derive(Default)]
pub struct Fx(u64);
impl std::hash::BuildHasher for FxBuild {
    type Hasher = Fx;
    fn build_hasher(&self) -> Fx {
        Fx(0)
    }
}
impl std::hash::Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(K);
        }
        for &b in chunks.remainder() {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(K);
        }
    }
    fn write_u8(&mut self, b: u8) {
        self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

fn recordable(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() < 4096
}

fn consider(ix: &Index, j: usize, words: &[&str], top: &mut std::collections::BinaryHeap<std::cmp::Reverse<(u32, u32)>>, hits: &mut Vec<u32>, limit: usize) {
    let hay = ix.hay(j);
    if !words.iter().all(|w| hay.contains(w)) {
        return;
    }
    hits.push(j as u32);
    let al = ix.addr_len[j] as usize;
    if let Some(s) = fuzzy::score(words, &hay[..al], &hay[al + 1..]) {
        let score = s * 30 + ix.fre[j];
        if top.len() < limit {
            top.push(std::cmp::Reverse((score, j as u32)));
        } else if top.peek().map(|r| score > r.0 .0).unwrap_or(false) {
            top.pop();
            top.push(std::cmp::Reverse((score, j as u32)));
        }
    }
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
                self.reindex(i, None);
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
                let now = p.last;
                self.reindex(i, Some(now));
            }
            Some("t") => {
                let (Some(url), Some(title)) = (f.next(), f.next()) else { return };
                if let Some(&i) = self.by_url.get(url) {
                    self.pages[i].title = title.to_string();
                    self.reindex(i, None);
                }
            }
            Some("d") => {
                if let Some(url) = f.next() {
                    self.remove_mem(url);
                }
            }
            Some("x") => {
                self.pages.clear();
                self.by_url.clear();
                *self.index.get_mut() = None;
                *self.top_cache.get_mut() = None;
            }
            _ => {}
        }
    }

    fn slot(&mut self, url: &str) -> usize {
        if let Some(&i) = self.by_url.get(url) {
            return i;
        }
        self.pages.push(Page { url: url.to_string(), title: String::new(), visits: 0, last: 0 });
        self.by_url.insert(url.to_string(), self.pages.len() - 1);
        self.pages.len() - 1
    }

    /// Keeps the search index (if built) in step with page `i`. A brand-new
    /// page joins the unsorted tail; a changed one is updated in place.
    fn reindex(&mut self, i: usize, now: Option<u64>) {
        let Some(ix) = self.index.get_mut().as_mut() else { return };
        let p = &self.pages[i];
        if i == ix.slot.len() {
            ix.slot.push(ix.span.len() as u32);
            let f = now.map(|n| p.frecency(n)).unwrap_or(0);
            ix.push(p, i as u32, f);
        } else if i < ix.slot.len() {
            let j = ix.slot[i] as usize;
            ix.set(j, p);
            if let Some(n) = now {
                let f = p.frecency(n);
                // A page that became more frecent may no longer fit its sorted spot.
                if j < ix.sorted && f > ix.fre[j] {
                    *self.index.get_mut() = None;
                    return;
                }
                ix.fre[j] = f;
            }
        } else {
            *self.index.get_mut() = None;
            return;
        }
        ix.last.clear();
    }

    fn remove_mem(&mut self, url: &str) {
        *self.index.get_mut() = None;
        *self.top_cache.get_mut() = None;
        if let Some(i) = self.by_url.remove(url) {
            self.pages.swap_remove(i);
            if i < self.pages.len() {
                let moved = self.pages[i].url.clone();
                self.by_url.insert(moved, i);
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
        let new_site = !self.by_url.contains_key(url.as_str()) && self.len() < 64;
        self.apply(&format!("v\t{now}\t{url}\t{title}"));
        if new_site {
            // A young history changes fast; don't make a new user wait 5 minutes.
            *self.top_cache.get_mut() = None;
        }
        self.write(&format!("v\t{now}\t{url}\t{title}\n"));
    }

    /// Updates the title of a page you've visited (titles arrive after the visit).
    pub fn set_title(&mut self, url: &str, title: &str) {
        let Some(&i) = self.by_url.get(url) else { return };
        let title = clean(title);
        if title.is_empty() || self.pages[i].title == title {
            return;
        }
        self.pages[i].title = title.clone();
        self.reindex(i, None);
        self.write(&format!("t\t{url}\t{title}\n"));
    }

    pub fn remove(&mut self, url: &str) {
        self.remove_mem(url);
        self.write(&format!("d\t{}\n", clean(url)));
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.by_url.clear();
        *self.index.get_mut() = None;
        *self.top_cache.get_mut() = None;
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
        self.by_url.get(url).map(|&i| &self.pages[i])
    }

    /// Builds the search index if it isn't there. The command bar calls this
    /// when it opens, so the first keystroke doesn't pay for it.
    pub fn warm(&self, now: u64) {
        let mut ix = self.index.borrow_mut();
        if ix.is_none() {
            *ix = Some(Index::build(&self.pages, now));
        }
    }

    /// Frees the search index (the command bar closed).
    pub fn cool(&self) {
        *self.index.borrow_mut() = None;
    }

    /// The best matches for what's been typed so far.
    pub fn query(&self, q: &str, limit: usize, now: u64) -> Vec<&Page> {
        if limit == 0 {
            return Vec::new();
        }
        let q = fuzzy::fold(q.trim());
        let words: Vec<&str> = q.split_whitespace().collect();
        self.warm(now);
        let mut guard = self.index.borrow_mut();
        let ix = guard.as_mut().expect("index");
        // The most a page's text can add to its score; with frecency sorted
        // high to low, once (best text score + frecency) can't beat the
        // current k-th result, nothing further down can either.
        let text_max: u32 = words.iter().map(|w| 120 + w.len() as u32).sum::<u32>() * 30;
        let mut top: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u32)>> = std::collections::BinaryHeap::with_capacity(limit + 1);
        let mut hits = Vec::new();
        let mut complete = true;
        let narrow = !ix.last.is_empty() && q.starts_with(ix.last.as_str());
        let candidates: Vec<u32> = if narrow { std::mem::take(&mut ix.hits) } else { Vec::new() };
        let total = if narrow { candidates.len() } else { ix.span.len() };
        for n in 0..total {
            let j = if narrow { candidates[n] } else { n as u32 } as usize;
            if j < ix.sorted && top.len() == limit {
                let kth = top.peek().map(|r| r.0 .0).unwrap_or(0);
                if text_max + ix.fre[j] < kth {
                    // Everything left in the sorted part scores lower. Only the unsorted tail can still place.
                    complete = false;
                    let rest: Vec<usize> = if narrow {
                        candidates[n..].iter().map(|&c| c as usize).filter(|&c| c >= ix.sorted).collect()
                    } else {
                        (ix.sorted..ix.span.len()).collect()
                    };
                    for j in rest {
                        consider(ix, j, &words, &mut top, &mut hits, limit);
                    }
                    break;
                }
            }
            consider(ix, j, &words, &mut top, &mut hits, limit);
        }
        // Only a full scan can seed the next keystroke's narrowing.
        if complete {
            ix.last = q.clone();
            ix.hits = hits;
        } else {
            ix.last.clear();
            ix.hits.clear();
        }
        let mut out: Vec<(u32, u32)> = top.into_iter().map(|r| r.0).collect();
        out.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(_, j)| &self.pages[ix.page[j as usize] as usize]).collect()
    }

    /// Inline completion: typing `git` offers `github.com`.
    pub fn complete(&self, typed: &str, now: u64) -> Option<String> {
        let t = fuzzy::fold(typed.trim());
        if t.is_empty() || t.contains(' ') {
            return None;
        }
        let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(&t);
        let t = t.strip_prefix("www.").unwrap_or(t);
        self.warm(now);
        let guard = self.index.borrow();
        let ix = guard.as_ref().expect("index");
        // Sorted by frecency, so the first match in the sorted part is the best one there.
        let mut best: Option<(u32, usize)> = None;
        let matches = |j: usize| !ix.search[j] && ix.addr_len[j] as usize >= t.len() && ix.hay(j).starts_with(t);
        if let Some(j) = (0..ix.sorted).find(|&j| matches(j)) {
            best = Some((ix.fre[j], j));
        }
        for j in ix.sorted..ix.span.len() {
            if matches(j) && best.map(|(bf, _)| ix.fre[j] > bf).unwrap_or(true) {
                best = Some((ix.fre[j], j));
            }
        }
        let (_, j) = best?;
        let addr = &ix.hay(j)[..ix.addr_len[j] as usize];
        let host = &addr[..addr.find(['/', '?', '#', ':']).unwrap_or(addr.len())];
        Some(if host.starts_with(t) { host } else { addr }.to_string())
    }

    /// Most-visited sites, one per host, for the new tab page. Cached for a few minutes.
    pub fn top_sites(&self, n: usize, now: u64) -> TopSites {
        if let Some((at, cn, v)) = self.top_cache.borrow().as_ref() {
            if *cn == n && now.saturating_sub(*at) < TOP_TTL {
                return v.clone();
            }
        }
        let v = self.compute_top_sites(n, now);
        *self.top_cache.borrow_mut() = Some((now, n, v.clone()));
        v
    }

    fn compute_top_sites(&self, n: usize, now: u64) -> TopSites {
        let mut by_host: HashMap<&str, (u32, u32), FxBuild> = HashMap::with_capacity_and_hasher(self.pages.len() / 4 + 16, FxBuild);
        for (i, p) in self.pages.iter().enumerate() {
            if crate::search::query_of(&p.url).is_some() {
                continue;
            }
            let f = p.frecency(now);
            let e = by_host.entry(omnibox::host(&p.url)).or_insert((0, i as u32));
            e.0 += f;
            // Prefer the shortest URL on a host: usually its front page.
            if p.url.len() < self.pages[e.1 as usize].url.len() {
                e.1 = i as u32;
            }
        }
        let mut v: Vec<(u32, &str, u32)> = by_host.into_iter().map(|(h, (f, i))| (f, h, i)).collect();
        let k = n.min(v.len());
        if k == 0 {
            return Vec::new();
        }
        let cmp = |a: &(u32, &str, u32), b: &(u32, &str, u32)| b.0.cmp(&a.0).then(a.1.cmp(b.1));
        if v.len() > k {
            v.select_nth_unstable_by(k - 1, cmp);
            v.truncate(k);
        }
        v.sort_unstable_by(cmp);
        v.into_iter()
            .map(|(_, h, i)| {
                let p = &self.pages[i as usize];
                (h.to_string(), p.url.clone(), p.title.clone())
            })
            .collect()
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

    #[test]
    fn index_tracks_changes_while_typing() {
        let now = 1_700_000_000;
        let mut h = History::ephemeral();
        h.visit("https://alpha.com/", "Alpha", now);
        h.warm(now);
        assert_eq!(h.query("al", 5, now).len(), 1);
        h.visit("https://alpine.org/", "Alpine", now); // arrives while the bar is open
        assert_eq!(h.query("alp", 5, now).len(), 2, "narrowing must not hide new pages");
        h.set_title("https://alpha.com/", "Renamed");
        assert_eq!(h.query("renamed", 5, now).len(), 1);
        h.remove("https://alpha.com/");
        assert_eq!(h.query("alp", 5, now).len(), 1);
        assert_eq!(h.complete("alp", now).as_deref(), Some("alpine.org"));
        h.cool();
        assert_eq!(h.query("alpine", 5, now)[0].url, "https://alpine.org/");
    }
}
