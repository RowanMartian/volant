//! Time zones for `Date` and `Intl.DateTimeFormat`: the system's zone
//! database (TZif files, as macOS and Linux ship them) read directly,
//! with the POSIX rule at a file's end for dates past its last transition.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One zone: its transitions and the offsets they switch between.
pub struct Zone {
    /// Transition instants (seconds since 1970, UTC), ascending.
    times: Vec<i64>,
    /// For each transition, the index into `types`.
    idx: Vec<u8>,
    /// (seconds east of UTC, is daylight time, abbreviation)
    types: Vec<(i32, bool, String)>,
    /// The rule for instants after the last transition.
    rule: Option<Posix>,
}

/// A POSIX TZ rule: `EST5EDT,M3.2.0,M11.1.0`.
struct Posix {
    std: (String, i32),
    /// Daylight time and when it starts and ends (local times).
    dst: Option<(String, i32, Change, Change)>,
}

#[derive(Clone, Copy)]
enum Change {
    /// Month 1–12, week 1–5 (5 = last), weekday 0–6; seconds after midnight.
    Month(u32, u32, u32, i64),
    /// Day of the year, 1–365, never counting 29 February.
    Julian(u32, i64),
    /// Day of the year, 0–365.
    Day(u32, i64),
}

impl Zone {
    /// The offset (seconds east of UTC) and abbreviation in force at `t` (seconds).
    pub fn at(&self, t: i64) -> (i32, &str) {
        if let Some(rule) = &self.rule {
            if self.times.last().is_none_or(|&last| t >= last) {
                return rule.at(t);
            }
        }
        let k = match self.times.binary_search(&t) {
            Ok(k) => Some(k),
            Err(0) => None,
            Err(k) => Some(k - 1),
        };
        let ty = match k {
            Some(k) => self.idx.get(k).map(|&i| i as usize).unwrap_or(0),
            // Before the first transition: the first standard type.
            None => self.types.iter().position(|t| !t.1).unwrap_or(0),
        };
        match self.types.get(ty) {
            Some((off, _, abbr)) => (*off, abbr),
            None => (0, "UTC"),
        }
    }

    fn parse(b: &[u8]) -> Option<Zone> {
        if b.get(..4)? != b"TZif" {
            return Posix::parse(std::str::from_utf8(b).ok()?.trim()).map(Zone::from_rule);
        }
        let version = *b.get(4)?;
        let counts = |at: usize| -> Option<[usize; 6]> {
            let mut c = [0usize; 6];
            for (k, slot) in c.iter_mut().enumerate() {
                let p = at + 20 + k * 4;
                *slot = u32::from_be_bytes(b.get(p..p + 4)?.try_into().ok()?) as usize;
            }
            Some(c)
        };
        let [isut, isstd, leap, timecnt, typecnt, charcnt] = counts(0)?;
        let v1_len = timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isut;
        // Version 2 and later repeat the data with 64-bit times; use that.
        let (base, wide) = if version >= b'2' { (44 + v1_len, true) } else { (0, false) };
        let [isut, isstd, leap, timecnt, typecnt, charcnt] = if wide { counts(base)? } else { [isut, isstd, leap, timecnt, typecnt, charcnt] };
        let tsize = if wide { 8 } else { 4 };
        let mut p = base + 44;
        let mut times = Vec::with_capacity(timecnt);
        for _ in 0..timecnt {
            let raw = b.get(p..p + tsize)?;
            times.push(if wide { i64::from_be_bytes(raw.try_into().ok()?) } else { i32::from_be_bytes(raw.try_into().ok()?) as i64 });
            p += tsize;
        }
        let idx = b.get(p..p + timecnt)?.to_vec();
        p += timecnt;
        let mut raw_types = Vec::with_capacity(typecnt);
        for _ in 0..typecnt {
            let r = b.get(p..p + 6)?;
            raw_types.push((i32::from_be_bytes(r[0..4].try_into().ok()?), r[4] != 0, r[5] as usize));
            p += 6;
        }
        let chars = b.get(p..p + charcnt)?;
        p += charcnt + leap * (tsize + 4) + isstd + isut;
        let types = raw_types
            .into_iter()
            .map(|(off, dst, ai)| {
                let end = chars[ai.min(chars.len())..].iter().position(|&c| c == 0).map(|e| ai + e).unwrap_or(chars.len());
                (off, dst, String::from_utf8_lossy(&chars[ai.min(end)..end]).into_owned())
            })
            .collect();
        let rule = if wide {
            b.get(p..).and_then(|f| std::str::from_utf8(f).ok()).map(str::trim).filter(|s| !s.is_empty()).and_then(Posix::parse)
        } else {
            None
        };
        Some(Zone { times, idx, types, rule })
    }

    fn from_rule(rule: Posix) -> Zone {
        Zone { times: Vec::new(), idx: Vec::new(), types: vec![(rule.std.1, false, rule.std.0.clone())], rule: Some(rule) }
    }
}

impl Posix {
    fn at(&self, t: i64) -> (i32, &str) {
        let Some((dname, doff, start, end)) = &self.dst else { return (self.std.1, &self.std.0) };
        let year = crate::builtins::year_of_secs(t + self.std.1 as i64);
        // Both changes happen at local wall-clock times: the start in
        // standard time, the end in daylight time.
        let s = start.instant(year) - self.std.1 as i64;
        let e = end.instant(year) - *doff as i64;
        let in_dst = if s < e { t >= s && t < e } else { t >= s || t < e };
        if in_dst {
            (*doff, dname)
        } else {
            (self.std.1, &self.std.0)
        }
    }

    fn parse(s: &str) -> Option<Posix> {
        let mut r = Reader { s: s.as_bytes(), i: 0 };
        let std_name = r.name()?;
        let std_off = -r.offset()?;
        if r.done() {
            return Some(Posix { std: (std_name, std_off), dst: None });
        }
        let dst_name = r.name()?;
        let dst_off = if r.peek().is_some_and(|c| c == b'+' || c == b'-' || c.is_ascii_digit()) { -r.offset()? } else { std_off + 3600 };
        let (start, end) = if r.eat(b',') {
            let a = r.change()?;
            r.eat(b',').then_some(())?;
            (a, r.change()?)
        } else {
            // The US rule, the POSIX default.
            (Change::Month(3, 2, 0, 7200), Change::Month(11, 1, 0, 7200))
        };
        Some(Posix { std: (std_name, std_off), dst: Some((dst_name, dst_off, start, end)) })
    }
}

impl Change {
    /// The local instant (seconds, as if UTC) of this change in `year`.
    fn instant(&self, year: i64) -> i64 {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let jan1 = crate::builtins::days_from_civil_pub(year, 1, 1);
        let (day, secs) = match *self {
            Change::Julian(n, s) => (jan1 + n as i64 - 1 + i64::from(leap && n >= 60), s),
            Change::Day(n, s) => (jan1 + n as i64, s),
            Change::Month(m, w, d, s) => {
                let first = crate::builtins::days_from_civil_pub(year, m as i64, 1);
                let wd1 = (first + 4).rem_euclid(7);
                let mut day = first + (d as i64 - wd1).rem_euclid(7) + (w as i64 - 1) * 7;
                let next = if m == 12 { crate::builtins::days_from_civil_pub(year + 1, 1, 1) } else { crate::builtins::days_from_civil_pub(year, m as i64 + 1, 1) };
                while day >= next {
                    day -= 7;
                }
                (day, s)
            }
        };
        day * 86_400 + secs
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<String> {
        let from = self.i;
        if self.eat(b'<') {
            while self.peek()? != b'>' {
                self.i += 1;
            }
            self.i += 1;
            return Some(String::from_utf8_lossy(&self.s[from + 1..self.i - 1]).into_owned());
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        (self.i - from >= 3).then(|| String::from_utf8_lossy(&self.s[from..self.i]).into_owned())
    }
    fn num(&mut self) -> Option<i64> {
        let from = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[from..self.i]).ok()?.parse().ok()
    }
    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn offset(&mut self) -> Option<i32> {
        let neg = if self.eat(b'-') {
            true
        } else {
            self.eat(b'+');
            false
        };
        let mut secs = self.num()? * 3600;
        if self.eat(b':') {
            secs += self.num()? * 60;
            if self.eat(b':') {
                secs += self.num()?;
            }
        }
        Some(if neg { -secs } else { secs } as i32)
    }
    fn change(&mut self) -> Option<Change> {
        let c = if self.eat(b'M') {
            let m = self.num()? as u32;
            self.eat(b'.').then_some(())?;
            let w = self.num()? as u32;
            self.eat(b'.').then_some(())?;
            let d = self.num()? as u32;
            Change::Month(m, w, d, 0)
        } else if self.eat(b'J') {
            Change::Julian(self.num()? as u32, 0)
        } else {
            Change::Day(self.num()? as u32, 0)
        };
        let secs = if self.eat(b'/') { self.offset()? as i64 } else { 7200 };
        Some(match c {
            Change::Month(m, w, d, _) => Change::Month(m, w, d, secs),
            Change::Julian(n, _) => Change::Julian(n, secs),
            Change::Day(n, _) => Change::Day(n, secs),
        })
    }
}

thread_local! {
    static ZONES: RefCell<HashMap<String, Option<Rc<Zone>>>> = RefCell::new(HashMap::new());
    static LOCAL: RefCell<Option<(String, Option<Rc<Zone>>)>> = const { RefCell::new(None) };
}

const ZONEINFO: [&str; 3] = ["/usr/share/zoneinfo", "/var/db/timezone/zoneinfo", "/usr/lib/zoneinfo"];

/// A named zone ("America/New_York"), read once.
pub fn zone(name: &str) -> Option<Rc<Zone>> {
    let name = if name.eq_ignore_ascii_case("utc") || name.eq_ignore_ascii_case("etc/utc") || name.eq_ignore_ascii_case("gmt") { "UTC" } else { name };
    if name.is_empty() || name.contains("..") || name.starts_with('/') {
        return None;
    }
    if let Some(z) = ZONES.with(|z| z.borrow().get(name).cloned()) {
        return z;
    }
    let found = if name == "UTC" {
        Some(Rc::new(Zone::from_rule(Posix { std: ("UTC".into(), 0), dst: None })))
    } else {
        ZONEINFO.iter().find_map(|dir| std::fs::read(format!("{dir}/{name}")).ok()).and_then(|b| Zone::parse(&b)).map(Rc::new)
    };
    ZONES.with(|z| z.borrow_mut().insert(name.to_string(), found.clone()));
    found
}

/// This machine's zone: `TZ` if set, else /etc/localtime. Its IANA name
/// ("UTC" when none is known) and the zone itself.
fn local() -> (String, Option<Rc<Zone>>) {
    if let Some(l) = LOCAL.with(|l| l.borrow().clone()) {
        return l;
    }
    let found = match std::env::var("TZ") {
        Ok(tz) if !tz.is_empty() => {
            let name = tz.trim_start_matches(':');
            match zone(name) {
                Some(z) => (name.to_string(), Some(z)),
                None => match Posix::parse(name) {
                    Some(p) if p.dst.is_none() && p.std.1 == 0 => ("UTC".to_string(), zone("UTC")),
                    Some(p) => (name.to_string(), Some(Rc::new(Zone::from_rule(p)))),
                    None => ("UTC".to_string(), zone("UTC")),
                },
            }
        }
        _ => {
            let name = std::fs::read_link("/etc/localtime")
                .ok()
                .and_then(|p| p.to_str().and_then(|s| s.split_once("zoneinfo/").map(|(_, n)| n.to_string())))
                .unwrap_or_else(|| "UTC".to_string());
            let z = std::fs::read("/etc/localtime").ok().and_then(|b| Zone::parse(&b)).map(Rc::new).or_else(|| zone(&name));
            let name = if name == "Etc/UTC" || name == "Etc/GMT" || name == "GMT" { "UTC".to_string() } else { name };
            (name, z)
        }
    };
    LOCAL.with(|l| *l.borrow_mut() = Some(found.clone()));
    found
}

/// The local zone's IANA name.
pub fn local_name() -> String {
    local().0
}

/// Milliseconds to add to a UTC time to get local wall-clock time at that instant.
pub fn local_offset(t_ms: f64) -> f64 {
    if !t_ms.is_finite() {
        return 0.0;
    }
    match local().1 {
        Some(z) => z.at((t_ms / 1000.0).floor() as i64).0 as f64 * 1000.0,
        None => 0.0,
    }
}

/// The local zone's abbreviation at an instant ("PST", "CEST").
pub fn local_abbr(t_ms: f64) -> String {
    match local().1 {
        Some(z) if t_ms.is_finite() => z.at((t_ms / 1000.0).floor() as i64).1.to_string(),
        _ => "UTC".into(),
    }
}

/// The UTC time of a local wall-clock time (ms), choosing the earlier
/// instant when the clock repeats and skipping forward over a gap.
pub fn local_to_utc(local_ms: f64) -> f64 {
    if !local_ms.is_finite() {
        return local_ms;
    }
    let first = local_offset(local_ms);
    let t = local_ms - first;
    let second = local_offset(t);
    if second == first {
        return t;
    }
    let t2 = local_ms - second;
    if local_offset(t2) == second {
        t2.min(t)
    } else {
        t
    }
}

/// The offset (minutes east of UTC) of a named zone at an instant, if the zone is known.
pub fn zone_offset_minutes(name: &str, t_ms: f64) -> Option<f64> {
    let z = zone(name)?;
    Some(z.at((t_ms / 1000.0).floor() as i64).0 as f64 / 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_rules() {
        let z = Zone::from_rule(Posix::parse("EST5EDT,M3.2.0,M11.1.0").unwrap());
        // 2024-01-15 12:00 UTC: standard time.
        assert_eq!(z.at(1_705_320_000).0, -5 * 3600);
        // 2024-07-15 12:00 UTC: daylight time.
        assert_eq!(z.at(1_721_044_800), (-4 * 3600, "EDT"));
        // 2024-03-10 06:59:59 UTC is 01:59:59 EST; a second later is 03:00 EDT.
        assert_eq!(z.at(1_710_053_999).0, -5 * 3600);
        assert_eq!(z.at(1_710_054_000).0, -4 * 3600);
        // Southern hemisphere: AEST-10AEDT,M10.1.0,M4.1.0/3
        let s = Zone::from_rule(Posix::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap());
        assert_eq!(s.at(1_705_320_000).0, 11 * 3600);
        assert_eq!(s.at(1_721_044_800).0, 10 * 3600);
        assert_eq!(Zone::from_rule(Posix::parse("<+0530>-5:30").unwrap()).at(0).0, 5 * 3600 + 1800);
    }

    #[test]
    fn system_zone_files() {
        // Present on the machines this runs on; skipped where they are not.
        if let Some(z) = zone("America/New_York") {
            assert_eq!(z.at(1_705_320_000).0, -5 * 3600);
            assert_eq!(z.at(1_721_044_800).0, -4 * 3600);
            // Far past the transitions a slim file lists: the footer rule.
            assert_eq!(z.at(4_102_444_800 + 180 * 86_400).0, -4 * 3600);
        }
        assert!(zone("No/Such_Zone").is_none());
        assert_eq!(zone("UTC").unwrap().at(0).0, 0);
    }
}
