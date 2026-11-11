//! Volant: a featherweight web browser for macOS.
//!
//! The window, sidebar, command bar and menus are native AppKit, driven
//! directly through the Objective-C runtime (no Electron, no toolkit, no
//! bundled engine). Pages render in the WebKit that ships with macOS.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Volant runs on macOS.");
    std::process::exit(1);
}
