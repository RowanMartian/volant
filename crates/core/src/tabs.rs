//! The tab strip as data: order, pinning, selection, closed-tab memory and
//! which tabs have been idle long enough to sleep. The macOS shell mirrors this
//! with web views; keeping the rules here makes them testable anywhere.

use crate::store::{SavedTab, Session};

pub type TabId = u64;

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    pub pinned: bool,
    /// When this tab was last in front (seconds).
    pub last_active: u64,
    /// Asleep: its web view has been released and it reloads when selected.
    pub asleep: bool,
}

#[derive(Default)]
pub struct Tabs {
    pub list: Vec<Tab>,
    pub active: Option<TabId>,
    closed: Vec<(SavedTab, usize)>,
    next_id: TabId,
    /// Background tabs opened in a row from the same tab, so the next one goes after them.
    run: Option<(TabId, usize)>,
}

impl Tabs {
    pub fn new() -> Self {
        Self { next_id: 1, ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn index(&self, id: TabId) -> Option<usize> {
        self.list.iter().position(|t| t.id == id)
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.list.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    fn pinned_count(&self) -> usize {
        self.list.iter().filter(|t| t.pinned).count()
    }

    /// Opens a tab. New tabs go right after the current one (or after the pinned tabs).
    pub fn open(&mut self, url: &str, title: &str, pinned: bool, next_to_active: bool, now: u64) -> TabId {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let tab = Tab { id, url: url.into(), title: title.into(), pinned, last_active: now, asleep: false };
        let at = if pinned {
            self.pinned_count()
        } else if next_to_active {
            // Like Chrome and Safari: links opened one after another from the same
            // page line up in the order you opened them, right after that page.
            let after = match (self.active, self.run) {
                (Some(a), Some((opener, n))) if opener == a => n,
                _ => 0,
            };
            if let Some(a) = self.active {
                self.run = Some((a, after + 1));
            }
            self.active.and_then(|a| self.index(a)).map(|i| i + 1 + after).unwrap_or(self.list.len()).max(self.pinned_count())
        } else {
            self.list.len()
        };
        self.list.insert(at.min(self.list.len()), tab);
        id
    }

    pub fn select(&mut self, id: TabId, now: u64) -> bool {
        if self.active != Some(id) {
            self.run = None;
        }
        if let Some(t) = self.get_mut(id) {
            t.last_active = now;
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Closes a tab and returns which tab should be in front next.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let i = self.index(id)?;
        let t = self.list.remove(i);
        self.closed.push((SavedTab { url: t.url, title: t.title, pinned: t.pinned }, i));
        if self.closed.len() > 50 {
            self.closed.remove(0);
        }
        if self.active == Some(id) {
            // Like Safari: the tab to the right, else the one to the left.
            self.active = self.list.get(i).or_else(|| i.checked_sub(1).and_then(|j| self.list.get(j))).map(|t| t.id);
        }
        self.active
    }

    /// Brings back the most recently closed tab (⇧⌘T).
    pub fn reopen(&mut self, now: u64) -> Option<TabId> {
        let (s, at) = self.closed.pop()?;
        let id = self.open(&s.url, &s.title, s.pinned, false, now);
        let cur = self.index(id)?;
        let t = self.list.remove(cur);
        let at = at.min(self.list.len());
        self.list.insert(at, t);
        self.select(id, now);
        Some(id)
    }

    pub fn set_pinned(&mut self, id: TabId, pinned: bool) {
        let Some(i) = self.index(id) else { return };
        let mut t = self.list.remove(i);
        t.pinned = pinned;
        let at = self.pinned_count();
        self.list.insert(at, t);
    }

    /// Moves a tab to position `to`, keeping pinned tabs above the rest.
    pub fn move_to(&mut self, id: TabId, to: usize) {
        let Some(i) = self.index(id) else { return };
        let t = self.list.remove(i);
        let p = self.pinned_count();
        let to = if t.pinned { to.min(p) } else { to.clamp(p, self.list.len()) };
        self.list.insert(to, t);
    }

    /// The tab `delta` steps away from the current one, wrapping around (⌃Tab).
    pub fn neighbor(&self, delta: isize) -> Option<TabId> {
        let n = self.list.len() as isize;
        if n == 0 {
            return None;
        }
        let i = self.active.and_then(|a| self.index(a)).unwrap_or(0) as isize;
        Some(self.list[((i + delta) % n + n) as usize % n as usize].id)
    }

    /// Tabs that have sat in the background longer than `idle_secs` and are still awake.
    pub fn sleepy(&self, now: u64, idle_secs: u64) -> Vec<TabId> {
        if idle_secs == 0 {
            return Vec::new();
        }
        self.list
            .iter()
            .filter(|t| Some(t.id) != self.active && !t.asleep && now.saturating_sub(t.last_active) >= idle_secs)
            .map(|t| t.id)
            .collect()
    }

    pub fn to_session(&self) -> Session {
        Session {
            tabs: self.list.iter().map(|t| SavedTab { url: t.url.clone(), title: t.title.clone(), pinned: t.pinned }).collect(),
            active: self.active.and_then(|a| self.index(a)).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_close_reopen() {
        let mut t = Tabs::new();
        let a = t.open("a", "", false, true, 0);
        t.select(a, 0);
        let b = t.open("b", "", false, true, 0);
        let c = t.open("c", "", false, true, 0); // a run from a: b, then c
        assert_eq!(t.list.iter().map(|x| x.url.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        t.select(b, 1);
        let d = t.open("d", "", false, true, 1); // a new run starts after b
        assert_eq!(t.list.iter().map(|x| x.url.as_str()).collect::<Vec<_>>(), ["a", "b", "d", "c"]);
        t.close(d);
        t.select(c, 1);
        assert_eq!(t.close(c), Some(b));
        assert_eq!(t.close(b), Some(a));
        let r = t.reopen(2).unwrap();
        assert_eq!(t.get(r).unwrap().url, "b");
        assert_eq!(t.active, Some(r));
    }

    #[test]
    fn pinning_and_neighbors() {
        let mut t = Tabs::new();
        let a = t.open("a", "", false, false, 0);
        let b = t.open("b", "", false, false, 0);
        t.set_pinned(b, true);
        assert_eq!(t.list[0].id, b);
        let p = t.open("p", "", true, false, 0);
        assert_eq!(t.list[1].id, p);
        t.move_to(a, 0);
        assert_eq!(t.list[2].id, a, "unpinned tabs stay below pinned ones");
        t.select(a, 0);
        assert_eq!(t.neighbor(1), Some(b));
        assert_eq!(t.neighbor(-1), Some(p));
    }

    #[test]
    fn sleeping() {
        let mut t = Tabs::new();
        let a = t.open("a", "", false, false, 0);
        let b = t.open("b", "", false, false, 0);
        t.select(b, 1000);
        assert_eq!(t.sleepy(1000, 600), vec![a]);
        assert!(t.sleepy(1000, 0).is_empty());
    }
}
