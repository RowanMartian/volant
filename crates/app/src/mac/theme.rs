//! Colours. Flat surfaces told apart by fill alone: no borders, shadows or gradients.

use super::{ui, Browser};
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSColor};
use objc2_core_graphics::CGColor;
use objc2_foundation::NSArray;
use std::cell::Cell;
use volant_core::store::Appearance;

#[derive(Clone, Copy, PartialEq)]
pub struct Rgba(pub f64, pub f64, pub f64, pub f64);

const fn hex(v: u32) -> Rgba {
    Rgba(((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0, 1.0)
}
const fn alpha(c: Rgba, a: f64) -> Rgba {
    Rgba(c.0, c.1, c.2, a)
}

#[derive(Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// Window and sidebar.
    pub side: Rgba,
    /// Behind web content.
    pub content: Rgba,
    /// Floating panels (command bar).
    pub panel: Rgba,
    pub fill: Rgba,
    pub hover: Rgba,
    pub active: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub accent: Rgba,
    pub accent_soft: Rgba,
    pub scrim: Rgba,
}

pub const LIGHT: Theme = Theme {
    dark: false,
    side: hex(0xf5f5f7),
    content: hex(0xffffff),
    panel: hex(0xffffff),
    fill: hex(0xe8e8ec),
    hover: alpha(Rgba(0.0, 0.0, 0.0, 1.0), 0.045),
    active: alpha(Rgba(0.0, 0.0, 0.0, 1.0), 0.075),
    text: hex(0x1d1d1f),
    text2: hex(0x6e6e73),
    text3: hex(0xa1a1a6),
    accent: hex(0x0078f0),
    accent_soft: alpha(hex(0x0078f0), 0.10),
    scrim: alpha(Rgba(0.0, 0.0, 0.0, 1.0), 0.18),
};

pub const DARK: Theme = Theme {
    dark: true,
    side: hex(0x232325),
    content: hex(0x1c1c1e),
    panel: hex(0x2c2c2e),
    fill: hex(0x313134),
    hover: alpha(Rgba(1.0, 1.0, 1.0, 1.0), 0.055),
    active: alpha(Rgba(1.0, 1.0, 1.0, 1.0), 0.09),
    text: hex(0xf5f5f7),
    text2: hex(0xa1a1a6),
    text3: hex(0x6e6e73),
    accent: hex(0x1f95ff),
    accent_soft: alpha(hex(0x1f95ff), 0.18),
    scrim: alpha(Rgba(0.0, 0.0, 0.0, 1.0), 0.42),
};

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(LIGHT) };
}

pub fn current() -> Theme {
    CURRENT.with(|c| c.get())
}

pub fn ns(c: Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, c.3)
}

pub fn cg(c: Rgba) -> Retained<CGColor> {
    ns(c).CGColor()
}

pub fn is_dark(mtm: MainThreadMarker) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    let appearance = app.effectiveAppearance();
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    appearance.bestMatchFromAppearancesWithNames(&names).map(|n| &*n == unsafe { NSAppearanceNameDarkAqua }).unwrap_or(false)
}

pub fn apply_appearance(a: Appearance, mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let appearance = match a {
        Appearance::System => None,
        Appearance::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        Appearance::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
    };
    app.setAppearance(appearance.as_deref());
}

/// Recomputes the theme from the current appearance and repaints everything.
pub fn refresh(b: &mut Browser) {
    let t = if is_dark(b.mtm) { DARK } else { LIGHT };
    CURRENT.with(|c| c.set(t));
    ui::repaint(b);
    ui::relight_soon();
    super::engine_view::sync(b);
}
