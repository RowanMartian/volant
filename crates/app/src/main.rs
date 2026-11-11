//! Volant: a featherweight web browser for macOS.
//!
//! The window, sidebar, command bar and menus are native AppKit, driven
//! directly through the Objective-C runtime (no Electron, no toolkit, no
//! bundled engine). Pages render in the WebKit that ships with macOS.

#[cfg(target_os = "macos")]
mod mac;

/// When the process started, for measuring how long launch takes.
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    mac::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Volant runs on macOS.");
    std::process::exit(1);
}
