//! A TrueType font reader: the tables Canard needs to set and draw text from a
//! font file by itself (`cmap` for characters, `hmtx` for advances, `hhea` for
//! ascent and descent, `kern` for pairs, `glyf`/`loca` for outlines, simple and
//! compound). WOFF files are unpacked first. Fonts with CFF outlines read their
//! metrics but have no outlines yet.

use std::sync::Arc;

/// A parsed font file.
#[derive(Debug)]
pub struct Face {
    data: Vec<u8>,
    pub units_per_em: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
    num_glyphs: u16,
    long_loca: bool,
    n_hmetrics: u16,
    cmap: Option<(usize, u16)>,
    hmtx: Option<usize>,
    loca: Option<usize>,
    glyf: Option<(usize, usize)>,
    kern: Option<(usize, usize)>,
}

/// One piece of a glyph's outline, in font units with y up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Close,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    u16_at(d, o).map(|v| v as i16)
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}

impl Face {
    /// Reads a TrueType or OpenType file (or a WOFF, or the first font of a collection).
    pub fn parse(bytes: &[u8]) -> Option<Arc<Face>> {
        let data = if volant_core::woff::is_woff(bytes) { volant_core::woff::to_sfnt(bytes, 64 << 20)? } else { bytes.to_vec() };
        let base = if data.get(..4) == Some(b"ttcf") { u32_at(&data, 12)? as usize } else { 0 };
        let version = u32_at(&data, base)?;
        if !matches!(version, 0x0001_0000 | 0x7472_7565 | 0x4f54_544f) {
            return None;
        }
        let n = u16_at(&data, base + 4)? as usize;
        let table = |tag: &[u8; 4]| -> Option<(usize, usize)> {
            (0..n).find_map(|k| {
                let r = base + 12 + k * 16;
                if data.get(r..r + 4)? != tag {
                    return None;
                }
                let (off, len) = (u32_at(&data, r + 8)? as usize, u32_at(&data, r + 12)? as usize);
                (off.checked_add(len)? <= data.len()).then_some((off, len))
            })
        };
        let (head, _) = table(b"head")?;
        let (hhea, _) = table(b"hhea")?;
        let maxp = table(b"maxp");
        let units_per_em = u16_at(&data, head + 18).filter(|&u| u >= 16)?;
        let long_loca = i16_at(&data, head + 50)? == 1;
        let num_glyphs = maxp.and_then(|(m, _)| u16_at(&data, m + 4)).unwrap_or(0);
        let cmap = table(b"cmap").and_then(|(c, _)| best_cmap(&data, c));
        let hmtx = table(b"hmtx").map(|t| t.0);
        let loca = table(b"loca").map(|t| t.0);
        let glyf = table(b"glyf");
        let kern = table(b"kern");
        Some(Arc::new(Face {
            units_per_em,
            ascent: i16_at(&data, hhea + 4)?,
            descent: i16_at(&data, hhea + 6)?,
            line_gap: i16_at(&data, hhea + 8)?,
            n_hmetrics: u16_at(&data, hhea + 34)?,
            num_glyphs,
            long_loca,
            cmap,
            hmtx,
            loca,
            glyf,
            kern,
            data,
        }))
    }

    /// The glyph for a character (0, the missing glyph, if the font hasn't one).
    pub fn glyph(&self, ch: char) -> u16 {
        self.lookup(ch as u32).unwrap_or(0)
    }

    /// Whether the font has a glyph for this character.
    pub fn has(&self, ch: char) -> bool {
        self.lookup(ch as u32).is_some_and(|g| g != 0)
    }

    fn lookup(&self, c: u32) -> Option<u16> {
        let (o, format) = self.cmap?;
        let d = &self.data;
        match format {
            0 => (c < 256).then(|| d.get(o + 6 + c as usize).map(|&g| g as u16)).flatten(),
            4 => {
                if c > 0xFFFF {
                    return None;
                }
                let segs = u16_at(d, o + 6)? as usize / 2;
                let ends = o + 14;
                let starts = ends + segs * 2 + 2;
                let deltas = starts + segs * 2;
                let ranges = deltas + segs * 2;
                // Binary search for the first segment ending at or after c.
                let (mut lo, mut hi) = (0, segs);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if (u16_at(d, ends + mid * 2)? as u32) < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo == segs {
                    return None;
                }
                let start = u16_at(d, starts + lo * 2)? as u32;
                if start > c {
                    return None;
                }
                let delta = u16_at(d, deltas + lo * 2)?;
                let range = u16_at(d, ranges + lo * 2)? as usize;
                if range == 0 {
                    return Some((c as u16).wrapping_add(delta));
                }
                let g = u16_at(d, ranges + lo * 2 + range + 2 * (c - start) as usize)?;
                Some(if g == 0 { 0 } else { g.wrapping_add(delta) })
            }
            6 => {
                let first = u16_at(d, o + 6)? as u32;
                let count = u16_at(d, o + 8)? as u32;
                (c >= first && c < first + count).then(|| u16_at(d, o + 10 + 2 * (c - first) as usize)).flatten()
            }
            12 => {
                let groups = u32_at(d, o + 12)? as usize;
                let (mut lo, mut hi) = (0, groups);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = o + 16 + mid * 12;
                    let (s, e) = (u32_at(d, g)?, u32_at(d, g + 4)?);
                    if c < s {
                        hi = mid;
                    } else if c > e {
                        lo = mid + 1;
                    } else {
                        return Some((u32_at(d, g + 8)? + (c - s)) as u16);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// A glyph's advance width, in font units.
    pub fn advance(&self, g: u16) -> u16 {
        let Some(h) = self.hmtx else { return self.units_per_em / 2 };
        let k = g.min(self.n_hmetrics.saturating_sub(1)) as usize;
        u16_at(&self.data, h + k * 4).unwrap_or(0)
    }

    /// The pair kerning between two glyphs, in font units (the `kern` table's format 0).
    pub fn kerning(&self, a: u16, b: u16) -> i16 {
        let Some((k, _)) = self.kern else { return 0 };
        let d = &self.data;
        let n = u16_at(d, k + 2).unwrap_or(0);
        let mut o = k + 4;
        for _ in 0..n {
            let (Some(len), Some(cov)) = (u16_at(d, o + 2), u16_at(d, o + 4)) else { return 0 };
            // Horizontal, format 0, not cross-stream.
            if cov & 0xFF07 == 0x0001 {
                let pairs = u16_at(d, o + 6).unwrap_or(0) as usize;
                let key = (a as u32) << 16 | b as u32;
                let (mut lo, mut hi) = (0, pairs);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let p = o + 14 + mid * 6;
                    let Some(pk) = u32_at(d, p) else { return 0 };
                    match pk.cmp(&key) {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return i16_at(d, p + 4).unwrap_or(0),
                    }
                }
            }
            o += len.max(6) as usize;
        }
        0
    }

    /// Whether the font's outlines can be drawn (TrueType `glyf` outlines).
    pub fn has_outlines(&self) -> bool {
        self.glyf.is_some() && self.loca.is_some()
    }

    /// A glyph's outline, in font units with y up.
    pub fn outline(&self, g: u16, out: &mut Vec<Cmd>) {
        self.outline_at(g, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], out, 0);
    }

    fn glyph_range(&self, g: u16) -> Option<(usize, usize)> {
        let (loca, (glyf, glen)) = (self.loca?, self.glyf?);
        if g >= self.num_glyphs && self.num_glyphs > 0 {
            return None;
        }
        let (a, b) = if self.long_loca {
            (u32_at(&self.data, loca + g as usize * 4)? as usize, u32_at(&self.data, loca + g as usize * 4 + 4)? as usize)
        } else {
            (u16_at(&self.data, loca + g as usize * 2)? as usize * 2, u16_at(&self.data, loca + g as usize * 2 + 2)? as usize * 2)
        };
        (b > a && b <= glen).then_some((glyf + a, glyf + b))
    }

    fn outline_at(&self, g: u16, m: [f32; 6], out: &mut Vec<Cmd>, depth: u8) {
        if depth > 8 {
            return;
        }
        let Some((start, end)) = self.glyph_range(g) else { return };
        let d = &self.data[..end];
        let Some(contours) = i16_at(d, start) else { return };
        if contours >= 0 {
            let _ = simple(d, start, contours as usize, &m, out);
            return;
        }
        // A compound glyph: other glyphs, each moved and scaled.
        let mut o = start + 10;
        loop {
            let (Some(flags), Some(child)) = (u16_at(d, o), u16_at(d, o + 2)) else { return };
            o += 4;
            let (dx, dy) = if flags & 1 != 0 {
                let r = (i16_at(d, o), i16_at(d, o + 2));
                o += 4;
                (r.0.unwrap_or(0) as f32, r.1.unwrap_or(0) as f32)
            } else {
                let r = (d.get(o).map(|&v| v as i8), d.get(o + 1).map(|&v| v as i8));
                o += 2;
                (r.0.unwrap_or(0) as f32, r.1.unwrap_or(0) as f32)
            };
            // (Point-matched placement, rare, is left at the origin.)
            let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
            let f2 = |o: usize| i16_at(d, o).unwrap_or(0) as f32 / 16384.0;
            let (a, b, c, e) = if flags & 8 != 0 {
                o += 2;
                (f2(o - 2), 0.0, 0.0, f2(o - 2))
            } else if flags & 0x40 != 0 {
                o += 4;
                (f2(o - 4), 0.0, 0.0, f2(o - 2))
            } else if flags & 0x80 != 0 {
                o += 8;
                (f2(o - 8), f2(o - 6), f2(o - 4), f2(o - 2))
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            // The child's transform, then ours.
            let child_m = [a, b, c, e, dx, dy];
            let mm = [
                m[0] * child_m[0] + m[2] * child_m[1],
                m[1] * child_m[0] + m[3] * child_m[1],
                m[0] * child_m[2] + m[2] * child_m[3],
                m[1] * child_m[2] + m[3] * child_m[3],
                m[0] * child_m[4] + m[2] * child_m[5] + m[4],
                m[1] * child_m[4] + m[3] * child_m[5] + m[5],
            ];
            self.outline_at(child, mm, out, depth + 1);
            if flags & 0x20 == 0 {
                return;
            }
        }
    }
}

/// The best character map the file has: full Unicode, then the basic plane, then anything.
fn best_cmap(d: &[u8], c: usize) -> Option<(usize, u16)> {
    let n = u16_at(d, c + 2)? as usize;
    let mut best: Option<(u8, usize, u16)> = None;
    for k in 0..n {
        let r = c + 4 + k * 8;
        let (pid, eid, off) = (u16_at(d, r)?, u16_at(d, r + 2)?, u32_at(d, r + 4)? as usize);
        let o = c + off;
        let format = u16_at(d, o)?;
        let rank = match (pid, eid, format) {
            (3, 10, 12) | (0, 4 | 6, 12) => 5,
            (3, 1, 4) | (0, 3, 4) => 4,
            (0, _, 4) => 3,
            (3, 0, 4) => 2,
            (_, _, 0 | 4 | 6 | 12) => 1,
            _ => 0,
        };
        if rank > 0 && best.is_none_or(|b| rank > b.0) {
            best = Some((rank, o, format));
        }
    }
    best.map(|(_, o, f)| (o, f))
}

/// A simple glyph's contours (quadratic B-splines with implied on-curve midpoints).
fn simple(d: &[u8], start: usize, contours: usize, m: &[f32; 6], out: &mut Vec<Cmd>) -> Option<()> {
    let mut ends = Vec::with_capacity(contours);
    for k in 0..contours {
        ends.push(u16_at(d, start + 10 + k * 2)? as usize);
    }
    let n = ends.last().map(|e| e + 1).unwrap_or(0);
    if n > 65535 || ends.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }
    let ins = u16_at(d, start + 10 + contours * 2)? as usize;
    let mut o = start + 12 + contours * 2 + ins;
    let mut flags = Vec::with_capacity(n);
    while flags.len() < n {
        let f = *d.get(o)?;
        o += 1;
        flags.push(f);
        if f & 8 != 0 {
            let r = *d.get(o)?;
            o += 1;
            for _ in 0..r {
                if flags.len() < n {
                    flags.push(f);
                }
            }
        }
    }
    let mut coords = |short: u8, same: u8| -> Option<Vec<i32>> {
        let mut v = Vec::with_capacity(n);
        let mut cur = 0i32;
        for &f in &flags {
            if f & short != 0 {
                let b = *d.get(o)? as i32;
                o += 1;
                cur += if f & same != 0 { b } else { -b };
            } else if f & same == 0 {
                cur += i16_at(d, o)? as i32;
                o += 2;
            }
            v.push(cur);
        }
        Some(v)
    };
    let xs = coords(2, 16)?;
    let ys = coords(4, 32)?;
    let at = |k: usize| {
        let (x, y) = (xs[k] as f32, ys[k] as f32);
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    };
    let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let mut first = 0;
    for &last in &ends {
        if last < first {
            first = last + 1;
            continue;
        }
        let len = last - first + 1;
        let pt = |k: usize| at(first + k % len);
        let on = |k: usize| flags[first + k % len] & 1 != 0;
        // Start on an on-curve point (or between two off-curve ones).
        let (start_pt, mut ctrl, begin, stop) = match (0..len).find(|&k| on(k)) {
            Some(s) => (pt(s), None, s + 1, s + len),
            None => (mid(pt(0), pt(1)), Some(pt(1)), 2, len + 1),
        };
        out.push(Cmd::Move(start_pt.0, start_pt.1));
        for k in begin..=stop {
            let closing = k == stop;
            let p = if closing { start_pt } else { pt(k) };
            let is_on = closing || on(k);
            match (is_on, ctrl) {
                (true, None) => out.push(Cmd::Line(p.0, p.1)),
                (true, Some(c)) => {
                    out.push(Cmd::Quad(c.0, c.1, p.0, p.1));
                    ctrl = None;
                }
                (false, None) => ctrl = Some(p),
                (false, Some(c)) => {
                    let mp = mid(c, p);
                    out.push(Cmd::Quad(c.0, c.1, mp.0, mp.1));
                    ctrl = Some(p);
                }
            }
        }
        out.push(Cmd::Close);
        first = last + 1;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny TrueType font, built here: glyph 1 is a 1000-unit square for 'A',
    /// glyph 2 a triangle with an off-curve point for 'B', glyph 3 both via a compound for 'C'.
    pub(crate) fn tiny_font() -> Vec<u8> {
        let mut glyf = Vec::new();
        let mut loca = vec![0u32];
        // Glyph 0: empty.
        loca.push(glyf.len() as u32);
        // Glyph 1: square 0,0 → 1000,1000 (on-curve, word coordinates).
        let square: Vec<u8> = {
            let mut g = Vec::new();
            for v in [1i16, 0, 0, 1000, 1000] {
                g.extend(v.to_be_bytes());
            }
            g.extend(3u16.to_be_bytes()); // end point of contour 0
            g.extend(0u16.to_be_bytes()); // no instructions
            g.extend([1u8; 4]); // on-curve, long x and y
            for x in [0i16, 1000, 0, -1000] {
                g.extend(x.to_be_bytes());
            }
            for y in [0i16, 0, 1000, 0] {
                g.extend(y.to_be_bytes());
            }
            g
        };
        glyf.extend(&square);
        loca.push(glyf.len() as u32);
        // Glyph 2: (0,0) on, (500,1000) off, (1000,0) on.
        let mut g = Vec::new();
        for v in [1i16, 0, 0, 1000, 1000] {
            g.extend(v.to_be_bytes());
        }
        g.extend(2u16.to_be_bytes());
        g.extend(0u16.to_be_bytes());
        g.extend([1u8, 0, 1]);
        for x in [0i16, 500, 500] {
            g.extend(x.to_be_bytes());
        }
        for y in [0i16, 1000, -1000] {
            g.extend(y.to_be_bytes());
        }
        glyf.extend(&g);
        loca.push(glyf.len() as u32);
        // Glyph 3: compound of glyph 1 moved by (100, 50).
        let mut g = Vec::new();
        for v in [-1i16, 0, 0, 1100, 1050] {
            g.extend(v.to_be_bytes());
        }
        g.extend((1u16 | 2).to_be_bytes());
        g.extend(1u16.to_be_bytes());
        g.extend(100i16.to_be_bytes());
        g.extend(50i16.to_be_bytes());
        glyf.extend(&g);
        loca.push(glyf.len() as u32);
        let loca: Vec<u8> = loca.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        head[50..52].copy_from_slice(&1i16.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
        hhea[34..36].copy_from_slice(&4u16.to_be_bytes());
        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&4u16.to_be_bytes());
        let hmtx: Vec<u8> = [500u16, 1000, 1000, 1200].iter().flat_map(|a| [a.to_be_bytes(), [0, 0]].concat()).collect();
        // cmap format 4: 'A'..'C' → 1..3, and the 0xFFFF end segment.
        let mut sub = Vec::new();
        for v in [4u16, 32, 0, 4, 4, 1, 0] {
            sub.extend(v.to_be_bytes());
        }
        for v in [0x43u16, 0xFFFF, 0, 0x41, 0xFFFF, (1i16 - 0x41) as u16, 1, 0, 0] {
            sub.extend(v.to_be_bytes());
        }
        let mut cmap = Vec::new();
        for v in [0u16, 1, 3, 1] {
            cmap.extend(v.to_be_bytes());
        }
        cmap.extend(12u32.to_be_bytes());
        cmap.extend(sub);
        // kern: 'A' then 'B' closer by 100.
        let mut kern = Vec::new();
        for v in [0u16, 1, 0, 20, 1, 1, 0, 0, 0, 1, 2] {
            kern.extend(v.to_be_bytes());
        }
        kern.extend((-100i16).to_be_bytes());
        let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"cmap", cmap), (b"glyf", glyf), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx), (b"kern", kern), (b"loca", loca), (b"maxp", maxp)];
        let mut out = Vec::new();
        out.extend(0x0001_0000u32.to_be_bytes());
        out.extend((tables.len() as u16).to_be_bytes());
        out.extend([0u8; 6]);
        let mut off = 12 + tables.len() * 16;
        let mut body = Vec::new();
        for (tag, t) in &tables {
            out.extend(*tag);
            out.extend(0u32.to_be_bytes());
            out.extend((off as u32).to_be_bytes());
            out.extend((t.len() as u32).to_be_bytes());
            body.extend(t);
            while body.len() % 4 != 0 {
                body.push(0);
            }
            off = 12 + tables.len() * 16 + body.len();
        }
        out.extend(body);
        out
    }

    #[test]
    fn reads_metrics_cmap_and_outlines() {
        let f = Face::parse(&tiny_font()).unwrap();
        assert_eq!((f.units_per_em, f.ascent, f.descent), (1000, 800, -200));
        assert_eq!((f.glyph('A'), f.glyph('B'), f.glyph('C'), f.glyph('Z')), (1, 2, 3, 0));
        assert_eq!((f.advance(1), f.advance(3)), (1000, 1200));
        assert_eq!(f.kerning(1, 2), -100);
        assert_eq!(f.kerning(2, 1), 0);
        let mut o = Vec::new();
        f.outline(1, &mut o);
        assert_eq!(o, vec![Cmd::Move(0.0, 0.0), Cmd::Line(1000.0, 0.0), Cmd::Line(1000.0, 1000.0), Cmd::Line(0.0, 1000.0), Cmd::Line(0.0, 0.0), Cmd::Close]);
        o.clear();
        f.outline(2, &mut o);
        assert_eq!(o, vec![Cmd::Move(0.0, 0.0), Cmd::Quad(500.0, 1000.0, 1000.0, 0.0), Cmd::Line(0.0, 0.0), Cmd::Close]);
        o.clear();
        f.outline(3, &mut o);
        assert_eq!(o[0], Cmd::Move(100.0, 50.0));
        assert_eq!(o[2], Cmd::Line(1100.0, 1050.0));
    }
}
