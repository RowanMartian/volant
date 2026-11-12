//! The menu bar. Every shortcut lives here as a menu key equivalent, so they
//! all show up in menus and work the way Mac shortcuts are expected to.

use super::command::{self, Mode};
use super::{actions, find, ns, pages, Browser};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{sel, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSPrintInfo};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(isize)]
pub enum Cmd {
    Settings = 1,
    MakeDefault,
    NewTab,
    OpenLocation,
    ReopenTab,
    CloseTab,
    Print,
    Find,
    FindNext,
    FindPrev,
    CopyUrl,
    ToggleSidebar,
    Reload,
    HardReload,
    Stop,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Back,
    Forward,
    Home,
    History,
    PinTab,
    NextTab,
    PrevTab,
    Welcome,
    Source,
    Reader,
    CheckUpdates,
    PickElement,
    BlockerSettings,
    Tab1 = 100,
}

const CMDS: &[Cmd] = &[
    Cmd::Settings, Cmd::MakeDefault, Cmd::NewTab, Cmd::OpenLocation, Cmd::ReopenTab, Cmd::CloseTab, Cmd::Print, Cmd::Find, Cmd::FindNext,
    Cmd::FindPrev, Cmd::CopyUrl, Cmd::ToggleSidebar, Cmd::Reload, Cmd::HardReload, Cmd::Stop, Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset,
    Cmd::Back, Cmd::Forward, Cmd::Home, Cmd::History, Cmd::PinTab, Cmd::NextTab, Cmd::PrevTab, Cmd::Welcome, Cmd::Source, Cmd::Reader, Cmd::CheckUpdates, Cmd::PickElement, Cmd::BlockerSettings,
];

const CMD: u8 = 1;
const SHIFT: u8 = 2;
const OPT: u8 = 4;
const CTRL: u8 = 8;

fn mask(m: u8) -> NSEventModifierFlags {
    let mut f = NSEventModifierFlags::empty();
    if m & CMD != 0 {
        f |= NSEventModifierFlags::Command;
    }
    if m & SHIFT != 0 {
        f |= NSEventModifierFlags::Shift;
    }
    if m & OPT != 0 {
        f |= NSEventModifierFlags::Option;
    }
    if m & CTRL != 0 {
        f |= NSEventModifierFlags::Control;
    }
    f
}

struct Builder<'a> {
    b: &'a Browser,
    menu: Retained<NSMenu>,
}

impl<'a> Builder<'a> {
    fn new(b: &'a Browser, title: &str) -> Self {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(b.mtm), &ns(title));
        Self { b, menu }
    }

    fn raw(&self, title: &str, action: Option<Sel>, key: &str, m: u8, target: bool, tag: isize) -> Retained<NSMenuItem> {
        let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.b.mtm), &ns(title), action, &ns(key)) };
        if !key.is_empty() {
            item.setKeyEquivalentModifierMask(mask(m));
        }
        if target {
            unsafe { item.setTarget(Some(&self.b.delegate as &AnyObject)) };
        }
        item.setTag(tag);
        self.menu.addItem(&item);
        item
    }

    /// An item that runs one of our commands.
    fn cmd(self, title: &str, key: &str, m: u8, c: Cmd) -> Self {
        self.raw(title, Some(sel!(menu:)), key, m, true, c as isize);
        self
    }

    /// The same command on another key, without its own line in the menu.
    fn alt(self, key: &str, m: u8, c: Cmd) -> Self {
        let item = self.raw("", Some(sel!(menu:)), key, m, true, c as isize);
        item.setHidden(true);
        item.setAllowsKeyEquivalentWhenHidden(true);
        self
    }

    /// A standard AppKit action sent along the responder chain.
    fn std(self, title: &str, action: Sel, key: &str, m: u8) -> Self {
        self.raw(title, Some(action), key, m, false, 0);
        self
    }

    fn sep(self) -> Self {
        self.menu.addItem(&NSMenuItem::separatorItem(self.b.mtm));
        self
    }

    fn done(self, bar: &NSMenu) -> Retained<NSMenu> {
        let holder = NSMenuItem::new(self.b.mtm);
        holder.setSubmenu(Some(&self.menu));
        bar.addItem(&holder);
        self.menu
    }
}

pub fn install(b: &Browser) {
    let mtm = b.mtm;
    let bar = NSMenu::new(mtm);

    Builder::new(b, "Volant")
        .std("About Volant", sel!(orderFrontStandardAboutPanel:), "", 0)
        .cmd("Check for Updates…", "", 0, Cmd::CheckUpdates)
        .sep()
        .cmd("Settings…", ",", CMD, Cmd::Settings)
        .cmd("Blocker Settings…", "", 0, Cmd::BlockerSettings)
        .cmd("Make Volant Your Default Browser", "", 0, Cmd::MakeDefault)
        .sep()
        .std("Hide Volant", sel!(hide:), "h", CMD)
        .std("Hide Others", sel!(hideOtherApplications:), "h", CMD | OPT)
        .std("Show All", sel!(unhideAllApplications:), "", 0)
        .sep()
        .std("Quit Volant", sel!(terminate:), "q", CMD)
        .done(&bar);

    Builder::new(b, "File")
        .cmd("New Tab", "t", CMD, Cmd::NewTab)
        .alt("n", CMD, Cmd::NewTab)
        .cmd("Open Location…", "l", CMD, Cmd::OpenLocation)
        .cmd("Reopen Closed Tab", "t", CMD | SHIFT, Cmd::ReopenTab)
        .sep()
        .cmd("Close Tab", "w", CMD, Cmd::CloseTab)
        .sep()
        .cmd("Print…", "p", CMD, Cmd::Print)
        .done(&bar);

    Builder::new(b, "Edit")
        .std("Undo", sel!(undo:), "z", CMD)
        .std("Redo", sel!(redo:), "z", CMD | SHIFT)
        .sep()
        .std("Cut", sel!(cut:), "x", CMD)
        .std("Copy", sel!(copy:), "c", CMD)
        .std("Paste", sel!(paste:), "v", CMD)
        .std("Paste and Match Style", sel!(pasteAsPlainText:), "v", CMD | SHIFT | OPT)
        .std("Select All", sel!(selectAll:), "a", CMD)
        .sep()
        .cmd("Find…", "f", CMD, Cmd::Find)
        .cmd("Find Next", "g", CMD, Cmd::FindNext)
        .cmd("Find Previous", "g", CMD | SHIFT, Cmd::FindPrev)
        .sep()
        .cmd("Copy Page Address", "c", CMD | SHIFT, Cmd::CopyUrl)
        .done(&bar);

    Builder::new(b, "View")
        .cmd("Toggle Sidebar", "s", CMD, Cmd::ToggleSidebar)
        .cmd("Show Reader", "r", CMD | SHIFT, Cmd::Reader)
        .cmd("Block Element…", "b", CMD | OPT, Cmd::PickElement)
        .sep()
        .cmd("Reload Page", "r", CMD, Cmd::Reload)
        .cmd("Reload Ignoring Cache", "r", CMD | OPT, Cmd::HardReload)
        .cmd("Stop", ".", CMD, Cmd::Stop)
        .sep()
        .cmd("Actual Size", "0", CMD, Cmd::ZoomReset)
        .cmd("Zoom In", "=", CMD, Cmd::ZoomIn)
        .alt("+", CMD, Cmd::ZoomIn)
        .cmd("Zoom Out", "-", CMD, Cmd::ZoomOut)
        .sep()
        .std("Enter Full Screen", sel!(toggleFullScreen:), "f", CMD | CTRL)
        .done(&bar);

    Builder::new(b, "History")
        .cmd("Back", "[", CMD, Cmd::Back)
        .cmd("Forward", "]", CMD, Cmd::Forward)
        .cmd("New Tab Page", "h", CMD | SHIFT, Cmd::Home)
        .sep()
        .cmd("Show All History", "y", CMD, Cmd::History)
        .done(&bar);

    let tabs = Builder::new(b, "Tabs")
        .cmd("Pin or Unpin Tab", "d", CMD, Cmd::PinTab)
        .sep()
        .cmd("Show Next Tab", "]", CMD | SHIFT, Cmd::NextTab)
        .alt("\t", CTRL, Cmd::NextTab)
        .cmd("Show Previous Tab", "[", CMD | SHIFT, Cmd::PrevTab)
        .alt("\u{19}", CTRL | SHIFT, Cmd::PrevTab)
        .sep();
    for i in 0..9isize {
        let title = if i == 8 { "Last Tab".to_string() } else { format!("Tab {}", i + 1) };
        tabs.raw(&title, Some(sel!(menu:)), &(i + 1).to_string(), CMD, true, Cmd::Tab1 as isize + i);
    }
    tabs.done(&bar);

    let window = Builder::new(b, "Window")
        .std("Minimize", sel!(performMiniaturize:), "m", CMD)
        .std("Zoom", sel!(performZoom:), "", 0)
        .sep()
        .std("Bring All to Front", sel!(arrangeInFront:), "", 0)
        .done(&bar);

    let help = Builder::new(b, "Help").cmd("Welcome to Volant", "", 0, Cmd::Welcome).cmd("Volant on GitHub", "", 0, Cmd::Source).done(&bar);

    let app = NSApplication::sharedApplication(mtm);
    app.setMainMenu(Some(&bar));
    app.setWindowsMenu(Some(&window));
    app.setHelpMenu(Some(&help));
}

fn active_web(b: &Browser) -> Option<Retained<objc2_web_kit::WKWebView>> {
    b.tabs.active.and_then(|id| b.webs.get(&id)).map(|w| w.web.clone())
}

pub fn run(b: &mut Browser, tag: isize) {
    if (Cmd::Tab1 as isize..Cmd::Tab1 as isize + 9).contains(&tag) {
        actions::select_index(b, (tag - Cmd::Tab1 as isize) as usize);
        return;
    }
    let Some(&cmd) = CMDS.iter().find(|c| **c as isize == tag) else { return };
    match cmd {
        Cmd::Settings => actions::open_or_switch(b, "volant://settings"),
        Cmd::MakeDefault => pages::handle(b, None, "makeDefault", ""),
        Cmd::NewTab => actions::new_tab(b),
        Cmd::OpenLocation => {
            let url = actions::current_url(b);
            command::open(b, Mode::Current, &url);
        }
        Cmd::ReopenTab => actions::reopen_closed(b),
        Cmd::CloseTab => actions::close_current(b),
        Cmd::Print => {
            if let Some(w) = active_web(b) {
                let info = NSPrintInfo::sharedPrintInfo();
                let op = unsafe { w.printOperationWithPrintInfo(&info) };
                unsafe { op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(&b.window, None, None, std::ptr::null_mut()) };
            }
        }
        Cmd::Find => find::open(b),
        Cmd::FindNext => find::step(b, false),
        Cmd::FindPrev => find::step(b, true),
        Cmd::CopyUrl => {
            let url = actions::current_url(b);
            if !url.is_empty() {
                actions::copy_text(&url);
            }
        }
        Cmd::ToggleSidebar => actions::toggle_sidebar(b),
        Cmd::Reload => actions::reload(b, false),
        Cmd::HardReload => actions::reload(b, true),
        Cmd::Stop => actions::stop(b),
        Cmd::ZoomIn => actions::zoom(b, 1),
        Cmd::ZoomOut => actions::zoom(b, -1),
        Cmd::ZoomReset => actions::zoom(b, 0),
        Cmd::Back => actions::back(b),
        Cmd::Forward => actions::forward(b),
        Cmd::Home => actions::navigate(b, actions::NEWTAB),
        Cmd::History => actions::open_or_switch(b, "volant://history"),
        Cmd::PinTab => {
            if let Some(id) = b.tabs.active {
                actions::toggle_pin(b, id);
            }
        }
        Cmd::NextTab => actions::cycle(b, 1),
        Cmd::PrevTab => actions::cycle(b, -1),
        Cmd::Welcome => actions::open_or_switch(b, "volant://welcome"),
        Cmd::Source => {
            actions::open_tab(b, "https://github.com/RowanMartian/volant", true);
        }
        Cmd::Reader => actions::toggle_reader(b),
        Cmd::CheckUpdates => super::updater::check(b, true),
        Cmd::PickElement => super::blocking::pick_element(b),
        Cmd::BlockerSettings => actions::open_or_switch(b, "volant://blocker"),
        Cmd::Tab1 => {}
    }
}
