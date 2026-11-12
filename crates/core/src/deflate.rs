//! DEFLATE (RFC 1951) in both directions, and gzip (RFC 1952) reading.
//!
//! Inflating unpacks updates (zip) and compressed web responses (gzip).
//! Deflating packs the release zip: LZ77 over a 32 KB window with hash chains
//! and lazy matching, then per-block Huffman codes built by package-merge
//! (length-limited, so always valid), falling back to stored blocks when the
//! data doesn't compress (the icon catalogue is already compressed).

use crate::hash::crc32_update;

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] =
    [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// The order code-length code lengths are sent in.
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The data ends before the stream does.
    Truncated,
    /// Not valid DEFLATE / gzip.
    Corrupt,
    /// It would unpack to more than the limit given.
    TooBig,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Truncated => "the compressed data is cut short",
            Error::Corrupt => "the compressed data is damaged",
            Error::TooBig => "it unpacks to more than allowed",
        })
    }
}

// ------------------------------------------------------------------ inflate

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, buf: 0, n: 0 }
    }
    fn refill(&mut self) {
        while self.n <= 56 {
            // Past the end reads zeros; `overrun` notices.
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.buf |= (b as u64) << self.n;
            self.n += 8;
        }
    }
    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        if self.n < k {
            self.refill();
        }
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.buf >>= k;
        self.n -= k;
        v
    }
    /// Bytes fully or partly consumed so far.
    fn consumed(&self) -> usize {
        (self.pos * 8 - self.n as usize).div_ceil(8)
    }
    fn overrun(&self) -> bool {
        self.pos * 8 - self.n as usize > self.data.len() * 8
    }
    fn align(&mut self) {
        let drop = self.n % 8;
        self.buf >>= drop;
        self.n -= drop;
    }
}

/// A canonical Huffman code as a lookup table indexed by the next `bits` bits.
struct Huff {
    /// symbol << 4 | length; length 0 marks a code that doesn't exist.
    table: Vec<u16>,
    bits: u32,
}

fn reverse(code: u32, len: u32) -> u32 {
    code.reverse_bits() >> (32 - len)
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut count = [0u32; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let max = (1..16).rev().find(|&l| count[l] > 0).unwrap_or(0) as u32;
        // Over-subscribed codes are invalid; incomplete ones are allowed (a lone distance code).
        let mut left: i64 = 1;
        for &c in &count[1..] {
            left = left * 2 - c as i64;
            if left < 0 {
                return Err(Error::Corrupt);
            }
        }
        let mut next = [0u32; 16];
        let mut code = 0;
        for len in 1..16 {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        let size = 1usize << max;
        let mut table = vec![0u16; size];
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as u32;
            let c = next[l as usize];
            next[l as usize] += 1;
            let mut i = reverse(c, l) as usize;
            while i < size {
                table[i] = (sym as u16) << 4 | l as u16;
                i += 1 << l;
            }
        }
        Ok(Self { table, bits: max })
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, Error> {
        if self.bits == 0 {
            return Err(Error::Corrupt);
        }
        if b.n < self.bits {
            b.refill();
        }
        let e = self.table[(b.buf & ((1u64 << self.bits) - 1)) as usize];
        let len = (e & 15) as u32;
        if len == 0 {
            return Err(Error::Corrupt);
        }
        b.buf >>= len;
        b.n -= len;
        Ok(e >> 4)
    }
}

fn fixed_lengths() -> ([u8; 288], [u8; 30]) {
    let mut lit = [0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    (lit, [5u8; 30])
}

/// Unpacks a raw DEFLATE stream, refusing to produce more than `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    inflate_counted(data, limit).map(|(out, _)| out)
}

/// Like [`inflate`], also returning how many input bytes the stream used.
pub fn inflate_counted(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut b = Bits::new(data);
    let mut out: Vec<u8> = Vec::with_capacity((data.len() * 3).min(limit).min(64 << 20));
    loop {
        let last = b.bits(1);
        match b.bits(2) {
            0 => {
                b.align();
                let len = b.bits(16);
                let nlen = b.bits(16);
                if len != !nlen & 0xffff {
                    return Err(if b.overrun() { Error::Truncated } else { Error::Corrupt });
                }
                if out.len() + len as usize > limit {
                    return Err(Error::TooBig);
                }
                // Whole bytes still in the bit buffer first, then straight from the input.
                let mut len = len as usize;
                while len > 0 && b.n >= 8 {
                    out.push(b.bits(8) as u8);
                    len -= 1;
                }
                if len > 0 {
                    let end = b.pos + len;
                    if end > data.len() {
                        return Err(Error::Truncated);
                    }
                    out.extend_from_slice(&data[b.pos..end]);
                    b.pos = end;
                }
            }
            1 => {
                let (lit, dist) = fixed_lengths();
                codes(&mut b, &mut out, &Huff::new(&lit)?, &Huff::new(&dist)?, limit)?;
            }
            2 => {
                let hlit = b.bits(5) as usize + 257;
                let hdist = b.bits(5) as usize + 1;
                let hclen = b.bits(4) as usize + 4;
                let mut cl = [0u8; 19];
                for &i in &CL_ORDER[..hclen] {
                    cl[i] = b.bits(3) as u8;
                }
                let clh = Huff::new(&cl)?;
                let mut lens = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lens.len() {
                    let sym = clh.decode(&mut b)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lens.get(i.wrapping_sub(1)).ok_or(Error::Corrupt)?, 3 + b.bits(2) as usize),
                        17 => (0, 3 + b.bits(3) as usize),
                        _ => (0, 11 + b.bits(7) as usize),
                    };
                    if i + rep > lens.len() {
                        return Err(Error::Corrupt);
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                    if b.overrun() {
                        return Err(Error::Truncated);
                    }
                }
                if lens[256] == 0 {
                    return Err(Error::Corrupt);
                }
                codes(&mut b, &mut out, &Huff::new(&lens[..hlit])?, &Huff::new(&lens[hlit..])?, limit)?;
            }
            _ => return Err(Error::Corrupt),
        }
        if b.overrun() {
            return Err(Error::Truncated);
        }
        if last == 1 {
            return Ok((out, b.consumed()));
        }
    }
}

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huff, dist: &Huff, limit: usize) -> Result<(), Error> {
    loop {
        let sym = lit.decode(b)? as usize;
        if b.overrun() {
            return Err(Error::Truncated);
        }
        match sym {
            0..=255 => {
                if out.len() >= limit {
                    return Err(Error::TooBig);
                }
                out.push(sym as u8);
            }
            256 => return Ok(()),
            _ => {
                let i = sym - 257;
                if i >= 29 {
                    return Err(Error::Corrupt);
                }
                let len = LEN_BASE[i] as usize + b.bits(LEN_EXTRA[i] as u32) as usize;
                let ds = dist.decode(b)? as usize;
                if ds >= 30 {
                    return Err(Error::Corrupt);
                }
                let d = DIST_BASE[ds] as usize + b.bits(DIST_EXTRA[ds] as u32) as usize;
                if d > out.len() {
                    return Err(Error::Corrupt);
                }
                if out.len() + len > limit {
                    return Err(Error::TooBig);
                }
                let start = out.len() - d;
                if d >= len {
                    out.extend_from_within(start..start + len);
                } else {
                    for k in 0..len {
                        out.push(out[start + k]);
                    }
                }
            }
        }
    }
}

/// Unpacks gzip data (every member, if there are several), checking each CRC.
pub fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut rest = data;
    loop {
        if rest.len() < 18 {
            return Err(Error::Truncated);
        }
        if rest[0] != 0x1f || rest[1] != 0x8b || rest[2] != 8 {
            return Err(Error::Corrupt);
        }
        let flags = rest[3];
        let mut i = 10;
        if flags & 4 != 0 {
            let xlen = u16::from_le_bytes([rest[i], rest[i + 1]]) as usize;
            i += 2 + xlen;
        }
        for bit in [8u8, 16] {
            if flags & bit != 0 {
                i += rest.get(i..).and_then(|r| r.iter().position(|&c| c == 0)).ok_or(Error::Truncated)? + 1;
            }
        }
        if flags & 2 != 0 {
            i += 2;
        }
        let body = rest.get(i..).ok_or(Error::Truncated)?;
        let (part, used) = inflate_counted(body, limit.saturating_sub(out.len()))?;
        let trailer = body.get(used..used + 8).ok_or(Error::Truncated)?;
        let crc = u32::from_le_bytes(trailer[..4].try_into().expect("4"));
        let size = u32::from_le_bytes(trailer[4..].try_into().expect("4"));
        if crc != crc32_update(0, &part) || size != part.len() as u32 {
            return Err(Error::Corrupt);
        }
        out.extend_from_slice(&part);
        rest = &body[used + 8..];
        // Trailing zero padding after the last member is allowed.
        if rest.iter().all(|&c| c == 0) {
            return Ok(out);
        }
    }
}

// ------------------------------------------------------------------ deflate

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, k: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += k;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

/// Code lengths for `freq`, no longer than `limit`, by package-merge.
fn huffman_lengths(freq: &[u32], limit: u8) -> Vec<u8> {
    let mut lens = vec![0u8; freq.len()];
    let mut syms: Vec<(u64, usize)> = freq.iter().enumerate().filter(|(_, &f)| f > 0).map(|(s, &f)| (f as u64, s)).collect();
    match syms.len() {
        0 => return lens,
        1 => {
            // A lone code is "incomplete"; zlib rejects that for code lengths,
            // so give it a partner (unused, but it keeps every decoder happy).
            let s = syms[0].1;
            lens[s] = 1;
            lens[if s == 0 { 1 } else { 0 }] = 1;
            return lens;
        }
        _ => {}
    }
    syms.sort_by_key(|&(w, s)| (w, s));
    let n = syms.len();
    let leaves: Vec<(u64, Vec<u16>)> = syms.iter().enumerate().map(|(i, &(w, _))| (w, vec![i as u16])).collect();
    let mut cur = leaves.clone();
    for _ in 1..limit {
        let packages: Vec<(u64, Vec<u16>)> = cur
            .chunks_exact(2)
            .map(|p| {
                let mut v = p[0].1.clone();
                v.extend_from_slice(&p[1].1);
                (p[0].0 + p[1].0, v)
            })
            .collect();
        let mut merged = Vec::with_capacity(leaves.len() + packages.len());
        let (mut a, mut b) = (leaves.iter().peekable(), packages.into_iter().peekable());
        loop {
            match (a.peek(), b.peek()) {
                (Some(x), Some(y)) if x.0 <= y.0 => merged.push(a.next().expect("peeked").clone()),
                (_, Some(_)) => merged.push(b.next().expect("peeked")),
                (Some(_), None) => merged.push(a.next().expect("peeked").clone()),
                (None, None) => break,
            }
        }
        cur = merged;
    }
    let mut count = vec![0u8; n];
    for item in &cur[..2 * n - 2] {
        for &i in &item.1 {
            count[i as usize] += 1;
        }
    }
    for (i, &(_, s)) in syms.iter().enumerate() {
        lens[s] = count[i];
    }
    lens
}

/// Canonical codes for `lens`, bit-reversed ready for writing.
fn codes_for(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            reverse(c, l as u32)
        })
        .collect()
}

fn len_sym(len: usize) -> usize {
    LEN_BASE.partition_point(|&b| b as usize <= len) - 1
}

fn dist_sym(d: usize) -> usize {
    DIST_BASE.partition_point(|&b| b as usize <= d) - 1
}

/// A literal (`dist == 0`) or a back-reference.
#[derive(Clone, Copy)]
struct Token {
    len: u16,
    dist: u16,
}

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 192;
const NONE: u32 = u32::MAX;

struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<u32>,
    prev: Vec<u32>,
}

impl<'a> Matcher<'a> {
    fn hash(&self, i: usize) -> usize {
        let d = self.data;
        let v = d[i] as u32 | (d[i + 1] as u32) << 8 | (d[i + 2] as u32) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }
    fn insert(&mut self, i: usize) {
        if i + 3 <= self.data.len() {
            let h = self.hash(i);
            self.prev[i % WINDOW] = self.head[h];
            self.head[h] = i as u32;
        }
    }
    /// The longest earlier match for the bytes at `i` (not yet inserted).
    fn find(&self, i: usize) -> (usize, usize) {
        let d = self.data;
        if i + 3 > d.len() {
            return (0, 0);
        }
        let max = (d.len() - i).min(258);
        let (mut best, mut best_d) = (0, 0);
        let mut cand = self.head[self.hash(i)];
        let mut chain = 0;
        while cand != NONE && chain < MAX_CHAIN {
            let c = cand as usize;
            if i - c > WINDOW {
                break;
            }
            // Quick reject: the byte that would make this match longer than the best.
            if d[c + best.min(max - 1)] == d[i + best.min(max - 1)] {
                let mut l = 0;
                while l < max && d[c + l] == d[i + l] {
                    l += 1;
                }
                if l > best {
                    best = l;
                    best_d = i - c;
                    if l == max {
                        break;
                    }
                }
            }
            let p = self.prev[c % WINDOW];
            // Stale entries point forward (or at themselves) once the window wrapped.
            if p == NONE || p as usize >= c {
                break;
            }
            cand = p;
            chain += 1;
        }
        if best >= 3 {
            (best, best_d)
        } else {
            (0, 0)
        }
    }
}

/// Packs `data` as a raw DEFLATE stream.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 2 + 64), acc: 0, n: 0 };
    let mut m = Matcher { data, head: vec![NONE; 1 << HASH_BITS], prev: vec![NONE; WINDOW] };
    let mut tokens: Vec<Token> = Vec::with_capacity(1 << 15);
    let mut block_start = 0;
    let mut i = 0;
    let mut pending: Option<(usize, usize)> = None;
    let lit = |b: u8| Token { len: b as u16, dist: 0 };
    while i < data.len() {
        let (len, dist) = m.find(i);
        if let Some((pl, pd)) = pending {
            if len > pl {
                // A longer match one byte later: emit the byte, keep the new match pending.
                tokens.push(lit(data[i - 1]));
                pending = Some((len, dist));
                m.insert(i);
                i += 1;
            } else {
                tokens.push(Token { len: pl as u16, dist: pd as u16 });
                let end = i - 1 + pl;
                while i < end {
                    m.insert(i);
                    i += 1;
                }
                pending = None;
            }
        } else if len >= 3 {
            pending = Some((len, dist));
            m.insert(i);
            i += 1;
        } else {
            tokens.push(lit(data[i]));
            m.insert(i);
            i += 1;
        }
        if pending.is_none() && tokens.len() >= 1 << 15 {
            write_block(&mut w, &tokens, &data[block_start..i], false);
            tokens.clear();
            block_start = i;
        }
    }
    if let Some((pl, pd)) = pending {
        tokens.push(Token { len: pl as u16, dist: pd as u16 });
    }
    write_block(&mut w, &tokens, &data[block_start..], true);
    w.align();
    w.out
}

fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let mut lf = [0u32; 286];
    let mut df = [0u32; 30];
    for t in tokens {
        if t.dist == 0 {
            lf[t.len as usize] += 1;
        } else {
            lf[257 + len_sym(t.len as usize)] += 1;
            df[dist_sym(t.dist as usize)] += 1;
        }
    }
    lf[256] = 1;
    let ll = huffman_lengths(&lf, 15);
    let mut dl = huffman_lengths(&df, 15);
    if dl.iter().all(|&l| l == 0) {
        dl[0] = 1;
        dl[1] = 1;
    }
    let hlit = (257..286).rev().find(|&s| ll[s] > 0).map(|s| s + 1).unwrap_or(257).max(257);
    let hdist = (0..30).rev().find(|&s| dl[s] > 0).map(|s| s + 1).unwrap_or(1);
    // Run-length encode the code lengths with symbols 16/17/18.
    let all: Vec<u8> = ll[..hlit].iter().chain(&dl[..hdist]).copied().collect();
    let mut cl: Vec<(u8, u32)> = Vec::new();
    let mut k = 0;
    while k < all.len() {
        let v = all[k];
        let mut run = 1;
        while k + run < all.len() && all[k + run] == v {
            run += 1;
        }
        k += run;
        if v == 0 {
            while run >= 11 {
                let t = run.min(138);
                cl.push((18, (t - 11) as u32));
                run -= t;
            }
            if run >= 3 {
                cl.push((17, (run - 3) as u32));
                run = 0;
            }
        } else {
            cl.push((v, 0));
            run -= 1;
            while run >= 3 {
                let t = run.min(6);
                cl.push((16, (t - 3) as u32));
                run -= t;
            }
        }
        for _ in 0..run {
            cl.push((v, 0));
        }
    }
    let mut cf = [0u32; 19];
    for &(s, _) in &cl {
        cf[s as usize] += 1;
    }
    let cll = huffman_lengths(&cf, 7);
    let hclen = (4..=19).rev().find(|&j| cll[CL_ORDER[j - 1]] > 0).unwrap_or(4);
    let extra = |s: u8| match s {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    };
    // Size of each choice, in bits.
    let mut dyn_bits = 3 + 14 + 3 * hclen as u64;
    dyn_bits += cl.iter().map(|&(s, _)| cll[s as usize] as u64 + extra(s)).sum::<u64>();
    for t in tokens {
        dyn_bits += if t.dist == 0 {
            ll[t.len as usize] as u64
        } else {
            let ls = len_sym(t.len as usize);
            let ds = dist_sym(t.dist as usize);
            ll[257 + ls] as u64 + LEN_EXTRA[ls] as u64 + dl[ds] as u64 + DIST_EXTRA[ds] as u64
        };
    }
    dyn_bits += ll[256] as u64;
    let stored_bits = (raw.len().div_ceil(65535).max(1) as u64) * (3 + 7 + 32) + raw.len() as u64 * 8;
    if stored_bits <= dyn_bits {
        let chunks: Vec<&[u8]> = if raw.is_empty() { vec![&[][..]] } else { raw.chunks(65535).collect() };
        let n = chunks.len();
        for (j, c) in chunks.into_iter().enumerate() {
            w.put((last && j + 1 == n) as u32, 1);
            w.put(0, 2);
            w.align();
            w.put(c.len() as u32, 16);
            w.put(!(c.len() as u32) & 0xffff, 16);
            w.out.extend_from_slice(c);
        }
        return;
    }
    let lc = codes_for(&ll);
    let dc = codes_for(&dl);
    let cc = codes_for(&cll);
    w.put(last as u32, 1);
    w.put(2, 2);
    w.put((hlit - 257) as u32, 5);
    w.put((hdist - 1) as u32, 5);
    w.put((hclen - 4) as u32, 4);
    for &j in &CL_ORDER[..hclen] {
        w.put(cll[j] as u32, 3);
    }
    for &(s, x) in &cl {
        w.put(cc[s as usize], cll[s as usize] as u32);
        w.put(x, extra(s) as u32);
    }
    for t in tokens {
        if t.dist == 0 {
            w.put(lc[t.len as usize], ll[t.len as usize] as u32);
        } else {
            let ls = len_sym(t.len as usize);
            w.put(lc[257 + ls], ll[257 + ls] as u32);
            w.put(t.len as u32 - LEN_BASE[ls] as u32, LEN_EXTRA[ls] as u32);
            let ds = dist_sym(t.dist as usize);
            w.put(dc[ds], dl[ds] as u32);
            w.put(t.dist as u32 - DIST_BASE[ds] as u32, DIST_EXTRA[ds] as u32);
        }
    }
    w.put(lc[256], ll[256] as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        // Text, repeats at many distances, and a stretch of noise.
        let mut v = Vec::new();
        for i in 0..4000u32 {
            v.extend_from_slice(format!("line {i}: the quick brown fox jumps over {} lazy dogs\n", i % 37).as_bytes());
        }
        let mut x = 12345u32;
        for _ in 0..50_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push(x as u8);
        }
        v.extend(std::iter::repeat_n(b'a', 70_000));
        v
    }

    #[test]
    fn round_trips() {
        for data in [Vec::new(), b"a".to_vec(), b"abcabcabcabcabcabc".to_vec(), sample()] {
            let packed = deflate(&data);
            assert_eq!(inflate(&packed, usize::MAX).unwrap(), data);
        }
        let s = sample();
        let packed = deflate(&s);
        assert!(packed.len() < s.len() / 2, "compresses: {} -> {}", s.len(), packed.len());
        // Noise alone falls back to stored blocks and barely grows.
        let noise: Vec<u8> = s[s.len() - 120_000..s.len() - 70_000].to_vec();
        assert!(deflate(&noise).len() <= noise.len() + 64);
    }

    #[test]
    fn reads_zlib_output() {
        // `zlib.compressobj(9, zlib.DEFLATED, -15)` of 60 numbered sentences: one dynamic-Huffman block.
        let z = [0xed, 0xd2, 0x2b, 0x12, 0x80, 0x30, 0x0c, 0x84, 0xe1, 0xab, 0xe4, 0x04, 0x0c, 0xe1, 0x8d, 0xc5, 0xe2, 0x41, 0x17, 0xe8, 0x0c, 0x0c, 0x99, 0x46, 0xb4, 0x08, 0x6e, 0x8f, 0x80, 0x03, 0x2c, 0x3e, 0xfe, 0x53, 0xbb, 0xff, 0xa4, 0xe2, 0x42, 0xa2, 0x9c, 0xa2, 0xbb, 0x23, 0xed, 0x5e, 0x44, 0x29, 0x29, 0xcd, 0x7e, 0x19, 0x8f, 0x44, 0x2e, 0x6c, 0x74, 0x0d, 0xa2, 0xeb, 0x99, 0xd1, 0xf4, 0x4a, 0x86, 0x65, 0x01, 0xcb, 0x12, 0x96, 0x15, 0x2c, 0x6b, 0x58, 0x36, 0xb0, 0x6c, 0x61, 0xd9, 0xc1, 0xb2, 0xc7, 0x97, 0xff, 0x71, 0x12, 0xfe, 0x12, 0xe3, 0x37, 0x31, 0xfe, 0x13, 0xe3, 0x47, 0x31, 0xfe, 0x14, 0xe3, 0x57, 0x59, 0xd0, 0x16, 0xb4, 0x05, 0x6d, 0x41, 0x5b, 0xd0, 0x16, 0xb4, 0x05, 0xfd, 0xc9, 0x07];
        let want: Vec<u8> = (0..60).flat_map(|i| format!("Volant {} says hello to WebKit and uBlock. ", i % 17).into_bytes()).collect();
        assert_eq!(inflate(&z, usize::MAX).unwrap(), want);
        // Fixed-Huffman block (`zlib.compress(b"abc", 1)` without the zlib wrapper).
        assert_eq!(inflate(&[0x4b, 0x4c, 0x4a, 0x06, 0x00], 10).unwrap(), b"abc");
    }

    #[test]
    fn refuses_bad_input() {
        let packed = deflate(&sample());
        assert_eq!(inflate(&packed[..packed.len() / 2], usize::MAX), Err(Error::Truncated));
        assert_eq!(inflate(&packed, 1000), Err(Error::TooBig));
        assert!(inflate(&[0xff, 0xff, 0xff], 100).is_err());
        assert!(inflate(&[], 100).is_err());
    }

    #[test]
    fn gzip() {
        // Python's gzip of "volant\n", with the file name field set (flag 8).
        let gz = [0x1f, 0x8b, 0x08, 0x08, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x76, 0x00, 0x2b, 0xcb, 0xcf, 0x49, 0xcc, 0x2b, 0xe1, 0x02, 0x00, 0x0c, 0x3e, 0xe1, 0xdd, 0x07, 0x00, 0x00, 0x00];
        assert_eq!(gunzip(&gz, 100).unwrap(), b"volant\n");
        let mut bad = gz;
        bad[21] ^= 1;
        assert_eq!(gunzip(&bad, 100), Err(Error::Corrupt));
        assert_eq!(gunzip(&gz[..20], 100), Err(Error::Truncated));
    }

    #[test]
    fn long_codes_stay_valid() {
        // Literals with halving frequencies need codes up to (and past) 15
        // bits; a block like this once dropped its last code-length entry.
        let mut raw = Vec::new();
        for k in 0..20u32 {
            for _ in 0..(1u32 << (19 - k)).max(1) {
                raw.push(k as u8 * 13);
            }
        }
        // Shuffle (deterministically) so it's one literal per token.
        let mut x = 99u32;
        for i in (1..raw.len()).rev() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            raw.swap(i, x as usize % (i + 1));
        }
        let tokens: Vec<Token> = raw.iter().map(|&b| Token { len: b as u16, dist: 0 }).collect();
        let mut w = BitWriter { out: Vec::new(), acc: 0, n: 0 };
        write_block(&mut w, &tokens, &raw, true);
        w.align();
        assert_eq!(inflate(&w.out, usize::MAX).unwrap(), raw);
        assert!(strictly_complete(&w.out), "every code must be complete, as zlib requires");
        // And the whole compressor on mixed data.
        let mut v = raw.clone();
        v.extend(b"repeat me ".repeat(5000));
        let packed = deflate(&v);
        assert_eq!(inflate(&packed, usize::MAX).unwrap(), v);
        assert!(strictly_complete(&packed));
    }

    /// Walks the dynamic blocks' headers, checking each code-length and
    /// literal/length code is complete (zlib's rule; ours is more lenient).
    fn strictly_complete(data: &[u8]) -> bool {
        let mut b = Bits::new(data);
        let mut out = Vec::new();
        loop {
            let last = b.bits(1);
            let ty = b.bits(2);
            if ty != 2 {
                return true; // the test data only needs dynamic blocks checked until the first other kind
            }
            let hlit = b.bits(5) as usize + 257;
            let hdist = b.bits(5) as usize + 1;
            let hclen = b.bits(4) as usize + 4;
            let mut cl = [0u8; 19];
            for &i in &CL_ORDER[..hclen] {
                cl[i] = b.bits(3) as u8;
            }
            let kraft = |l: &[u8]| l.iter().filter(|&&x| x > 0).map(|&x| 1.0f64 / (1u64 << x) as f64).sum::<f64>();
            if (kraft(&cl) - 1.0).abs() > 1e-12 {
                return false;
            }
            let clh = Huff::new(&cl).unwrap();
            let mut lens = vec![0u8; hlit + hdist];
            let mut i = 0;
            while i < lens.len() {
                let sym = clh.decode(&mut b).unwrap();
                let (val, rep) = match sym {
                    0..=15 => (sym as u8, 1),
                    16 => (lens[i - 1], 3 + b.bits(2) as usize),
                    17 => (0, 3 + b.bits(3) as usize),
                    _ => (0, 11 + b.bits(7) as usize),
                };
                lens[i..i + rep].fill(val);
                i += rep;
            }
            if (kraft(&lens[..hlit]) - 1.0).abs() > 1e-12 {
                return false;
            }
            codes(&mut b, &mut out, &Huff::new(&lens[..hlit]).unwrap(), &Huff::new(&lens[hlit..]).unwrap(), usize::MAX).unwrap();
            if last == 1 {
                return true;
            }
        }
    }

    #[test]
    fn length_limited_codes() {
        // Fibonacci frequencies force deep trees; the limit must hold and the code stay complete.
        let mut f = vec![1u32, 1];
        while f.len() < 30 {
            let n = f[f.len() - 1] + f[f.len() - 2];
            f.push(n);
        }
        let l = huffman_lengths(&f, 15);
        assert!(l.iter().all(|&x| (1..=15).contains(&x)));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(x as i32)).sum();
        assert!((kraft - 1.0).abs() < 1e-9);
    }
}
