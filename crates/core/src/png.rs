//! PNG: decoding every standard colour type and bit depth (interlaced
//! files too) to 8-bit RGBA, and encoding RGBA.

use crate::hash::crc32;

const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// A decoded picture: (width, height, RGBA pixels row by row).
pub fn decode(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if !data.starts_with(SIG) {
        return Err("not a PNG".into());
    }
    let mut i = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat = Vec::new();
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let kind = &data[i + 4..i + 8];
        let body = data.get(i + 8..i + 8 + len).ok_or("a cut-off chunk")?;
        match kind {
            b"IHDR" if len >= 13 => {
                w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect(),
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    if w == 0 || h == 0 || (w as u64 * h as u64) > 64 << 20 {
        return Err("a bad size".into());
    }
    let channels = match ctype {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err("an unknown colour type".into()),
    };
    if !matches!(depth, 1 | 2 | 4 | 8 | 16) {
        return Err("an unknown bit depth".into());
    }
    for (k, a) in trns.iter().enumerate() {
        if ctype == 3 && k < palette.len() {
            palette[k][3] = *a;
        }
    }
    let bits = channels * depth as usize;
    let row_bytes = |pw: u32| (pw as usize * bits).div_ceil(8);
    // The zlib wrapper: 2 bytes of header, then raw DEFLATE.
    let raw = crate::deflate::inflate(idat.get(2..).ok_or("no image data")?, 256 << 20).map_err(|_| "bad image data")?;
    let mut out = vec![0u8; (w * h * 4) as usize];
    // (x start, y start, x step, y step) of each pass; one pass when not interlaced.
    let passes: &[(u32, u32, u32, u32)] = if interlace == 1 {
        &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        &[(0, 0, 1, 1)]
    };
    let bpp = bits.div_ceil(8).max(1);
    let mut pos = 0;
    for &(x0, y0, dx, dy) in passes {
        let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
        let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
        if pw == 0 || ph == 0 {
            continue;
        }
        let rb = row_bytes(pw);
        let mut prev = vec![0u8; rb];
        for r in 0..ph {
            let filter = *raw.get(pos).ok_or("short image data")?;
            let mut line = raw.get(pos + 1..pos + 1 + rb).ok_or("short image data")?.to_vec();
            pos += 1 + rb;
            unfilter(filter, &mut line, &prev, bpp)?;
            for c in 0..pw {
                let px = pixel(&line, c as usize, ctype, depth, &palette, &trns);
                let (x, y) = (x0 + c * dx, y0 + r * dy);
                let k = ((y * w + x) * 4) as usize;
                out[k..k + 4].copy_from_slice(&px);
            }
            prev = line;
        }
    }
    Ok((w, h, out))
}

fn unfilter(f: u8, line: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), String> {
    for k in 0..line.len() {
        let a = if k >= bpp { line[k - bpp] } else { 0 };
        let b = prev[k];
        let c = if k >= bpp { prev[k - bpp] } else { 0 };
        line[k] = line[k].wrapping_add(match f {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((a as u16 + b as u16) / 2) as u8,
            4 => {
                let p = a as i16 + b as i16 - c as i16;
                let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                }
            }
            _ => return Err("an unknown filter".into()),
        });
    }
    Ok(())
}

/// Sample `n` of a row (of `depth` bits each), scaled to 8 bits.
fn sample(line: &[u8], n: usize, depth: u8) -> (u8, u16) {
    match depth {
        16 => {
            let v = u16::from_be_bytes([line[n * 2], line[n * 2 + 1]]);
            ((v >> 8) as u8, v)
        }
        8 => (line[n], line[n] as u16),
        d => {
            let per = 8 / d as usize;
            let byte = line[n / per];
            let shift = 8 - d as usize * (n % per + 1);
            let v = (byte >> shift) & ((1u8 << d) - 1);
            ((v as u16 * 255 / ((1u16 << d) - 1)) as u8, v as u16)
        }
    }
}

fn pixel(line: &[u8], x: usize, ctype: u8, depth: u8, palette: &[[u8; 4]], trns: &[u8]) -> [u8; 4] {
    let key = |k: usize| trns.get(k * 2..k * 2 + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    match ctype {
        0 => {
            let (g, raw) = sample(line, x, depth);
            [g, g, g, if key(0) == Some(raw) { 0 } else { 255 }]
        }
        2 => {
            let (r, rr) = sample(line, x * 3, depth);
            let (g, rg) = sample(line, x * 3 + 1, depth);
            let (b, rb) = sample(line, x * 3 + 2, depth);
            let clear = trns.len() >= 6 && key(0) == Some(rr) && key(1) == Some(rg) && key(2) == Some(rb);
            [r, g, b, if clear { 0 } else { 255 }]
        }
        3 => {
            let (_, i) = sample(line, x, depth);
            palette.get(i as usize).copied().unwrap_or([0, 0, 0, 255])
        }
        4 => {
            let (g, _) = sample(line, x * 2, depth);
            let (a, _) = sample(line, x * 2 + 1, depth);
            [g, g, g, a]
        }
        _ => {
            let s = |k| sample(line, x * 4 + k, depth).0;
            [s(0), s(1), s(2), s(3)]
        }
    }
}

/// Encodes RGBA pixels as a PNG.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    z.extend(crate::deflate::deflate(&raw));
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = SIG.to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend((body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend(crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let px: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 40, 255 - i as u8 * 40, 7, if i == 4 { 0 } else { 255 }]).collect();
        let png = encode(3, 2, &px);
        assert_eq!(decode(&png).unwrap(), (3, 2, px));
        assert!(decode(b"GIF89a").is_err());
    }
}
