//! Paths for the canvas: building them (lines, curves, arcs), turning strokes
//! into outlines, and rasterising outlines into coverage masks.
//!
//! Points are stored in device pixels, transformed as they're added (as the
//! canvas spec says); curves are flattened into lines a quarter of a pixel
//! from the true curve at most.

use std::f32::consts::{PI, TAU};

/// An affine transform `[a, b, c, d, e, f]`: x' = a x + c y + e, y' = b x + d y + f.
pub type Matrix = [f32; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub fn apply(m: &Matrix, (x, y): (f32, f32)) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// `m` then `n` (n applied after m): the matrix for "transform by n inside m".
pub fn multiply(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

pub fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

/// How much `m` scales lengths, at most (for choosing how finely to flatten).
pub fn max_scale(m: &Matrix) -> f32 {
    let sx = (m[0] * m[0] + m[1] * m[1]).sqrt();
    let sy = (m[2] * m[2] + m[3] * m[3]).sqrt();
    sx.max(sy).max(1e-6)
}

/// A polygon (flattened subpath): its points, and whether it's closed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sub {
    pub pts: Vec<(f32, f32)>,
    pub closed: bool,
}

/// One piece of a subpath, ending at its last point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Line((f32, f32)),
    Quad((f32, f32), (f32, f32)),
    Cubic((f32, f32), (f32, f32), (f32, f32)),
}

impl Seg {
    fn end(&self) -> (f32, f32) {
        match *self {
            Seg::Line(p) | Seg::Quad(_, p) | Seg::Cubic(_, _, p) => p,
        }
    }
    fn map(&self, m: &Matrix) -> Seg {
        match *self {
            Seg::Line(p) => Seg::Line(apply(m, p)),
            Seg::Quad(c, p) => Seg::Quad(apply(m, c), apply(m, p)),
            Seg::Cubic(a, b, p) => Seg::Cubic(apply(m, a), apply(m, b), apply(m, p)),
        }
    }
}

/// A subpath: where it starts, its lines and curves, and whether it's closed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contour {
    pub start: (f32, f32),
    pub segs: Vec<Seg>,
    pub closed: bool,
}

/// A path being built, in device pixels (curves kept as curves until it's drawn).
#[derive(Debug, Clone, Default)]
pub struct Path {
    pub subs: Vec<Contour>,
    /// Whether there's a subpath going (lines join it).
    open: bool,
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

/// The angle between two directions (0 if either is a point).
fn turn(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (la, lb) = ((a.0 * a.0 + a.1 * a.1).sqrt(), (b.0 * b.0 + b.1 * b.1).sqrt());
    if la < 1e-9 || lb < 1e-9 {
        return 0.0;
    }
    ((a.0 * b.0 + a.1 * b.1) / (la * lb)).clamp(-1.0, 1.0).acos()
}

/// Segments for an arc of `angle` radians on a circle `radius` device pixels across, within 1/4 px.
fn arc_steps(radius: f32, angle: f32) -> usize {
    if radius <= 0.25 {
        return 1.max((angle.abs() / (PI / 2.0)).ceil() as usize);
    }
    let step = 2.0 * (1.0 - 0.25 / radius).clamp(-1.0, 1.0).acos();
    ((angle.abs() / step.max(1e-3)).ceil() as usize).clamp(1, 2048)
}

/// Unit-circle arc from `a0` sweeping `sweep` as cubic Béziers (pieces of at most a quarter turn).
fn arc_cubics(a0: f32, sweep: f32) -> Vec<[(f32, f32); 3]> {
    let n = ((sweep.abs() / (PI / 2.0)).ceil() as usize).clamp(1, 64);
    let step = sweep / n as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    (0..n)
        .map(|i| {
            let (a, b) = (a0 + step * i as f32, a0 + step * (i + 1) as f32);
            let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
            [(ca - k * sa, sa + k * ca), (cb + k * sb, sb - k * cb), (cb, sb)]
        })
        .collect()
}

/// How a path is flattened: within `tol` of the curve, and turning at most `max_turn` per line.
#[derive(Clone, Copy)]
pub struct Flatness {
    pub tol: f32,
    pub max_turn: f32,
}

pub const FILL: Flatness = Flatness { tol: 0.2, max_turn: 0.5 };

impl Contour {
    fn last(&self) -> (f32, f32) {
        self.segs.last().map(|s| s.end()).unwrap_or(self.start)
    }

    fn flatten(&self, f: Flatness) -> Sub {
        let mut pts = vec![self.start];
        let mut cur = self.start;
        for seg in &self.segs {
            match *seg {
                Seg::Line(p) => pts.push(p),
                Seg::Quad(c, p) => {
                    let dd = dist((0.0, 0.0), (cur.0 - 2.0 * c.0 + p.0, cur.1 - 2.0 * c.1 + p.1));
                    let by_dev = (0.25 * dd / f.tol).sqrt().ceil();
                    let by_turn = (turn((c.0 - cur.0, c.1 - cur.1), (p.0 - c.0, p.1 - c.1)) / f.max_turn).ceil();
                    let n = (by_dev.max(by_turn) as usize).clamp(1, 10_000);
                    for k in 1..=n {
                        let t = k as f32 / n as f32;
                        let u = 1.0 - t;
                        pts.push((u * u * cur.0 + 2.0 * u * t * c.0 + t * t * p.0, u * u * cur.1 + 2.0 * u * t * c.1 + t * t * p.1));
                    }
                }
                Seg::Cubic(a, b, p) => {
                    let d1 = dist((0.0, 0.0), (cur.0 - 2.0 * a.0 + b.0, cur.1 - 2.0 * a.1 + b.1));
                    let d2 = dist((0.0, 0.0), (a.0 - 2.0 * b.0 + p.0, a.1 - 2.0 * b.1 + p.1));
                    let by_dev = (0.75 * d1.max(d2) / f.tol).sqrt().ceil();
                    let bend = turn((a.0 - cur.0, a.1 - cur.1), (b.0 - a.0, b.1 - a.1)) + turn((b.0 - a.0, b.1 - a.1), (p.0 - b.0, p.1 - b.1));
                    let by_turn = (bend / f.max_turn).ceil();
                    let n = (by_dev.max(by_turn) as usize).clamp(1, 10_000);
                    for k in 1..=n {
                        let t = k as f32 / n as f32;
                        let u = 1.0 - t;
                        let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                        pts.push((w0 * cur.0 + w1 * a.0 + w2 * b.0 + w3 * p.0, w0 * cur.1 + w1 * a.1 + w2 * b.1 + w3 * p.1));
                    }
                }
            }
            cur = seg.end();
        }
        Sub { pts, closed: self.closed }
    }
}

impl Path {
    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    /// The path as polygons, flattened as `f` says.
    pub fn flat(&self, f: Flatness) -> Vec<Sub> {
        self.subs.iter().map(|c| c.flatten(f)).collect()
    }

    /// The path as polygons, for filling, clipping and hit tests.
    pub fn polys(&self) -> Vec<Sub> {
        self.flat(FILL)
    }

    fn last(&self) -> Option<(f32, f32)> {
        if !self.open {
            return None;
        }
        self.subs.last().map(|s| s.last())
    }

    /// Starts a subpath at a device point.
    pub fn move_dev(&mut self, p: (f32, f32)) {
        if let Some(s) = self.subs.last_mut() {
            // A subpath that's only its first point says nothing once another starts.
            if s.segs.is_empty() && !s.closed {
                s.start = p;
                self.open = true;
                return;
            }
        }
        self.subs.push(Contour { start: p, segs: Vec::new(), closed: false });
        self.open = true;
    }

    fn seg(&mut self, s: Seg) {
        match self.subs.last_mut() {
            Some(c) if self.open => c.segs.push(s),
            _ => self.move_dev(s.end()),
        }
    }

    pub fn line_dev(&mut self, p: (f32, f32)) {
        if !self.open {
            self.move_dev(p);
            return;
        }
        self.seg(Seg::Line(p));
    }

    pub fn quad_dev(&mut self, c: (f32, f32), p: (f32, f32)) {
        if self.last().is_none() {
            self.move_dev(c);
        }
        self.seg(Seg::Quad(c, p));
    }

    pub fn move_to(&mut self, m: &Matrix, x: f32, y: f32) {
        self.move_dev(apply(m, (x, y)));
    }

    pub fn line_to(&mut self, m: &Matrix, x: f32, y: f32) {
        self.line_dev(apply(m, (x, y)));
    }

    pub fn quad_to(&mut self, m: &Matrix, cx: f32, cy: f32, x: f32, y: f32) {
        if self.last().is_none() {
            self.move_to(m, cx, cy);
        }
        self.seg(Seg::Quad(apply(m, (cx, cy)), apply(m, (x, y))));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cubic_to(&mut self, m: &Matrix, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if self.last().is_none() {
            self.move_to(m, c1x, c1y);
        }
        self.seg(Seg::Cubic(apply(m, (c1x, c1y)), apply(m, (c2x, c2y)), apply(m, (x, y))));
    }

    /// An arc of the ellipse centred at (x, y), from `a0` sweeping `sweep` radians, as curves;
    /// a line joins it to the subpath before.
    #[allow(clippy::too_many_arguments)]
    fn arc_curves(&mut self, m: &Matrix, x: f32, y: f32, rx: f32, ry: f32, rot: f32, a0: f32, sweep: f32) {
        let (cr, sr) = (rot.cos(), rot.sin());
        let at = |(ux, uy): (f32, f32)| {
            let (ex, ey) = (rx * ux, ry * uy);
            apply(m, (x + ex * cr - ey * sr, y + ex * sr + ey * cr))
        };
        let start = at((a0.cos(), a0.sin()));
        if self.last().is_none() {
            self.move_dev(start);
        } else {
            self.line_dev(start);
        }
        if sweep == 0.0 || (rx == 0.0 && ry == 0.0) {
            return;
        }
        for [a, b, p] in arc_cubics(a0, sweep) {
            self.seg(Seg::Cubic(at(a), at(b), at(p)));
        }
    }

    /// `ellipse()` (and `arc()`), with the canvas spec's sweep rules.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(&mut self, m: &Matrix, x: f32, y: f32, rx: f32, ry: f32, rot: f32, a0: f32, a1: f32, ccw: bool) {
        let mut sweep = a1 - a0;
        if !ccw && sweep >= TAU || ccw && -sweep >= TAU {
            sweep = if ccw { -TAU } else { TAU };
        } else {
            if !ccw && sweep < 0.0 {
                sweep = sweep.rem_euclid(TAU);
            }
            if ccw && sweep > 0.0 {
                sweep = -((-sweep).rem_euclid(TAU));
            }
        }
        self.arc_curves(m, x, y, rx, ry, rot, a0, sweep);
    }

    /// `arcTo()`: a line to where a circle of `r` meets both lines, then around it.
    pub fn arc_to(&mut self, m: &Matrix, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) {
        let Some(inv) = invert(m) else { return };
        let Some(p0d) = self.last() else {
            self.move_to(m, x1, y1);
            return;
        };
        let p0 = apply(&inv, p0d);
        let (d0, d2) = ((p0.0 - x1, p0.1 - y1), (x2 - x1, y2 - y1));
        let cross = d0.0 * d2.1 - d0.1 * d2.0;
        let (l0, l2) = ((d0.0 * d0.0 + d0.1 * d0.1).sqrt(), (d2.0 * d2.0 + d2.1 * d2.1).sqrt());
        if l0 < 1e-6 || l2 < 1e-6 || r == 0.0 || (cross / (l0 * l2)).abs() < 1e-6 {
            self.line_to(m, x1, y1);
            return;
        }
        let (u0, u2) = ((d0.0 / l0, d0.1 / l0), (d2.0 / l2, d2.1 / l2));
        let cos = (u0.0 * u2.0 + u0.1 * u2.1).clamp(-1.0, 1.0);
        let half = cos.acos() / 2.0;
        let dist = r / half.tan();
        let t0 = (x1 + u0.0 * dist, y1 + u0.1 * dist);
        let t2 = (x1 + u2.0 * dist, y1 + u2.1 * dist);
        // The centre is along the bisector.
        let bis = (u0.0 + u2.0, u0.1 + u2.1);
        let bl = (bis.0 * bis.0 + bis.1 * bis.1).sqrt();
        let cd = r / half.sin();
        let c = (x1 + bis.0 / bl * cd, y1 + bis.1 / bl * cd);
        let a0 = (t0.1 - c.1).atan2(t0.0 - c.0);
        let a1 = (t2.1 - c.1).atan2(t2.0 - c.0);
        let mut sweep = a1 - a0;
        // The short way round, on the side the corner turns.
        if cross > 0.0 {
            if sweep > 0.0 {
                sweep -= TAU;
            }
        } else if sweep < 0.0 {
            sweep += TAU;
        }
        self.line_to(m, t0.0, t0.1);
        self.arc_curves(m, c.0, c.1, r, r, 0.0, a0, sweep);
    }

    pub fn rect(&mut self, m: &Matrix, x: f32, y: f32, w: f32, h: f32) {
        self.move_to(m, x, y);
        self.line_to(m, x + w, y);
        self.line_to(m, x + w, y + h);
        self.line_to(m, x, y + h);
        self.close();
        self.move_to(m, x, y);
    }

    /// `roundRect()` with up to four radii (each a circle or an ellipse).
    pub fn round_rect(&mut self, m: &Matrix, x: f32, y: f32, w: f32, h: f32, radii: &[(f32, f32)]) {
        let r = match radii.len() {
            1 => [radii[0]; 4],
            2 => [radii[0], radii[1], radii[0], radii[1]],
            3 => [radii[0], radii[1], radii[2], radii[1]],
            4 => [radii[0], radii[1], radii[2], radii[3]],
            _ => [(0.0, 0.0); 4],
        };
        // A negative width or height flips the corners.
        let (x, w, r) = if w < 0.0 { (x + w, -w, [r[1], r[0], r[3], r[2]]) } else { (x, w, r) };
        let (y, h, mut r) = if h < 0.0 { (y + h, -h, [r[3], r[2], r[1], r[0]]) } else { (y, h, r) };
        // Radii too big for the sides shrink together.
        let f = [w / (r[0].0 + r[1].0), w / (r[2].0 + r[3].0), h / (r[0].1 + r[3].1), h / (r[1].1 + r[2].1)]
            .into_iter()
            .filter(|v| v.is_finite())
            .fold(1.0f32, f32::min);
        if f < 1.0 {
            for c in &mut r {
                c.0 *= f;
                c.1 *= f;
            }
        }
        self.move_to(m, x + r[0].0, y);
        self.line_to(m, x + w - r[1].0, y);
        self.arc_curves(m, x + w - r[1].0, y + r[1].1, r[1].0, r[1].1, 0.0, -PI / 2.0, PI / 2.0);
        self.line_to(m, x + w, y + h - r[2].1);
        self.arc_curves(m, x + w - r[2].0, y + h - r[2].1, r[2].0, r[2].1, 0.0, 0.0, PI / 2.0);
        self.line_to(m, x + r[3].0, y + h);
        self.arc_curves(m, x + r[3].0, y + h - r[3].1, r[3].0, r[3].1, 0.0, PI / 2.0, PI / 2.0);
        self.line_to(m, x, y + r[0].1);
        self.arc_curves(m, x + r[0].0, y + r[0].1, r[0].0, r[0].1, 0.0, PI, PI / 2.0);
        self.close();
        self.move_to(m, x, y);
    }

    /// `closePath()`: back to the start; the next subpath starts there too.
    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        let Some(s) = self.subs.last_mut() else { return };
        s.closed = true;
        let start = s.start;
        self.subs.push(Contour { start, segs: Vec::new(), closed: false });
    }

    /// Adds another path's subpaths (already in device pixels).
    pub fn extend(&mut self, other: &Path) {
        for s in &other.subs {
            self.subs.push(s.clone());
        }
        self.open = other.open;
    }

    /// SVG path data (`new Path2D("M 0 0 L 10 10 Z")`), in user space through `m`.
    pub fn svg(&mut self, m: &Matrix, d: &str) {
        let mut tok = SvgTokens { s: d.as_bytes(), i: 0 };
        let (mut cur, mut start) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
        let mut prev_ctrl: Option<(char, (f32, f32))> = None;
        let mut cmd = ' ';
        loop {
            let c = match tok.command() {
                Some(c) => {
                    cmd = c;
                    c
                }
                None if tok.more_numbers() && cmd != ' ' => match cmd {
                    'M' => 'L',
                    'm' => 'l',
                    c => c,
                },
                None => break,
            };
            let rel = c.is_ascii_lowercase();
            let at = |p: (f32, f32), cur: (f32, f32)| if rel { (cur.0 + p.0, cur.1 + p.1) } else { p };
            macro_rules! n {
                () => {
                    match tok.number() {
                        Some(v) => v,
                        None => return,
                    }
                };
            }
            let mut ctrl = None;
            match c.to_ascii_uppercase() {
                'M' => {
                    let p = at((n!(), n!()), cur);
                    self.move_to(m, p.0, p.1);
                    cur = p;
                    start = p;
                    cmd = if rel { 'l' } else { 'L' };
                }
                'L' => {
                    let p = at((n!(), n!()), cur);
                    self.line_to(m, p.0, p.1);
                    cur = p;
                }
                'H' => {
                    let v = n!();
                    cur.0 = if rel { cur.0 + v } else { v };
                    self.line_to(m, cur.0, cur.1);
                }
                'V' => {
                    let v = n!();
                    cur.1 = if rel { cur.1 + v } else { v };
                    self.line_to(m, cur.0, cur.1);
                }
                'C' | 'S' => {
                    let c1 = if c.to_ascii_uppercase() == 'C' {
                        at((n!(), n!()), cur)
                    } else {
                        match prev_ctrl {
                            Some(('C', p)) => (2.0 * cur.0 - p.0, 2.0 * cur.1 - p.1),
                            _ => cur,
                        }
                    };
                    let c2 = at((n!(), n!()), cur);
                    let p = at((n!(), n!()), cur);
                    self.cubic_to(m, c1.0, c1.1, c2.0, c2.1, p.0, p.1);
                    ctrl = Some(('C', c2));
                    cur = p;
                }
                'Q' | 'T' => {
                    let c1 = if c.to_ascii_uppercase() == 'Q' {
                        at((n!(), n!()), cur)
                    } else {
                        match prev_ctrl {
                            Some(('Q', p)) => (2.0 * cur.0 - p.0, 2.0 * cur.1 - p.1),
                            _ => cur,
                        }
                    };
                    let p = at((n!(), n!()), cur);
                    self.quad_to(m, c1.0, c1.1, p.0, p.1);
                    ctrl = Some(('Q', c1));
                    cur = p;
                }
                'A' => {
                    let (rx, ry, rot) = (n!().abs(), n!().abs(), n!().to_radians());
                    let (large, sweep) = (tok.flag(), tok.flag());
                    let (Some(large), Some(sweep)) = (large, sweep) else { return };
                    let p = at((n!(), n!()), cur);
                    self.svg_arc(m, cur, rx, ry, rot, large, sweep, p);
                    cur = p;
                }
                'Z' => {
                    self.close();
                    cur = start;
                }
                _ => return,
            }
            prev_ctrl = ctrl;
        }
    }

    /// An SVG elliptical arc from `p0` to `p1` (endpoint form, converted to centre form).
    #[allow(clippy::too_many_arguments)]
    fn svg_arc(&mut self, m: &Matrix, p0: (f32, f32), mut rx: f32, mut ry: f32, rot: f32, large: bool, sweep: bool, p1: (f32, f32)) {
        if rx == 0.0 || ry == 0.0 || p0 == p1 {
            self.line_to(m, p1.0, p1.1);
            return;
        }
        let (c, s) = (rot.cos(), rot.sin());
        let (dx, dy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
        let (x1, y1) = (c * dx + s * dy, -s * dx + c * dy);
        let lam = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
        if lam > 1.0 {
            rx *= lam.sqrt();
            ry *= lam.sqrt();
        }
        let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let mut k = (num / den).sqrt();
        if large == sweep {
            k = -k;
        }
        let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
        let (cx, cy) = (c * cx1 - s * cy1 + (p0.0 + p1.0) / 2.0, s * cx1 + c * cy1 + (p0.1 + p1.1) / 2.0);
        let ang = |ux: f32, uy: f32| uy.atan2(ux);
        let a0 = ang((x1 - cx1) / rx, (y1 - cy1) / ry);
        let mut da = ang((-x1 - cx1) / rx, (-y1 - cy1) / ry) - a0;
        if sweep && da < 0.0 {
            da += TAU;
        } else if !sweep && da > 0.0 {
            da -= TAU;
        }
        self.arc_curves(m, cx, cy, rx, ry, rot, a0, da);
    }

    /// The path in another coordinate space (for stroking in user space).
    pub fn mapped(&self, m: &Matrix) -> Path {
        Path {
            subs: self.subs.iter().map(|c| Contour { start: apply(m, c.start), segs: c.segs.iter().map(|s| s.map(m)).collect(), closed: c.closed }).collect(),
            open: self.open,
        }
    }
}

struct SvgTokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl SvgTokens<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }
    fn command(&mut self) -> Option<char> {
        self.skip();
        let c = *self.s.get(self.i)? as char;
        if "MmLlHhVvCcSsQqTtAaZz".contains(c) {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }
    fn more_numbers(&mut self) -> bool {
        self.skip();
        self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
    }
    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let s = self.s;
        if self.i < s.len() && matches!(s[self.i], b'-' | b'+') {
            self.i += 1;
        }
        let mut dot = false;
        while self.i < s.len() && (s[self.i].is_ascii_digit() || s[self.i] == b'.' && !dot) {
            dot |= s[self.i] == b'.';
            self.i += 1;
        }
        if self.i < s.len() && matches!(s[self.i], b'e' | b'E') {
            let save = self.i;
            self.i += 1;
            if self.i < s.len() && matches!(s[self.i], b'-' | b'+') {
                self.i += 1;
            }
            if self.i < s.len() && s[self.i].is_ascii_digit() {
                while self.i < s.len() && s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        std::str::from_utf8(&s[start..self.i]).ok()?.parse().ok().filter(|v: &f32| v.is_finite())
    }
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ strokes

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

pub struct StrokeStyle<'a> {
    pub width: f32,
    pub cap: Cap,
    pub join: Join,
    pub miter: f32,
    pub dash: &'a [f32],
    pub dash_offset: f32,
}

/// A polygon, with positive area (so a nonzero fill of many is their union).
fn push_poly(out: &mut Vec<Sub>, mut pts: Vec<(f32, f32)>) {
    if pts.len() < 3 {
        return;
    }
    let mut area = 0.0;
    for k in 0..pts.len() {
        let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
        area += a.0 * b.1 - b.0 * a.1;
    }
    if area < 0.0 {
        pts.reverse();
    }
    if area != 0.0 {
        out.push(Sub { pts, closed: true });
    }
}

fn circle(out: &mut Vec<Sub>, c: (f32, f32), r: f32, scale: f32) {
    let n = arc_steps(r * scale, TAU).max(8);
    push_poly(out, (0..n).map(|k| {
        let t = TAU * k as f32 / n as f32;
        (c.0 + r * t.cos(), c.1 + r * t.sin())
    }).collect());
}

/// Splits subpaths into dashes (each an open subpath).
fn dashed(subs: &[Sub], dash: &[f32], offset: f32) -> Vec<Sub> {
    let total: f32 = dash.iter().sum();
    if total <= 0.0 {
        return subs.to_vec();
    }
    let mut out = Vec::new();
    for s in subs {
        let mut pts = s.pts.clone();
        if s.closed && pts.len() > 1 {
            pts.push(pts[0]);
        }
        // Where in the pattern the subpath starts.
        let mut pos = offset.rem_euclid(total);
        let mut idx = 0;
        while pos >= dash[idx] {
            pos -= dash[idx];
            idx = (idx + 1) % dash.len();
        }
        let mut left = dash[idx] - pos;
        let mut on = idx % 2 == 0;
        let mut cur: Vec<(f32, f32)> = if on { vec![pts[0]] } else { Vec::new() };
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            let mut done = 0.0;
            while len - done > left {
                done += left;
                let t = done / len;
                let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                if on {
                    cur.push(p);
                    out.push(Sub { pts: std::mem::take(&mut cur), closed: false });
                } else {
                    cur = vec![p];
                }
                on = !on;
                idx = (idx + 1) % dash.len();
                left = dash[idx];
                if out.len() > 100_000 {
                    return out;
                }
            }
            left -= len - done;
            if on {
                cur.push(b);
            }
        }
        if on && cur.len() > 1 {
            out.push(Sub { pts: cur, closed: false });
        }
    }
    out
}

/// The outline of a stroke along `path` (in the same space), as polygons to fill nonzero.
/// `scale` is how big a unit is in device pixels (for round parts).
pub fn stroke(path: &Path, st: &StrokeStyle, scale: f32) -> Vec<Sub> {
    let hw = st.width / 2.0;
    let mut out = Vec::new();
    if !(hw > 0.0) {
        return out;
    }
    // Flatter for wider lines: a line's ends and joins turn with the curve.
    let flat = Flatness { tol: 0.2 / scale, max_turn: (0.5 / (hw * scale)).clamp(0.004, 0.5) };
    let subs = path.flat(flat);
    let subs: Vec<Sub> = if st.dash.is_empty() { subs } else { dashed(&subs, st.dash, st.dash_offset) };
    for s in &subs {
        // Repeated points don't make segments.
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(s.pts.len());
        for &p in &s.pts {
            if pts.last().is_none_or(|q: &(f32, f32)| (q.0 - p.0).abs() > 1e-4 || (q.1 - p.1).abs() > 1e-4) {
                pts.push(p);
            }
        }
        let closed = s.closed && pts.len() > 2;
        if closed && pts.len() > 1 && (pts[0].0 - pts[pts.len() - 1].0).abs() < 1e-4 && (pts[0].1 - pts[pts.len() - 1].1).abs() < 1e-4 {
            pts.pop();
        }
        if pts.len() == 1 {
            // A dot: only round and square caps show (not for a subpath that's only a moveTo).
            if s.pts.len() > 1 {
                let p = pts[0];
                match st.cap {
                    Cap::Round => circle(&mut out, p, hw, scale),
                    Cap::Square => push_poly(&mut out, vec![(p.0 - hw, p.1 - hw), (p.0 + hw, p.1 - hw), (p.0 + hw, p.1 + hw), (p.0 - hw, p.1 + hw)]),
                    Cap::Butt => {}
                }
            }
            continue;
        }
        if pts.len() < 2 {
            continue;
        }
        let n = pts.len();
        let segs = if closed { n } else { n - 1 };
        let dir = |k: usize| {
            let (a, b) = (pts[k % n], pts[(k + 1) % n]);
            let l = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            ((b.0 - a.0) / l, (b.1 - a.1) / l)
        };
        for k in 0..segs {
            let (a, b) = (pts[k], pts[(k + 1) % n]);
            let d = dir(k);
            let nrm = (-d.1 * hw, d.0 * hw);
            let (mut a, mut b) = (a, b);
            if !closed && st.cap == Cap::Square {
                if k == 0 {
                    a = (a.0 - d.0 * hw, a.1 - d.1 * hw);
                }
                if k == segs - 1 {
                    b = (b.0 + d.0 * hw, b.1 + d.1 * hw);
                }
            }
            push_poly(&mut out, vec![(a.0 + nrm.0, a.1 + nrm.1), (b.0 + nrm.0, b.1 + nrm.1), (b.0 - nrm.0, b.1 - nrm.1), (a.0 - nrm.0, a.1 - nrm.1)]);
        }
        // Joins, at every vertex between two segments.
        let joints: Vec<usize> = if closed { (0..n).collect() } else { (1..n - 1).collect() };
        for v in joints {
            let (d0, d1) = (dir((v + n - 1) % n), dir(v));
            let p = pts[v];
            let cross = d0.0 * d1.1 - d0.1 * d1.0;
            let dot = d0.0 * d1.0 + d0.1 * d1.1;
            if cross.abs() < 1e-6 && dot > 0.0 {
                continue;
            }
            if st.join == Join::Round {
                circle(&mut out, p, hw, scale);
                continue;
            }
            let side = if cross > 0.0 { -1.0 } else { 1.0 };
            let o0 = (-d0.1 * side, d0.0 * side);
            let o1 = (-d1.1 * side, d1.0 * side);
            let e0 = (p.0 + o0.0 * hw, p.1 + o0.1 * hw);
            let e1 = (p.0 + o1.0 * hw, p.1 + o1.1 * hw);
            let sum = (o0.0 + o1.0, o0.1 + o1.1);
            let sl = (sum.0 * sum.0 + sum.1 * sum.1).sqrt();
            if st.join == Join::Miter && sl > 1e-6 && 2.0 / sl <= st.miter {
                let k = hw * 2.0 / (sl * sl);
                push_poly(&mut out, vec![p, e0, (p.0 + sum.0 * k, p.1 + sum.1 * k), e1]);
            } else {
                push_poly(&mut out, vec![p, e0, e1]);
            }
        }
        if !closed && st.cap == Cap::Round {
            circle(&mut out, pts[0], hw, scale);
            circle(&mut out, pts[n - 1], hw, scale);
        }
    }
    out
}

// ------------------------------------------------------------- rasterising

/// Coverage, 0 to 1, over part of the canvas.
pub struct Mask {
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub cov: Vec<f32>,
}

impl Mask {
    pub fn at(&self, x: i32, y: i32) -> f32 {
        let (dx, dy) = (x - self.x, y - self.y);
        if dx < 0 || dy < 0 || dx as usize >= self.w || dy as usize >= self.h {
            return 0.0;
        }
        self.cov[dy as usize * self.w + dx as usize]
    }
}

/// Vertical samples per pixel row (horizontal coverage is exact).
const SUB: usize = 16;

struct Edge {
    x0: f32,
    y0: f32,
    dxdy: f32,
    y1: f32,
    dir: i32,
}

/// Fills `subs` (each closed implicitly) into a mask clipped to `width` x `height`.
pub fn fill(subs: &[Sub], evenodd: bool, width: u32, height: u32) -> Option<Mask> {
    fill_in(subs, evenodd, (0, 0, width as i32, height as i32))
}

/// Coverage of the polygons within `bounds` (x0, y0, x1, y1), which may reach past the canvas.
pub fn fill_in(subs: &[Sub], evenodd: bool, bounds: (i32, i32, i32, i32)) -> Option<Mask> {
    let mut edges = Vec::new();
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in subs {
        let n = s.pts.len();
        if n < 2 {
            continue;
        }
        for k in 0..n {
            let (a, b) = (s.pts[k], s.pts[(k + 1) % n]);
            if !(a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite()) {
                return None;
            }
            minx = minx.min(a.0);
            maxx = maxx.max(a.0);
            miny = miny.min(a.1);
            maxy = maxy.max(a.1);
            if a.1 == b.1 {
                continue;
            }
            let (top, bot, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
            edges.push(Edge { x0: top.0, y0: top.1, dxdy: (bot.0 - top.0) / (bot.1 - top.1), y1: bot.1, dir });
        }
    }
    if edges.is_empty() {
        return None;
    }
    let x0 = (minx.floor().max(bounds.0 as f32)) as i32;
    let y0 = (miny.floor().max(bounds.1 as f32)) as i32;
    let x1 = (maxx.ceil().min(bounds.2 as f32)) as i32;
    let y1 = (maxy.ceil().min(bounds.3 as f32)) as i32;
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut cov = vec![0f32; w * h];
    edges.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal));
    let mut next = 0;
    let mut active: Vec<usize> = Vec::new();
    let mut xs: Vec<(f32, i32)> = Vec::new();
    let mut acc = vec![0f32; w + 1];
    let wgt = 1.0 / SUB as f32;
    for row in 0..h {
        let py = (y0 + row as i32) as f32;
        let line = &mut cov[row * w..(row + 1) * w];
        acc.iter_mut().for_each(|v| *v = 0.0);
        for sub in 0..SUB {
            let sy = py + (sub as f32 + 0.5) / SUB as f32;
            while next < edges.len() && edges[next].y0 <= sy {
                active.push(next);
                next += 1;
            }
            active.retain(|&e| edges[e].y1 > sy);
            xs.clear();
            for &e in &active {
                let ed = &edges[e];
                if ed.y0 <= sy {
                    xs.push((ed.x0 + (sy - ed.y0) * ed.dxdy - x0 as f32, ed.dir));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut wind = 0;
            for k in 0..xs.len() - 1 {
                wind += xs[k].1;
                let inside = if evenodd { wind % 2 != 0 } else { wind != 0 };
                if !inside {
                    continue;
                }
                let (a, b) = (xs[k].0.max(0.0), xs[k + 1].0.min(w as f32));
                if a >= b {
                    continue;
                }
                let (ia, ib) = (a as usize, (b as usize).min(w));
                if ia == ib || ia + 1 > w {
                    if ia < w {
                        line[ia] += (b - a) * wgt;
                    }
                    continue;
                }
                line[ia] += (ia as f32 + 1.0 - a) * wgt;
                acc[ia + 1] += wgt;
                acc[ib] -= wgt;
                if ib < w {
                    line[ib] += (b - ib as f32) * wgt;
                }
            }
        }
        let mut run = 0.0;
        for (x, v) in line.iter_mut().enumerate() {
            run += acc[x];
            *v = (*v + run).clamp(0.0, 1.0);
        }
    }
    Some(Mask { x: x0, y: y0, w, h, cov })
}

/// Is a point inside `subs` (points on an edge count)?
pub fn contains(subs: &[Sub], (px, py): (f32, f32), evenodd: bool) -> bool {
    let mut wind = 0;
    for s in subs {
        let n = s.pts.len();
        for k in 0..n {
            let (a, b) = (s.pts[k], s.pts[(k + 1) % n]);
            // On the edge itself.
            let cross = (b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0);
            if cross.abs() < 1e-3 && px >= a.0.min(b.0) - 1e-3 && px <= a.0.max(b.0) + 1e-3 && py >= a.1.min(b.1) - 1e-3 && py <= a.1.max(b.1) + 1e-3 {
                return true;
            }
            if a.1 <= py {
                if b.1 > py && cross > 0.0 {
                    wind += 1;
                }
            } else if b.1 <= py && cross < 0.0 {
                wind -= 1;
            }
        }
    }
    if evenodd {
        wind % 2 != 0
    } else {
        wind != 0
    }
}
