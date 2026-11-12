//! Zip archives, both ways: the updater unpacks `Volant-mac.zip` with
//! [`extract_to`], and the build packs it with [`pack_dir`] (what `ditto -c -k
//! --keepParent` did). Stored and deflated entries, Unix permissions, UTF-8
//! names. Unpacking refuses anything that could write outside the target
//! (absolute paths, `..`, symlinks) and checks every entry's CRC.

use crate::deflate;
use crate::hash::crc32;
use std::path::Path;

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Unix permission bits (and file type), when the archive records them.
    pub mode: u32,
    pub is_dir: bool,
    method: u16,
    crc: u32,
    csize: u64,
    pub size: u64,
    local: u64,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Lists the entries of an archive from its central directory.
pub fn entries(zip: &[u8]) -> Result<Vec<Entry>, String> {
    let bad = || "not a zip archive".to_string();
    let from = zip.len().saturating_sub(22 + 65535);
    let end = (from..zip.len().saturating_sub(21)).rev().find(|&i| u32_at(zip, i) == Some(END)).ok_or_else(bad)?;
    let count = u16_at(zip, end + 10).ok_or_else(bad)? as usize;
    let cd = u32_at(zip, end + 16).ok_or_else(bad)? as usize;
    if count == 0xffff || cd == 0xffff_ffff {
        return Err("zip64 archives aren't supported".into());
    }
    let mut out = Vec::with_capacity(count);
    let mut i = cd;
    for _ in 0..count {
        if u32_at(zip, i) != Some(CENTRAL) {
            return Err("the zip's directory is damaged".into());
        }
        let made_by = u16_at(zip, i + 4).ok_or_else(bad)?;
        let flags = u16_at(zip, i + 8).ok_or_else(bad)?;
        let method = u16_at(zip, i + 10).ok_or_else(bad)?;
        let crc = u32_at(zip, i + 16).ok_or_else(bad)?;
        let csize = u32_at(zip, i + 20).ok_or_else(bad)? as u64;
        let size = u32_at(zip, i + 24).ok_or_else(bad)? as u64;
        let nlen = u16_at(zip, i + 28).ok_or_else(bad)? as usize;
        let xlen = u16_at(zip, i + 30).ok_or_else(bad)? as usize;
        let clen = u16_at(zip, i + 32).ok_or_else(bad)? as usize;
        let ext = u32_at(zip, i + 38).ok_or_else(bad)?;
        let local = u32_at(zip, i + 42).ok_or_else(bad)? as u64;
        if flags & 1 != 0 {
            return Err("encrypted zips aren't supported".into());
        }
        let raw = zip.get(i + 46..i + 46 + nlen).ok_or_else(bad)?;
        let name = String::from_utf8(raw.to_vec()).map_err(|_| "a file name in the zip isn't UTF-8".to_string())?;
        let unix = made_by >> 8 == 3;
        let is_dir = name.ends_with('/');
        let mode = if unix && ext >> 16 != 0 {
            ext >> 16
        } else if is_dir {
            0o040755
        } else {
            0o100644
        };
        out.push(Entry { name, mode, is_dir, method, crc, csize, size, local });
        i += 46 + nlen + xlen + clen;
    }
    Ok(out)
}

/// The uncompressed contents of one entry, CRC-checked.
pub fn read(zip: &[u8], e: &Entry, limit: usize) -> Result<Vec<u8>, String> {
    if e.size as usize > limit {
        return Err(format!("{} is too big", e.name));
    }
    let l = e.local as usize;
    if u32_at(zip, l) != Some(LOCAL) {
        return Err(format!("{} is damaged", e.name));
    }
    let nlen = u16_at(zip, l + 26).ok_or("damaged")? as usize;
    let xlen = u16_at(zip, l + 28).ok_or("damaged")? as usize;
    let start = l + 30 + nlen + xlen;
    let body = zip.get(start..start + e.csize as usize).ok_or_else(|| format!("{} is cut short", e.name))?;
    let data = match e.method {
        0 => body.to_vec(),
        8 => deflate::inflate(body, e.size as usize).map_err(|err| format!("{}: {err}", e.name))?,
        m => return Err(format!("{} uses compression method {m}, which isn't supported", e.name)),
    };
    if data.len() as u64 != e.size || crc32(&data) != e.crc {
        return Err(format!("{} failed its checksum", e.name));
    }
    Ok(data)
}

/// A name that stays inside the target folder: relative, no `..`, no backslashes.
fn safe(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name.split('/').all(|part| part != ".." && part != ".")
}

/// Unpacks every entry under `dir`. Mac resource-fork shadows (`__MACOSX/`,
/// `._name`) are skipped; symlinks and unsafe names are refused.
#[cfg(unix)]
pub fn extract_to(zip: &[u8], dir: &Path, limit: u64) -> Result<Vec<String>, String> {
    use std::os::unix::fs::PermissionsExt;
    let list = entries(zip)?;
    if list.iter().map(|e| e.size).sum::<u64>() > limit {
        return Err("the archive unpacks to more than allowed".into());
    }
    let mut written = Vec::new();
    for e in &list {
        let shadow = e.name.starts_with("__MACOSX/") || e.name.rsplit('/').find(|s| !s.is_empty()).map(|b| b.starts_with("._")).unwrap_or(false);
        if shadow {
            continue;
        }
        if !safe(&e.name) {
            return Err(format!("refusing an unsafe path in the archive: {}", e.name));
        }
        let kind = e.mode & 0o170000;
        if kind == 0o120000 {
            return Err(format!("refusing a symbolic link in the archive: {}", e.name));
        }
        let path = dir.join(e.name.trim_end_matches('/'));
        if e.is_dir || kind == 0o040000 {
            std::fs::create_dir_all(&path).map_err(|err| format!("{}: {err}", e.name))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", e.name))?;
        }
        let data = read(zip, e, limit as usize)?;
        std::fs::write(&path, &data).map_err(|err| format!("{}: {err}", e.name))?;
        let perm = e.mode & 0o777;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(if perm == 0 { 0o644 } else { perm })).map_err(|err| format!("{}: {err}", e.name))?;
        written.push(e.name.clone());
    }
    Ok(written)
}

/// Builds a zip in memory.
#[derive(Default)]
pub struct Writer {
    out: Vec<u8>,
    central: Vec<u8>,
    count: usize,
}

/// MS-DOS date and time for a Unix timestamp (UTC), as zip stores them.
fn dos_time(secs: u64) -> (u16, u16) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    let y = y.clamp(1980, 2107);
    let date = ((y - 1980) << 9 | m << 5 | d) as u16;
    let time = ((rem / 3600) << 11 | (rem % 3600 / 60) << 5 | (rem % 60 / 2)) as u16;
    (date, time)
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&mut self, name: &str, mode: u32, mtime: u64, data: &[u8]) {
        let (date, time) = dos_time(mtime);
        let crc = crc32(data);
        let (method, body) = if data.is_empty() {
            (0u16, Vec::new())
        } else {
            let packed = deflate::deflate(data);
            if packed.len() < data.len() {
                (8, packed)
            } else {
                (0, data.to_vec())
            }
        };
        let offset = self.out.len() as u32;
        let flags: u16 = 1 << 11; // UTF-8 names
        let version: u16 = if method == 8 { 20 } else { 10 };
        let mut local = Vec::with_capacity(30 + name.len());
        local.extend(LOCAL.to_le_bytes());
        local.extend(version.to_le_bytes());
        local.extend(flags.to_le_bytes());
        local.extend(method.to_le_bytes());
        local.extend(time.to_le_bytes());
        local.extend(date.to_le_bytes());
        local.extend(crc.to_le_bytes());
        local.extend((body.len() as u32).to_le_bytes());
        local.extend((data.len() as u32).to_le_bytes());
        local.extend((name.len() as u16).to_le_bytes());
        local.extend(0u16.to_le_bytes());
        local.extend(name.as_bytes());
        self.out.extend(local);
        self.out.extend(body.iter());
        let c = &mut self.central;
        c.extend(CENTRAL.to_le_bytes());
        c.extend((3u16 << 8 | 20).to_le_bytes()); // made by Unix, spec 2.0
        c.extend(version.to_le_bytes());
        c.extend(flags.to_le_bytes());
        c.extend(method.to_le_bytes());
        c.extend(time.to_le_bytes());
        c.extend(date.to_le_bytes());
        c.extend(crc.to_le_bytes());
        c.extend((body.len() as u32).to_le_bytes());
        c.extend((data.len() as u32).to_le_bytes());
        c.extend((name.len() as u16).to_le_bytes());
        c.extend([0u8; 8]); // extra, comment, disk, internal attributes
        let dos_dir = if mode & 0o170000 == 0o040000 { 0x10 } else { 0 };
        c.extend((mode << 16 | dos_dir).to_le_bytes());
        c.extend(offset.to_le_bytes());
        c.extend(name.as_bytes());
        self.count += 1;
    }

    pub fn add_dir(&mut self, name: &str, mode: u32, mtime: u64) {
        let name = format!("{}/", name.trim_end_matches('/'));
        self.entry(&name, 0o040000 | (mode & 0o777), mtime, &[]);
    }

    pub fn add_file(&mut self, name: &str, mode: u32, mtime: u64, data: &[u8]) {
        self.entry(name, 0o100000 | (mode & 0o777), mtime, data);
    }

    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        if self.count >= 0xffff || self.out.len() + self.central.len() >= 0xffff_ffff {
            return Err("too big for a plain zip".into());
        }
        let cd_offset = self.out.len() as u32;
        let cd_len = self.central.len() as u32;
        self.out.extend(&self.central);
        self.out.extend(END.to_le_bytes());
        self.out.extend([0u8; 4]);
        self.out.extend((self.count as u16).to_le_bytes());
        self.out.extend((self.count as u16).to_le_bytes());
        self.out.extend(cd_len.to_le_bytes());
        self.out.extend(cd_offset.to_le_bytes());
        self.out.extend(0u16.to_le_bytes());
        Ok(self.out)
    }
}

/// Zips the folder `src` with its own name as the top level (like `ditto -c
/// -k --keepParent`), in sorted order so the same input gives the same zip.
#[cfg(unix)]
pub fn pack_dir(src: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let top = src.file_name().and_then(|n| n.to_str()).ok_or("the folder needs a UTF-8 name")?.to_string();
    let mut w = Writer::new();
    fn walk(w: &mut Writer, path: &Path, name: &str) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;
        let mode = meta.permissions().mode();
        let mtime = meta.mtime().max(0) as u64;
        if meta.file_type().is_symlink() {
            return Err(format!("{name} is a symbolic link; Volant's zips don't carry links"));
        }
        if meta.is_dir() {
            w.add_dir(name, mode, mtime);
            let mut kids: Vec<_> = std::fs::read_dir(path).map_err(|e| format!("{name}: {e}"))?.filter_map(Result::ok).collect();
            kids.sort_by_key(|k| k.file_name());
            for k in kids {
                let child = k.file_name().to_str().ok_or_else(|| format!("a file in {name} has a non-UTF-8 name"))?.to_string();
                walk(w, &k.path(), &format!("{name}/{child}"))?;
            }
        } else {
            let data = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
            w.add_file(name, mode, mtime, &data);
        }
        Ok(())
    }
    walk(&mut w, src, &top)?;
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_on_disk() {
        let base = std::env::temp_dir().join(format!("volant-zip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let app = base.join("src/Thing.app/Contents/MacOS");
        std::fs::create_dir_all(&app).unwrap();
        let exe: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8 ^ (i / 997) as u8).collect();
        std::fs::write(app.join("Thing"), &exe).unwrap();
        std::fs::write(base.join("src/Thing.app/Contents/Info.plist"), "<plist>thing</plist>").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(app.join("Thing"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let zip = pack_dir(&base.join("src/Thing.app")).unwrap();
            let names: Vec<String> = entries(&zip).unwrap().into_iter().map(|e| e.name).collect();
            assert_eq!(names, ["Thing.app/", "Thing.app/Contents/", "Thing.app/Contents/Info.plist", "Thing.app/Contents/MacOS/", "Thing.app/Contents/MacOS/Thing"]);
            let out = base.join("out");
            extract_to(&zip, &out, 1 << 30).unwrap();
            assert_eq!(std::fs::read(out.join("Thing.app/Contents/MacOS/Thing")).unwrap(), exe);
            let mode = std::fs::metadata(out.join("Thing.app/Contents/MacOS/Thing")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
            assert!(extract_to(&zip, &out, 1000).is_err(), "size limit");
            // A flipped byte in the compressed executable fails the checksum (or the inflate).
            let mut bad = zip.clone();
            let e = entries(&zip).unwrap().into_iter().find(|e| e.name.ends_with("MacOS/Thing")).unwrap();
            bad[e.local as usize + 30 + e.name.len() + 100] ^= 0x55;
            assert!(extract_to(&bad, &base.join("bad"), 1 << 30).is_err());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_escapes() {
        for name in ["../evil", "/etc/passwd", "a/../../b", "a\\b", "./x"] {
            assert!(!safe(name), "{name}");
        }
        assert!(safe("Volant.app/Contents/Info.plist"));
        let mut w = Writer::new();
        w.add_file("../evil", 0o644, 0, b"x");
        let zip = w.finish().unwrap();
        let dir = std::env::temp_dir().join(format!("volant-zip-escape-{}", std::process::id()));
        #[cfg(unix)]
        assert!(extract_to(&zip, &dir, 100).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates() {
        // 2026-11-12 06:47:13 UTC
        let (d, t) = dos_time(1_794_466_033);
        assert_eq!((d >> 9) + 1980, 2026);
        assert_eq!((d >> 5) & 15, 11);
        assert_eq!(d & 31, 12);
        assert_eq!((t >> 11, (t >> 5) & 63, (t & 31) * 2), (6, 47, 12));
    }
}
