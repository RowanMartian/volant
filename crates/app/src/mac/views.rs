//! The few custom views Volant needs. Everything is a plain layer-backed view
//! with a fill colour and rounded corners.

use super::theme::{self, Rgba};
use super::{actions, ns, rect};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSEvent, NSFont, NSFontWeight, NSFontWeightBold, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSImage,
    NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLineBreakMode, NSMenu, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSView,
};
use objc2_foundation::{NSPoint, NSRect};
use std::cell::{Cell, RefCell};

/// What a click on a [`Hit`] view means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Kind {
    None = 0,
    Tab,
    CloseTab,
    NewTab,
    Address,
    Back,
    Forward,
    Reload,
    Settings,
    Sidebar,
    Suggestion,
    Scrim,
    Downloads,
    FindPrev,
    FindNext,
    FindClose,
    Panel,
    Reader,
    Update,
    ShowSidebar,
}

impl Kind {
    fn from(v: u32) -> Self {
        use Kind::*;
        [None, Tab, CloseTab, NewTab, Address, Back, Forward, Reload, Settings, Sidebar, Suggestion, Scrim, Downloads, FindPrev, FindNext, FindClose, Panel, Reader, Update, ShowSidebar]
            .get(v as usize)
            .copied()
            .unwrap_or(None)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Click {
    Primary,
    Middle,
}

pub struct HitIvars {
    kind: Cell<u32>,
    data: Cell<u64>,
    hover: Cell<bool>,
    selected: Cell<bool>,
    hoverable: Cell<bool>,
    /// Shown only while hovered or selected (a tab's close button).
    aux: RefCell<Option<Retained<NSView>>>,
    fill: Cell<Option<Rgba>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; Hit does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = HitIvars]
    pub struct Hit;

    impl Hit {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn can_move_window(&self) -> bool {
            false
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, p: NSPoint) -> Option<Retained<NSView>> {
            let r: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: p] };
            match r {
                Some(v) if v.isKindOfClass(Hit::class()) || is_editable_field(&v) => Some(v),
                Some(_) => Some(self.retain().into_super()),
                None => None,
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            let (k, d) = (Kind::from(self.ivars().kind.get()), self.ivars().data.get());
            if k == Kind::Panel {
                return;
            }
            actions::hit(k, d, Click::Primary);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, _e: &NSEvent) {
            actions::hit(Kind::from(self.ivars().kind.get()), self.ivars().data.get(), Click::Middle);
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _e: &NSEvent) {
            self.ivars().hover.set(true);
            self.restyle();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            self.ivars().hover.set(false);
            self.restyle();
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _e: &NSEvent) -> Option<Retained<NSMenu>> {
            match Kind::from(self.ivars().kind.get()) {
                Kind::Tab => actions::tab_menu(self.ivars().data.get()),
                _ => None,
            }
        }
    }
);

fn is_editable_field(v: &NSView) -> bool {
    // Text fields (and their field editors) must get their own clicks.
    // Plain labels are NSTextFields too; they should let the click through.
    if let Some(tf) = (v as &AnyObject).downcast_ref::<NSTextField>() {
        return tf.isEditable();
    }
    objc2::runtime::AnyClass::get(c"NSTextView").map(|c| v.isKindOfClass(c)).unwrap_or(false)
}

impl Hit {
    pub fn new(kind: Kind, data: u64, frame: NSRect, hoverable: bool, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HitIvars {
            kind: Cell::new(kind as u32),
            data: Cell::new(data),
            hover: Cell::new(false),
            selected: Cell::new(false),
            hoverable: Cell::new(hoverable),
            aux: RefCell::new(None),
            fill: Cell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        if let Some(l) = this.layer() {
            l.setCornerRadius(8.0);
            set_continuous(&l);
        }
        if hoverable {
            let opts = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), rect(0.0, 0.0, 0.0, 0.0), opts, Some(&this as &AnyObject), None)
            };
            this.addTrackingArea(&area);
        }
        this
    }

    pub fn set_selected(&self, s: bool) {
        if self.ivars().selected.get() != s {
            self.ivars().selected.set(s);
            self.restyle();
        }
    }

    pub fn set_aux(&self, v: &NSView) {
        *self.ivars().aux.borrow_mut() = Some(v.retain());
        self.restyle();
    }

    /// A fixed fill instead of the hover/selected fills.
    pub fn set_fill(&self, c: Option<Rgba>) {
        self.ivars().fill.set(c);
        self.restyle();
    }

    pub fn restyle(&self) {
        let t = theme::current();
        let iv = self.ivars();
        let bg = if let Some(f) = iv.fill.get() {
            if iv.hover.get() && iv.hoverable.get() {
                blend(f, t.hover)
            } else {
                f
            }
        } else if iv.selected.get() {
            t.active
        } else if iv.hover.get() && iv.hoverable.get() {
            t.hover
        } else {
            Rgba(0.0, 0.0, 0.0, 0.0)
        };
        if let Some(l) = self.layer() {
            l.setBackgroundColor(Some(&theme::cg(bg)));
        }
        if let Some(a) = iv.aux.borrow().as_ref() {
            a.setHidden(!(iv.hover.get() || iv.selected.get()));
        }
    }
}

/// `over` drawn on top of `base`.
fn blend(base: Rgba, over: Rgba) -> Rgba {
    let a = over.3;
    Rgba(base.0 * (1.0 - a) + over.0 * a, base.1 * (1.0 - a) + over.1 * a, base.2 * (1.0 - a) + over.2 * a, base.3.max(a))
}

pub struct RootIvars;

define_class!(
    // SAFETY: NSView has no subclassing requirements; Flipped does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RootIvars]
    pub struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn can_move_window(&self) -> bool {
            true
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            // Only the window's root view reacts; it is the one tagged 1.
            if self.tag_value() == 1 {
                super::with(super::theme::refresh);
            }
        }
    }
);

impl Flipped {
    pub fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RootIvars);
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        this
    }

    fn tag_value(&self) -> isize {
        let ptr = self as *const Self as usize;
        ROOT.with(|r| if r.get() == ptr { 1 } else { 0 })
    }

    pub fn mark_root(&self) {
        let ptr = self as *const Self as usize;
        ROOT.with(|r| r.set(ptr));
    }
}

thread_local! {
    static ROOT: Cell<usize> = const { Cell::new(0) };
}

pub fn set_continuous(layer: &objc2_quartz_core::CALayer) {
    layer.setCornerCurve(unsafe { objc2_quartz_core::kCACornerCurveContinuous });
}

/// A rounded, filled rectangle.
pub fn plate(frame: NSRect, radius: f64, mtm: MainThreadMarker) -> Retained<Flipped> {
    let v = Flipped::new(frame, mtm);
    if let Some(l) = v.layer() {
        l.setCornerRadius(radius);
        set_continuous(&l);
    }
    v
}

pub fn fill(v: &NSView, c: Rgba) {
    if let Some(l) = v.layer() {
        l.setBackgroundColor(Some(&theme::cg(c)));
    }
}

#[derive(Clone, Copy)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

pub fn font(size: f64, w: Weight) -> Retained<NSFont> {
    let weight: NSFontWeight = unsafe {
        match w {
            Weight::Regular => NSFontWeightRegular,
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
            Weight::Bold => NSFontWeightBold,
        }
    };
    NSFont::systemFontOfSize_weight(size, weight)
}

pub fn label(text: &str, size: f64, w: Weight, color: Rgba, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm);
    l.setFont(Some(&font(size, w)));
    l.setTextColor(Some(&theme::ns(color)));
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l.setSelectable(false);
    l
}

pub fn set_text(l: &NSTextField, text: &str) {
    if l.stringValue().to_string() != text {
        l.setStringValue(&ns(text));
    }
}

pub fn recolor(l: &NSTextField, c: Rgba) {
    l.setTextColor(Some(&theme::ns(c)));
}

pub fn symbol(name: &str, size: f64, w: Weight) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None)?;
    let weight: NSFontWeight = unsafe {
        match w {
            Weight::Regular => NSFontWeightRegular,
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
            Weight::Bold => NSFontWeightBold,
        }
    };
    let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, weight);
    img.imageWithSymbolConfiguration(&cfg)
}

pub fn image_view(img: Option<&NSImage>, frame: NSRect, tint: Option<Rgba>, mtm: MainThreadMarker) -> Retained<NSImageView> {
    let v = NSImageView::initWithFrame(NSImageView::alloc(mtm), frame);
    v.setImage(img);
    v.setImageScaling(NSImageScaling::ScaleProportionallyDown);
    if let Some(t) = tint {
        v.setContentTintColor(Some(&theme::ns(t)));
    }
    v
}

pub fn tint(v: &NSImageView, c: Rgba) {
    v.setContentTintColor(Some(&theme::ns(c)));
}

/// A square icon button with a hover fill.
pub struct IconButton {
    pub hit: Retained<Hit>,
    pub img: Retained<NSImageView>,
}

impl IconButton {
    pub fn new(kind: Kind, symbol_name: &str, size: f64, frame: NSRect, tooltip: &str, mtm: MainThreadMarker) -> Self {
        let hit = Hit::new(kind, 0, frame, true, mtm);
        if let Some(l) = hit.layer() {
            l.setCornerRadius(7.0);
        }
        let img = image_view(symbol(symbol_name, size, Weight::Medium).as_deref(), rect(0.0, 0.0, frame.size.width, frame.size.height), None, mtm);
        hit.addSubview(&img);
        hit.setToolTip(Some(&ns(tooltip)));
        Self { hit, img }
    }

    pub fn set_enabled(&self, on: bool) {
        self.hit.setAlphaValue(if on { 1.0 } else { 0.35 });
    }
}
