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
//! Not yet: regular expressions, Promises and async, generators, Symbols,
//! typed arrays, and the DOM. Those are the next chunks.

pub mod builtins;
pub mod interp;
pub mod parse;

pub use interp::{Interp, Value};

/// Runs a script; returns what it printed with console.log, or the error.
///
/// It runs on its own thread with a large stack, so deep (but bounded) JS
/// recursion can't overflow the caller's stack.
pub fn run(src: &str) -> Result<Vec<String>, String> {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut i = Interp::new();
            i.run(&src)?;
            Ok(i.output)
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "the script crashed the interpreter".to_string())?
}

#[cfg(test)]
mod tests;
