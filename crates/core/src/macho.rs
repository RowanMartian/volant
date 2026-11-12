//! Universal ("fat") Mach-O binaries: what `lipo -create` makes. Each thin
//! binary (Apple Silicon, Intel) goes in its own slice, page-aligned, behind a
//! big-endian table of contents.

const FAT_MAGIC: u32 = 0xcafe_babe;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC: u32 = 0xfeed_face;
pub const CPU_ARM64: u32 = 0x0100_000c;
pub const CPU_X86_64: u32 = 0x0100_0007;

/// A thin Mach-O's CPU type and subtype (as its header records them).
pub fn cpu(thin: &[u8]) -> Result<(u32, u32), String> {
    let word = |i: usize| thin.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    match word(0) {
        Some(MH_MAGIC_64) | Some(MH_MAGIC) => Ok((word(4).ok_or("short header")?, word(8).ok_or("short header")?)),
        Some(m) if m.swap_bytes() == FAT_MAGIC => Err("already a universal binary".into()),
        _ => Err("not a Mach-O binary".into()),
    }
}

pub fn arch_name(cputype: u32) -> &'static str {
    match cputype {
        CPU_ARM64 => "arm64",
        CPU_X86_64 => "x86_64",
        _ => "unknown",
    }
}

/// Slice alignment, as a power of two: 16 KB pages on Apple Silicon, 4 KB on Intel.
fn align_for(cputype: u32) -> u32 {
    if cputype == CPU_ARM64 {
        14
    } else {
        12
    }
}

/// Joins thin binaries into one universal binary.
pub fn fat(thins: &[&[u8]]) -> Result<Vec<u8>, String> {
    if thins.is_empty() {
        return Err("nothing to join".into());
    }
    let mut archs = Vec::new();
    for t in thins {
        let (ty, sub) = cpu(t)?;
        if archs.iter().any(|&(other, _, _)| other == ty) {
            return Err(format!("two {} binaries", arch_name(ty)));
        }
        archs.push((ty, sub, *t));
    }
    // Intel first, Apple Silicon last, as lipo orders them.
    archs.sort_by_key(|&(ty, _, _)| align_for(ty));
    let mut out = Vec::new();
    out.extend(FAT_MAGIC.to_be_bytes());
    out.extend((archs.len() as u32).to_be_bytes());
    let mut offset = 8 + 20 * archs.len();
    let mut slices = Vec::new();
    for &(ty, sub, data) in &archs {
        let align = align_for(ty);
        offset = offset.next_multiple_of(1 << align);
        out.extend(ty.to_be_bytes());
        out.extend(sub.to_be_bytes());
        out.extend((offset as u32).to_be_bytes());
        out.extend((data.len() as u32).to_be_bytes());
        out.extend(align.to_be_bytes());
        slices.push((offset, data));
        offset += data.len();
    }
    for (at, data) in slices {
        out.resize(at, 0);
        out.extend_from_slice(data);
    }
    Ok(out)
}

/// The architectures in a universal binary: (cputype, offset, size).
pub fn slices(fat: &[u8]) -> Result<Vec<(u32, usize, usize)>, String> {
    let be = |i: usize| fat.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or("truncated");
    if be(0)? != FAT_MAGIC {
        return Err("not a universal binary".into());
    }
    let n = be(4)? as usize;
    (0..n)
        .map(|i| {
            let base = 8 + 20 * i;
            let (ty, off, size) = (be(base)?, be(base + 8)? as usize, be(base + 12)? as usize);
            if off + size > fat.len() {
                return Err("a slice runs past the end".to_string());
            }
            Ok((ty, off, size))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thin(cputype: u32, sub: u32, len: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(MH_MAGIC_64.to_le_bytes());
        v.extend(cputype.to_le_bytes());
        v.extend(sub.to_le_bytes());
        v.resize(len, (cputype & 0xff) as u8);
        v
    }

    #[test]
    fn joins_and_reads_back() {
        let arm = thin(CPU_ARM64, 0, 50_000);
        let x86 = thin(CPU_X86_64, 0x8000_0003, 30_000);
        let f = fat(&[&arm, &x86]).unwrap();
        let s = slices(&f).unwrap();
        assert_eq!(s.iter().map(|x| arch_name(x.0)).collect::<Vec<_>>(), ["x86_64", "arm64"]);
        assert_eq!(s[0].1 % 4096, 0);
        assert_eq!(s[1].1 % 16384, 0);
        assert_eq!(&f[s[0].1..s[0].1 + s[0].2], &x86[..]);
        assert_eq!(&f[s[1].1..s[1].1 + s[1].2], &arm[..]);
        // The subtype (with its capability bits) is kept as the header has it.
        assert_eq!(u32::from_be_bytes(f[12..16].try_into().unwrap()), 0x8000_0003);
        assert!(fat(&[&arm, &arm]).is_err());
        assert!(fat(&[&f]).is_err());
        assert!(fat(&[b"#!/bin/sh\n"]).is_err());
    }
}
