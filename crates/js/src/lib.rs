//! volant-js: Volant's own JavaScript engine, written from scratch in Rust and
//! built up chunk by chunk, like its web engine.
//!
//! * [`parse`]: the lexer (with automatic semicolon insertion) and parser.
//! * [`interp`]: a tree-walking interpreter: closures, prototypes, `this`,
//!   `new`, classes with `extends`/`super`, destructuring, spread and rest,
//!   exceptions, labelled loops, getters and setters, and a step budget.
//! * [`builtins`]: Object, Function, Array, String, Number, Boolean, Math,
//!   JSON, the Error types, Map, Set, Date, console and timers.
//!
//! * [`regex`] and [`regexp`]: regular expressions and the methods using them.
//! * [`promise`]: the event loop, Promises, async/await, timers, generators,
//!   iterators and Symbols.
//!
//! The DOM lives with the web engine (volant-engine's `script` module).

pub mod builtins;
pub mod interp;
pub mod parse;
pub mod promise;
pub mod regex;
pub mod regexp;

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
