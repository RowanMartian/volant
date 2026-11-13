//! WOFF 1.0 web fonts back to the TrueType/OpenType (sfnt) files inside
//! them: each table is zlib-compressed (or stored), and the sfnt header and
//! table directory are rebuilt around them. (WOFF 2 is handed to macOS's
//! CoreText as it is.)

use crate::deflate;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Is this a WOFF 1 file?
pub fn is_woff(data: &[u8]) -> bool {
    data.starts_with(b"wOFF")
}

/// The sfnt font inside a WOFF 1 file, or `None` if it's malformed or bigger than `limit`.
pub fn to_sfnt(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if !is_woff(data) {
        return None;
    }
    let flavor = u32_at(data, 4)?;
    let num = u16_at(data, 12)? as usize;
    let total = u32_at(data, 16)? as usize;
    if num == 0 || num > 512 || total > limit {
        return None;
    }
    // The sfnt header: flavor, table count, and the binary-search fields.
    let mut pow = 1usize;
    let mut log = 0u16;
    while pow * 2 <= num {
        pow *= 2;
        log += 1;
    }
    let search = (pow * 16) as u16;
    let mut out = Vec::with_capacity(total);
    out.extend(flavor.to_be_bytes());
    out.extend((num as u16).to_be_bytes());
    out.extend(search.to_be_bytes());
    out.extend(log.to_be_bytes());
    out.extend(((num * 16) as u16 - search).to_be_bytes());
    let mut offset = 12 + num * 16;
    let mut tables = Vec::with_capacity(num);
    for i in 0..num {
        let e = 44 + i * 20;
        let (tag, at, comp, orig, sum) = (u32_at(data, e)?, u32_at(data, e + 4)? as usize, u32_at(data, e + 8)? as usize, u32_at(data, e + 12)? as usize, u32_at(data, e + 16)?);
        let raw = data.get(at..at.checked_add(comp)?)?;
        let table = if comp < orig { deflate::inflate(raw.get(2..)?, orig).ok()? } else { raw.to_vec() };
        if table.len() != orig {
            return None;
        }
        out.extend(tag.to_be_bytes());
        out.extend(sum.to_be_bytes());
        out.extend((offset as u32).to_be_bytes());
        out.extend((orig as u32).to_be_bytes());
        offset += (orig + 3) & !3;
        if offset > limit {
            return None;
        }
        tables.push(table);
    }
    for t in tables {
        let pad = (4 - t.len() % 4) % 4;
        out.extend(t);
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WOFF with two tables, one compressed and one stored.
    fn woff(tables: &[(&[u8; 4], &[u8], bool)]) -> Vec<u8> {
        let mut dir = Vec::new();
        let mut blobs = Vec::new();
        let mut at = 44 + tables.len() * 20;
        for (tag, data, compress) in tables {
            let stored = if *compress {
                // zlib: a header, the raw deflate stream, an Adler-32 (unchecked here).
                let mut z = vec![0x78, 0x9c];
                z.extend(deflate::deflate(data));
                z.extend([0, 0, 0, 0]);
                z
            } else {
                data.to_vec()
            };
            dir.extend(*tag);
            dir.extend((at as u32).to_be_bytes());
            dir.extend((stored.len() as u32).to_be_bytes());
            dir.extend((data.len() as u32).to_be_bytes());
            dir.extend(0x1234u32.to_be_bytes());
            at += (stored.len() + 3) & !3;
            let pad = (4 - stored.len() % 4) % 4;
            blobs.extend(stored);
            blobs.extend(std::iter::repeat_n(0u8, pad));
        }
        let mut f = b"wOFF".to_vec();
        f.extend(0x00010000u32.to_be_bytes());
        f.extend(((44 + dir.len() + blobs.len()) as u32).to_be_bytes());
        f.extend((tables.len() as u16).to_be_bytes());
        f.extend([0, 0]);
        f.extend(4096u32.to_be_bytes());
        f.extend([0u8; 24]);
        f.extend(dir);
        f.extend(blobs);
        f
    }

    #[test]
    fn unpacks_tables() {
        let glyf = b"glyph outlines ".repeat(20);
        let f = woff(&[(b"glyf", &glyf, true), (b"head", b"head54", false)]);
        let s = to_sfnt(&f, 1 << 20).unwrap();
        assert_eq!(u32_at(&s, 0), Some(0x00010000));
        assert_eq!(u16_at(&s, 4), Some(2));
        assert_eq!((u16_at(&s, 6), u16_at(&s, 8), u16_at(&s, 10)), (Some(32), Some(1), Some(0)));
        // Directory entries point at the tables, padded to 4 bytes.
        assert_eq!(&s[12..16], b"glyf");
        let (off, len) = (u32_at(&s, 20).unwrap() as usize, u32_at(&s, 24).unwrap() as usize);
        assert_eq!(&s[off..off + len], &glyf[..]);
        assert_eq!(&s[28..32], b"head");
        let off2 = u32_at(&s, 36).unwrap() as usize;
        assert_eq!(off2 % 4, 0);
        assert_eq!(&s[off2..off2 + 6], b"head54");
        assert!(to_sfnt(b"wOF2....", 1 << 20).is_none());
        assert!(to_sfnt(&f[..60], 1 << 20).is_none());
    }
}
