//! Fonts as layout sees them: a family, size, weight and slant, and how wide
//! a run of text is. The app measures with the system's real fonts; tests and
//! previews use [`ApproxFonts`], built from the standard Helvetica and Times
//! advance widths (which the free Liberation fonts match).

use crate::style::Family;
use std::sync::Mutex;

/// Web font families that are loaded and can be used, by lower-case name.
static DECLARED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Numbers for (web font family, its fallback) pairs, for [`Family::Face`].
static FACES: Mutex<Vec<(String, Family)>> = Mutex::new(Vec::new());

/// A web font family is ready (its files are loaded): styles may use it now.
pub fn declare_face(name: &str) {
    let name = name.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    if let Ok(mut d) = DECLARED.lock() {
        if !d.contains(&name) {
            d.push(name);
        }
    }
}

pub fn is_declared(name: &str) -> bool {
    DECLARED.lock().map(|d| d.iter().any(|n| n == name)).unwrap_or(false)
}

/// The [`Family`] for a web font family with this fallback.
pub fn face(name: &str, fallback: Family) -> Family {
    let Ok(mut faces) = FACES.lock() else { return fallback };
    if let Some(i) = faces.iter().position(|(n, f)| n == name && *f == fallback) {
        return Family::Face(i as u16);
    }
    if faces.len() >= u16::MAX as usize {
        return fallback;
    }
    faces.push((name.to_string(), fallback));
    Family::Face(faces.len() as u16 - 1)
}

/// A web font's family name (lower case) and fallback.
pub fn face_info(i: u16) -> Option<(String, Family)> {
    FACES.lock().ok()?.get(i as usize).cloned()
}

/// Web font files Canard can read itself: (family, weight range, italic, the parsed file).
type FontFile = (String, (u16, u16), bool, std::sync::Arc<crate::sfnt::Face>);
static FILES: Mutex<Vec<FontFile>> = Mutex::new(Vec::new());

/// Adds a web font file (TrueType, OpenType or WOFF) for a family; false if Canard can't read it.
/// The family is declared too, so styles and canvases use it.
pub fn add_file(family: &str, weight: (u16, u16), italic: bool, bytes: &[u8]) -> bool {
    let Some(face) = crate::sfnt::Face::parse(bytes) else { return false };
    let name = family.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    if let Ok(mut f) = FILES.lock() {
        if f.len() >= 512 {
            f.remove(0);
        }
        f.push((name.clone(), weight, italic, face));
    }
    declare_face(&name);
    true
}

/// The loaded file closest to `font`, when it's set in a web font Canard has the file for:
/// the right slant first, then a weight range that holds its weight, else the nearest.
pub fn file_for(font: &Font) -> Option<std::sync::Arc<crate::sfnt::Face>> {
    let Family::Face(i) = font.family else { return None };
    let (name, _) = face_info(i)?;
    let files = FILES.lock().ok()?;
    files
        .iter()
        .filter(|f| f.0 == name)
        .min_by_key(|(_, (lo, hi), italic, _)| {
            let off = if font.weight < *lo { lo - font.weight } else { font.weight.saturating_sub(*hi) };
            ((*italic != font.italic) as u32) << 16 | off as u32
        })
        .map(|f| f.3.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Font {
    pub family: Family,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
}

impl Font {
    pub fn bold(&self) -> bool {
        self.weight >= 600
    }
    /// A hashable identity, for caches.
    pub fn key(&self) -> (Family, u32, u16, bool) {
        (self.family, (self.size * 64.0) as u32, self.weight, self.italic)
    }
}

/// What layout needs from a font system.
pub trait Fonts {
    /// The advance width of `text` set in `font`, in px.
    fn width(&self, text: &str, font: &Font) -> f32;
    /// Distance from the baseline up to the top of the em box.
    fn ascent(&self, font: &Font) -> f32 {
        font.size * 0.8
    }
    /// Distance from the baseline down.
    fn descent(&self, font: &Font) -> f32 {
        font.size * 0.2
    }
}

/// Helvetica advance widths for ASCII 32..=126, in 1/1000 em.
const SANS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584,
    584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667,
    611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500,
    722, 500, 500, 500, 334, 260, 334, 584,
];

/// Times Roman advance widths for ASCII 32..=126, in 1/1000 em.
const SERIF: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564,
    564, 564, 444, 921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722, 722,
    611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500,
    722, 500, 500, 444, 480, 200, 480, 541,
];

/// Metric-table fonts, for tests and SVG previews.
pub struct ApproxFonts;

impl Fonts for ApproxFonts {
    fn width(&self, text: &str, font: &Font) -> f32 {
        // Ahem, the CSS tests' font: every glyph a 1em box.
        if crate::raster::is_ahem(font) {
            return text.chars().count() as f32 * font.size;
        }
        let per_mille: u32 = text
            .chars()
            .map(|c| match font.family.generic() {
                // Combining marks and zero-width characters take no room of their own.
                _ if matches!(c as u32, 0x300..=0x36f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f | 0x200b..=0x200d | 0xfeff) => 0,
                Family::Mono => 600,
                fam => {
                    let table = if fam == Family::Serif { &SERIF } else { &SANS };
                    match c as u32 {
                        32..=126 => table[(c as u32 - 32) as usize] as u32,
                        0x3000..=0x9fff | 0xac00..=0xd7af | 0xff00..=0xffef => 1000, // CJK, Hangul, full-width
                        0xa0 => table[0] as u32,                                     // no-break space
                        0x2000..=0x200a => 250,                                      // spaces
                        _ => 556,
                    }
                }
            })
            .sum();
        let bold = if font.bold() && font.family.generic() != Family::Mono { 1.06 } else { 1.0 };
        per_mille as f32 / 1000.0 * font.size * bold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        let f = Font { family: Family::Sans, size: 10.0, weight: 400, italic: false };
        assert!((ApproxFonts.width("Hi", &f) - 9.44).abs() < 1e-3); // H 722 + i 222
        let m = Font { family: Family::Mono, ..f };
        assert_eq!(ApproxFonts.width("abcd", &m), 24.0);
    }
}
