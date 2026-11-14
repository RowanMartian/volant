//! volant-js: Volant's own JavaScript engine, written from scratch in Rust and
//! built up chunk by chunk, like its web engine.
//!
//! * [`parse`]: the lexer (with automatic semicolon insertion) and parser.
//! * [`compile`]: the compiler from syntax trees to bytecode, resolving every
//!   variable to a frame slot or a captured environment slot once.
//! * [`vm`]: the bytecode VM: frames without native recursion, inline caches
//!   for property lookups, exceptions, and generators and async functions
//!   that really suspend.
//! * [`interp`]: values, objects with prototypes, conversions, property
//!   access and calls, shared by the VM and the built-ins.
//! * [`builtins`]: Object, Function, Array, String, Number, Boolean, Math,
//!   JSON, the Error types, Map, Set, Date, console and timers.
//!
//! * [`regex`] and [`regexp`]: regular expressions and the methods using them.
//! * [`typed`]: ArrayBuffer, the typed arrays and DataView.
//! * [`promise`]: the event loop, Promises, async/await, timers, generators,
//!   iterators and Symbols.
//!
//! The DOM lives with the web engine (volant-engine's `script` module).

pub mod builtins;
pub mod compile;
pub mod interp;
pub mod module;
pub mod bigint;
pub mod normalize;
pub mod parse;
pub mod promise;
pub mod proxy;
pub mod regex;
pub mod regexp;
pub mod table;
pub mod typed;
pub mod tz;
pub mod vm;

pub use interp::{Interp, Value};

/// Runs a script. Returns what it printed with console.log, and how it ended:
/// `Err` holds an uncaught exception or a syntax error.
///
/// It runs on its own thread with a large stack, so deep (but bounded) JS
/// recursion can't overflow the caller's stack.
pub fn run(src: &str) -> (Vec<String>, Result<(), String>) {
    let src = src.to_string();
    let spawned = std::thread::Builder::new().stack_size(256 << 20).spawn(move || {
        let mut i = Interp::new();
        i.max_depth = 4000;
        // A standalone script (a benchmark, a test) may run for a long time.
        i.max_steps = 2_000_000_000;
        let r = i.run(&src).map(|_| ());
        // Then the event loop: timers in order of their (virtual) due times.
        i.run_jobs(f64::INFINITY, 100_000);
        let r = r.and_then(|_| match i.errors.first() {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        });
        (std::mem::take(&mut i.output), r)
    });
    match spawned {
        Ok(h) => h.join().unwrap_or_else(|_| (Vec::new(), Err("the script crashed the interpreter".into()))),
        Err(e) => (Vec::new(), Err(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
