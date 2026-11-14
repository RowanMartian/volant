//! JavaScript source → syntax tree: a lexer (with the newline tracking that
//! automatic semicolon insertion needs) and a recursive-descent parser with
//! precedence climbing for expressions.

use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    /// A BigInt literal, in decimal.
    BigInt(String),
    Str(String),
    /// A template literal: its string pieces and the tokens of each `${...}`.
    /// Cooked pieces, the expressions' tokens, and the raw source of each piece.
    Template(Vec<String>, Vec<Vec<Token>>, Vec<String>),
    Ident(String),
    Punct(&'static str),
    /// A regular expression literal: (pattern, flags)
    Regex(String, String),
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// A line break came before it (for automatic semicolon insertion).
    pub nl: bool,
    pub line: u32,
    /// The character offset in the source (minified scripts are one long line).
    pub at: u32,
    /// The offset just past it.
    pub end: u32,
}

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^",
    "!", "~", "?", ":", "=", ".",
];

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let s: Vec<char> = src.chars().collect();
    let mut lx = Lexer { s: &s, i: 0, line: 1 };
    lx.tokens(false)
}

struct Lexer<'a> {
    s: &'a [char],
    i: usize,
    line: u32,
}

impl Lexer<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("line {}: {msg}", Where(self.line, self.i as u32)))
    }

    /// Tokens to the end of the source, or (`nested`: inside a template's
    /// `${`) to the `}` closing it. They end with an end marker.
    fn tokens(&mut self, nested: bool) -> Result<Vec<Token>, String> {
        let s = self.s;
        let mut out: Vec<Token> = Vec::new();
        let mut nl = false;
        let mut braces = 0u32;
        // For telling a regular expression from a division after `)` and `}`:
        // does each open `(` follow if/for/while/with, and does each `{` open a block?
        let mut parens: Vec<bool> = Vec::new();
        let mut blocks: Vec<bool> = Vec::new();
        let mut slash_starts_regex = false;
        while self.i < s.len() {
            let c = s[self.i];
            if c == '\n' || c == '\u{2028}' || c == '\u{2029}' {
                nl = true;
                self.line += 1;
                self.i += 1;
                continue;
            }
            if c.is_whitespace() || c == '\u{feff}' {
                self.i += 1;
                continue;
            }
            let html_open = c == '<' && s.get(self.i + 1..self.i + 4) == Some(&['!', '-', '-'][..]);
            let html_close = c == '-' && (nl || out.is_empty()) && s.get(self.i + 1..self.i + 3) == Some(&['-', '>'][..]);
            if (c == '/' && s.get(self.i + 1) == Some(&'/')) || (!nested && (html_open || html_close)) {
                while self.i < s.len() && s[self.i] != '\n' {
                    self.i += 1;
                }
                continue;
            }
            if c == '/' && s.get(self.i + 1) == Some(&'*') {
                self.i += 2;
                while self.i + 1 < s.len() && !(s[self.i] == '*' && s[self.i + 1] == '/') {
                    if s[self.i] == '\n' {
                        nl = true;
                        self.line += 1;
                    }
                    self.i += 1;
                }
                self.i += 2;
                continue;
            }
            if nested && c == '}' && braces == 0 {
                out.push(Token { tok: Tok::Eof, nl: true, line: self.line, at: self.i as u32, end: self.i as u32 });
                self.i += 1;
                return Ok(out);
            }
            if nested && c == '{' {
                braces += 1;
            } else if nested && c == '}' {
                braces -= 1;
            }
            let (line, at) = (self.line, self.i as u32);
            let prev = out.last().map(|t| &t.tok);
            let tok = self.token(prev, slash_starts_regex)?;
            slash_starts_regex = false;
            match &tok {
                Tok::Punct("(") => parens.push(matches!(prev, Some(Tok::Ident(w)) if matches!(w.as_str(), "if" | "for" | "while" | "with"))),
                Tok::Punct(")") => slash_starts_regex = parens.pop().unwrap_or(false),
                Tok::Punct("{") => blocks.push(match prev {
                    None | Some(Tok::Punct(";" | "{" | "}" | ")" | "=>")) => true,
                    Some(Tok::Ident(w)) => matches!(w.as_str(), "else" | "do" | "try" | "finally"),
                    _ => false,
                }),
                Tok::Punct("}") => slash_starts_regex = blocks.pop().unwrap_or(true),
                _ => {}
            }
            out.push(Token { tok, nl, line, at, end: self.i as u32 });
            nl = false;
        }
        if nested {
            return self.err("unterminated template");
        }
        out.push(Token { tok: Tok::Eof, nl: true, line: self.line, at: s.len() as u32, end: s.len() as u32 });
        Ok(out)
    }

    /// The token at `i` (not whitespace or a comment).
    fn token(&mut self, prev: Option<&Tok>, slash_starts_regex: bool) -> Result<Tok, String> {
        let s = self.s;
        let c = s[self.i];
        if c.is_ascii_digit() || (c == '.' && s.get(self.i + 1).map(|d| d.is_ascii_digit()).unwrap_or(false)) {
            return self.number();
        }
        if c == '"' || c == '\'' {
            self.i += 1;
            let mut v = String::new();
            while self.i < s.len() && s[self.i] != c {
                if s[self.i] == '\n' {
                    return self.err("unterminated string");
                }
                if s[self.i] == '\\' {
                    self.i += 1;
                    escape(s, &mut self.i, &mut v, &mut self.line);
                } else {
                    v.push(s[self.i]);
                    self.i += 1;
                }
            }
            if self.i >= s.len() {
                return self.err("unterminated string");
            }
            self.i += 1;
            return Ok(Tok::Str(v));
        }
        if c == '`' {
            self.i += 1;
            let (mut strs, mut exprs, mut raws) = (vec![String::new()], Vec::new(), vec![String::new()]);
            while self.i < s.len() && s[self.i] != '`' {
                if s[self.i] == '\\' {
                    let from = self.i;
                    self.i += 1;
                    escape(s, &mut self.i, strs.last_mut().expect("a piece"), &mut self.line);
                    raws.last_mut().expect("a piece").extend(s[from..self.i.min(s.len())].iter().filter(|&&c| c != '\r'));
                } else if s[self.i] == '$' && s.get(self.i + 1) == Some(&'{') {
                    self.i += 2;
                    exprs.push(self.tokens(true)?);
                    strs.push(String::new());
                    raws.push(String::new());
                } else {
                    if s[self.i] != '\r' {
                        raws.last_mut().expect("a piece").push(s[self.i]);
                    }
                    if s[self.i] == '\n' {
                        self.line += 1;
                    }
                    strs.last_mut().expect("a piece").push(s[self.i]);
                    self.i += 1;
                }
            }
            if self.i >= s.len() {
                return self.err("unterminated template");
            }
            self.i += 1;
            return Ok(Tok::Template(strs, exprs, raws));
        }
        let private = c == '#' && s.get(self.i + 1).map(|&d| id_part(d) && !d.is_ascii_digit()).unwrap_or(false);
        if (id_part(c) && !c.is_ascii_digit()) || private || c == '\\' {
            let mut name = String::new();
            if private {
                // Private names live under a hidden key, so they never show
                // in keys, for-in or JSON and cannot meet a real "#x" key.
                name.push_str("\0#");
                self.i += 1;
            }
            while self.i < s.len() && (id_part(s[self.i]) || s[self.i] == '\\') {
                if s[self.i] == '\\' {
                    // A \u escape in a name.
                    self.i += 1;
                    escape(s, &mut self.i, &mut name, &mut self.line);
                } else {
                    name.push(s[self.i]);
                    self.i += 1;
                }
            }
            return Ok(Tok::Ident(name));
        }
        if c == '/' && (slash_starts_regex || regex_allowed(prev)) {
            let mut j = self.i + 1;
            let mut class = false;
            while j < s.len() {
                match s[j] {
                    '\\' => j += 1,
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => break,
                    '\n' => return self.err("unterminated regular expression"),
                    _ => {}
                }
                j += 1;
            }
            if j >= s.len() {
                return self.err("unterminated regular expression");
            }
            let body: String = s[self.i + 1..j].iter().collect();
            j += 1;
            let fstart = j;
            while j < s.len() && s[j].is_alphanumeric() {
                j += 1;
            }
            let flags: String = s[fstart..j].iter().collect();
            self.i = j;
            return Ok(Tok::Regex(body, flags));
        }
        // A `#!` line at the very start.
        if c == '#' && self.i == 0 && s.get(1) == Some(&'!') {
            while self.i < s.len() && s[self.i] != '\n' {
                self.i += 1;
            }
            return Ok(Tok::Punct(";"));
        }
        let rest = &s[self.i..(self.i + 4).min(s.len())];
        let starts = |p: &str| p.chars().count() <= rest.len() && p.chars().zip(rest.iter()).all(|(a, b)| a == *b);
        match PUNCTS.iter().find(|p| starts(p)) {
            Some(p) => {
                // `?.5` is a ternary and a number, not optional chaining.
                if *p == "?." && s.get(self.i + 2).map(|d| d.is_ascii_digit()).unwrap_or(false) {
                    self.i += 1;
                    Ok(Tok::Punct("?"))
                } else {
                    self.i += p.len();
                    Ok(Tok::Punct(p))
                }
            }
            None => self.err(&format!("unexpected character {c:?}")),
        }
    }

    fn number(&mut self) -> Result<Tok, String> {
        let s = self.s;
        let c = s[self.i];
        let start = self.i;
        if c == '0' && matches!(s.get(self.i + 1), Some('x' | 'X' | 'b' | 'B' | 'o' | 'O')) {
            let radix = match s[self.i + 1] {
                'x' | 'X' => 16,
                'b' | 'B' => 2,
                _ => 8,
            };
            self.i += 2;
            let ds = self.i;
            while self.i < s.len() && (s[self.i].is_ascii_alphanumeric() || s[self.i] == '_') {
                self.i += 1;
            }
            let mut digits: String = s[ds..self.i].iter().filter(|&&c| c != '_').collect();
            if digits.ends_with('n') {
                digits.pop();
                return match crate::bigint::BigInt::parse_digits(&digits, radix) {
                    Some(b) => Ok(Tok::BigInt(b.to_string_radix(10))),
                    None => self.err("bad BigInt"),
                };
            }
            return match u128::from_str_radix(&digits, radix) {
                Ok(n) => Ok(Tok::Num(n as f64)),
                // Longer than 128 bits (obfuscators write such constants): in floating point.
                Err(_) if !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)) => {
                    Ok(Tok::Num(digits.chars().fold(0.0, |n, c| n * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64)))
                }
                Err(_) => self.err("bad number"),
            };
        }
        // Digits, then at most one '.' and its digits: in `1..toString()` and
        // `1.5.toFixed()` the second dot is a property access.
        let digits = |i: &mut usize| {
            while *i < s.len() && (s[*i].is_ascii_digit() || s[*i] == '_') {
                *i += 1;
            }
        };
        digits(&mut self.i);
        if s.get(self.i) == Some(&'.') {
            self.i += 1;
            digits(&mut self.i);
        }
        if self.i < s.len() && (s[self.i] == 'e' || s[self.i] == 'E') {
            self.i += 1;
            if self.i < s.len() && (s[self.i] == '+' || s[self.i] == '-') {
                self.i += 1;
            }
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let text: String = s[start..self.i].iter().filter(|&&c| c != '_').collect();
        if s.get(self.i) == Some(&'n') {
            self.i += 1;
            return match crate::bigint::BigInt::parse_digits(&text, 10) {
                Some(b) => Ok(Tok::BigInt(b.to_string_radix(10))),
                None => self.err(&format!("bad BigInt {text}n")),
            };
        }
        match text.parse() {
            Ok(n) => Ok(Tok::Num(n)),
            Err(_) => self.err(&format!("bad number {text}")),
        }
    }
}

/// Can this character be part of a name? Outside strings and comments, the
/// only non-ASCII characters in working scripts are in names (letters,
/// combining marks, `℘`), so anything that isn't a space or punctuation is.
fn id_part(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_' || c == '$';
    }
    if c.is_whitespace() || c == '\u{feff}' {
        return false;
    }
    match c as u32 {
        0x200C | 0x200D | 0x203F | 0x2040 | 0xAA | 0xB5 | 0xB7 | 0xBA => true,
        0xA0..=0xBF | 0xD7 | 0xF7 | 0x2000..=0x206F | 0x2190..=0x23FF | 0x2500..=0x27BF | 0x2E00..=0x2E7F | 0x3000..=0x3003 | 0x3008..=0x3020 | 0xFF01..=0xFF0F => false,
        _ => true,
    }
}

/// Whether a `/` after this token starts a regular expression (rather than dividing).
fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Some(Tok::Ident(w)) => matches!(
            w.as_str(),
            "return" | "typeof" | "instanceof" | "in" | "of" | "new" | "delete" | "void" | "throw" | "case" | "do" | "else" | "yield" | "await"
        ),
        _ => false,
    }
}

fn escape(s: &[char], i: &mut usize, v: &mut String, line: &mut u32) {
    let Some(&e) = s.get(*i) else { return };
    *i += 1;
    match e {
        'n' => v.push('\n'),
        't' => v.push('\t'),
        'r' => v.push('\r'),
        'b' => v.push('\u{8}'),
        'f' => v.push('\u{c}'),
        'v' => v.push('\u{b}'),
        '0' if !s.get(*i).map(|d| d.is_ascii_digit()).unwrap_or(false) => v.push('\0'),
        'x' => {
            let hex: String = s[*i..(*i + 2).min(s.len())].iter().collect();
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                v.push(ch);
                *i += 2;
            }
        }
        'u' => {
            let (hex, len) = if s.get(*i) == Some(&'{') {
                let end = s[*i..].iter().position(|&c| c == '}').map(|p| *i + p).unwrap_or(*i);
                (s[*i + 1..end].iter().collect::<String>(), end + 1 - *i)
            } else {
                (s[*i..(*i + 4).min(s.len())].iter().collect::<String>(), 4)
            };
            let mut code = u32::from_str_radix(&hex, 16).unwrap_or(0xfffd);
            *i += len;
            // A surrogate pair written as two escapes.
            if (0xd800..0xdc00).contains(&code) && s.get(*i) == Some(&'\\') && s.get(*i + 1) == Some(&'u') {
                let lo: String = s[*i + 2..(*i + 6).min(s.len())].iter().collect();
                // (Only a low surrogate completes it: `\ud803\ud83a` is two lone halves.)
                if let Ok(lo @ 0xdc00..=0xdfff) = u32::from_str_radix(&lo, 16) {
                    code = 0x10000 + ((code - 0xd800) << 10) + (lo - 0xdc00);
                    *i += 6;
                }
            }
            v.push(char::from_u32(code).unwrap_or('\u{fffd}'));
        }
        '\n' => *line += 1, // a line continuation
        other => v.push(other),
    }
}

// ------------------------------------------------------------------ syntax tree

pub type P<T> = Box<T>;

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    BigInt(Rc<str>),
    Str(Rc<str>),
    Template(Vec<String>, Vec<Expr>),
    Bool(bool),
    Null,
    Undefined,
    Ident(Rc<str>),
    This,
    /// `new.target`
    NewTarget,
    Array(Vec<Expr>),
    /// Properties: (key, value), with computed keys and spreads.
    Object(Vec<Prop>),
    Func(Rc<Func>),
    Unary(&'static str, P<Expr>),
    /// `++x` / `x--`: (op, prefix, target)
    Update(&'static str, bool, P<Expr>),
    Binary(&'static str, P<Expr>, P<Expr>),
    Logical(&'static str, P<Expr>, P<Expr>),
    Assign(&'static str, P<Expr>, P<Expr>),
    Cond(P<Expr>, P<Expr>, P<Expr>),
    /// obj.name or obj[expr]; `optional` for `?.`
    Member(P<Expr>, P<Expr>, bool),
    Call(P<Expr>, Vec<Expr>, bool),
    New(P<Expr>, Vec<Expr>),
    Seq(Vec<Expr>),
    Spread(P<Expr>),
    /// Destructuring patterns (as assignment targets and parameters).
    ArrayPat(Vec<Option<Expr>>),
    ObjectPat(Vec<(Expr, Expr)>),
    /// /pattern/flags
    Regex(Rc<str>, Rc<str>),
    /// `yield x` (delegate: `yield* x`)
    Yield(Option<P<Expr>>, bool),
    Await(P<Expr>),
    /// `import(specifier)`
    Import(P<Expr>),
    /// `import.meta`
    ImportMeta,
    /// `target = default` inside a pattern or parameter list.
    Default(P<Expr>, P<Expr>),
    Rest(P<Expr>),
}

#[derive(Debug, Clone)]
pub enum Prop {
    KeyValue(Expr, Expr),
    Spread(Expr),
    Getter(Expr, Rc<Func>),
    Setter(Expr, Rc<Func>),
}

/// A class body member.
#[derive(Debug, Clone)]
pub enum Member {
    Method(Rc<Func>),
    Get(Rc<Func>),
    Set(Rc<Func>),
    /// `name = value;` (or just `name;`)
    Field(Option<Expr>),
    /// `static { ... }`
    StaticBlock(Vec<Stmt>),
}

#[derive(Debug)]
pub struct Func {
    pub name: Option<Rc<str>>,
    pub params: Vec<Expr>,
    pub body: Vec<Stmt>,
    /// Arrow functions keep `this` and `arguments` from where they're written.
    pub arrow: bool,
    /// An arrow with an expression body: `body` is one `Return`.
    pub expr_body: bool,
    pub is_async: bool,
    /// `function*`
    pub is_gen: bool,
    /// Where it starts in the source, for error messages (see [`Where::packed`]).
    pub line: u32,
    /// Its source text, for `toString`: the script and a character range in it.
    pub src: Option<SrcSpan>,
}

/// A character range of a script's source.
#[derive(Debug, Clone)]
pub struct SrcSpan(pub Rc<str>, pub u32, pub u32);

impl SrcSpan {
    pub fn text(&self) -> String {
        self.0.chars().skip(self.1 as usize).take(self.2.saturating_sub(self.1) as usize).collect()
    }
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    /// (kind, [(pattern, initialiser)])
    Var(&'static str, Vec<(Expr, Option<Expr>)>),
    Func(Rc<Func>),
    Return(Option<Expr>),
    If(Expr, P<Stmt>, Option<P<Stmt>>),
    Block(Vec<Stmt>),
    For(Option<P<Stmt>>, Option<Expr>, Option<Expr>, P<Stmt>),
    /// for (decl in/of expr) body: (kind, pattern, `of`, iterable, body)
    ForIn(&'static str, Expr, bool, Expr, P<Stmt>),
    While(Expr, P<Stmt>),
    DoWhile(P<Stmt>, Expr),
    Break(Option<Rc<str>>),
    Continue(Option<Rc<str>>),
    Throw(Expr),
    /// try, catch (param, body), finally
    Try(Vec<Stmt>, Option<(Option<Expr>, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(Rc<str>, P<Stmt>),
    /// `with (object) body`
    With(Expr, P<Stmt>),
    /// (name, extends, [(key, member, static)])
    Class(Rc<str>, Option<Expr>, Vec<(Expr, Member, bool)>),
    /// `import ... from "specifier"`: (specifier, [(imported, local)]);
    /// imported is a name, "default", or "*" for the whole namespace.
    Import(Rc<str>, Vec<(Rc<str>, Rc<str>)>),
    Export(P<Export>),
    Empty,
}

/// An `export` declaration in a module. Pairs are (local, exported).
#[derive(Debug, Clone)]
pub enum Export {
    /// `export const a = 1`, `export function f() {}`, `export default function f() {}`
    Decl(Stmt, Vec<(Rc<str>, Rc<str>)>),
    /// `export default expression` (held in the local "default")
    Default(Expr),
    /// `export { a, b as c }`
    Names(Vec<(Rc<str>, Rc<str>)>),
    /// `export { a as b } from "m"` (an imported "*" re-exports the namespace)
    From(Rc<str>, Vec<(Rc<str>, Rc<str>)>),
    /// `export * from "m"`
    All(Rc<str>),
}

// ------------------------------------------------------------------ parser

pub struct Parser {
    toks: Vec<Token>,
    /// The whole script, which functions keep a range of for `toString`.
    source: Option<Rc<str>>,
    i: usize,
    /// Nesting depth, to stop pathological input before it overflows the stack.
    depth: u32,
    /// The next function parsed is `async` / a generator.
    next_async: bool,
    next_gen: bool,
    /// Inside a generator body (`yield` is a keyword).
    in_gen: bool,
    /// Parsing a module (`import` and `export` declarations are allowed).
    module: bool,
}

type R<T> = Result<T, String>;

/// A place in the source, for error messages: "3", or "1, char 52011" in a long line.
#[derive(Clone, Copy)]
pub struct Where(pub u32, pub u32);

impl Where {
    /// One number for a place: the line, or in a minified script (a few very
    /// long lines) the character in the file, marked by the top bit.
    pub fn packed(&self) -> u32 {
        if self.1 > 2000 {
            0x8000_0000 | self.1.min(0x7fff_ffff)
        } else {
            self.0
        }
    }

    /// A packed place, for people: "line 12" or "char 120034".
    pub fn unpack(p: u32) -> String {
        if p & 0x8000_0000 != 0 {
            format!("char {}", p & 0x7fff_ffff)
        } else {
            format!("line {p}")
        }
    }
}

impl std::fmt::Display for Where {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Minified scripts are a few very long lines: give the place in the file too.
        if self.1 > 2000 {
            write!(f, "{}, char {}", self.0, self.1)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

fn binary_prec(op: &str) -> Option<u8> {
    Some(match op {
        "??" => 1,
        "||" => 2,
        "&&" => 3,
        "|" => 4,
        "^" => 5,
        "&" => 6,
        "==" | "!=" | "===" | "!==" => 7,
        "<" | ">" | "<=" | ">=" | "instanceof" | "in" => 8,
        "<<" | ">>" | ">>>" => 9,
        "+" | "-" => 10,
        "*" | "/" | "%" => 11,
        "**" => 12,
        _ => return None,
    })
}

fn intern(op: &str) -> &'static str {
    PUNCTS.iter().find(|p| **p == op).copied().unwrap_or(match op {
        "instanceof" => "instanceof",
        "in" => "in",
        "typeof" => "typeof",
        "void" => "void",
        "delete" => "delete",
        _ => "?",
    })
}

const RESERVED: &[&str] = &[
    "var", "let", "const", "function", "return", "if", "else", "for", "while", "do", "break", "continue", "throw", "try", "catch", "finally", "switch",
    "case", "default", "new", "delete", "typeof", "void", "in", "instanceof", "this", "null", "true", "false", "class", "extends",
    "import", "export", "with",
];

impl Parser {
    pub fn new(src: &str) -> R<Self> {
        let mut p = Self::from_tokens(lex(src)?);
        p.source = Some(src.into());
        Ok(p)
    }

    fn from_tokens(toks: Vec<Token>) -> Self {
        Parser { toks, source: None, i: 0, depth: 0, next_async: false, next_gen: false, in_gen: false, module: false }
    }

    /// The source from token `from` to the last token read.
    fn span(&self, from: usize) -> Option<SrcSpan> {
        let src = self.source.clone()?;
        let start = self.toks.get(from)?.at;
        let end = self.toks.get(self.i.checked_sub(1)?)?.end;
        Some(SrcSpan(src, start, end.max(start)))
    }

    /// Where a function's text starts, walking back from its `(` over its
    /// name, `*`, `function`, `async`, `get` / `set`.
    fn func_start(&self, paren: usize) -> usize {
        let mut j = paren;
        let tok = |j: usize| &self.toks[j].tok;
        if j > 0 && matches!(tok(j - 1), Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) | Tok::BigInt(_)) && !matches!(tok(j - 1), Tok::Ident(w) if w == "function") {
            j -= 1;
        } else if j > 0 && matches!(tok(j - 1), Tok::Punct("]")) {
            // A computed key: back to its `[`.
            let mut depth = 0;
            while j > 0 {
                j -= 1;
                match tok(j) {
                    Tok::Punct("]") => depth += 1,
                    Tok::Punct("[") => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if j > 0 && matches!(tok(j - 1), Tok::Punct("*")) {
            j -= 1;
        }
        if j > 0 && matches!(tok(j - 1), Tok::Ident(w) if w == "function") {
            j -= 1;
        }
        if j > 0 && matches!(tok(j - 1), Tok::Ident(w) if w == "async" || w == "get" || w == "set") && j < paren {
            j -= 1;
        }
        j
    }

    /// The expression inside a template's `${...}`.
    fn sub_expr(&self, toks: Vec<Token>) -> R<Expr> {
        let mut p = Self::from_tokens(toks);
        p.source = self.source.clone();
        p.in_gen = self.in_gen;
        p.depth = self.depth;
        let e = p.expr()?;
        if !matches!(p.peek(), Tok::Eof) {
            return Err(format!("line {}: unexpected {} in a template", p.line(), describe(p.peek())));
        }
        Ok(e)
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }
    fn peek_at(&self, k: usize) -> &Tok {
        &self.toks[(self.i + k).min(self.toks.len() - 1)].tok
    }
    fn nl_before(&self) -> bool {
        self.toks[self.i].nl
    }
    fn line(&self) -> Where {
        let t = &self.toks[self.i];
        Where(t.line, t.at)
    }
    fn next(&mut self) -> Tok {
        let t = self.toks[self.i].tok.clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(x) if *x == p)
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if x == w)
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.next();
            true
        } else {
            false
        }
    }
    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(format!("line {}: expected '{p}', found {}", self.line(), describe(self.peek())))
        }
    }
    /// A statement ends: `;`, or (automatically) a line break, `}` or the end.
    fn semi(&mut self) -> R<()> {
        if self.eat(";") || self.is("}") || matches!(self.peek(), Tok::Eof) || self.nl_before() {
            Ok(())
        } else {
            Err(format!("line {}: expected ';', found {}", self.line(), describe(self.peek())))
        }
    }
    fn ident(&mut self) -> R<Rc<str>> {
        match self.next() {
            Tok::Ident(n) if !RESERVED.contains(&n.as_str()) => Ok(n.into()),
            t => Err(format!("line {}: expected a name, found {}", self.line(), describe(&t))),
        }
    }
    /// A property name after `.`: any identifier, reserved words included.
    fn prop_name(&mut self) -> R<Rc<str>> {
        match self.next() {
            Tok::Ident(n) => Ok(n.into()),
            t => Err(format!("line {}: expected a property name, found {}", self.line(), describe(&t))),
        }
    }

    pub fn program(&mut self) -> R<Vec<Stmt>> {
        let mut out = Vec::new();
        while !matches!(self.peek(), Tok::Eof) {
            out.push(self.stmt()?);
        }
        Ok(out)
    }

    fn block(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        let mut out = Vec::new();
        while !self.is("}") {
            if matches!(self.peek(), Tok::Eof) {
                return Err("unexpected end of script: missing '}'".into());
            }
            out.push(self.stmt()?);
        }
        self.next();
        Ok(out)
    }

    fn stmt(&mut self) -> R<Stmt> {
        self.depth += 1;
        if self.depth > 400 {
            return Err("the script nests too deeply".into());
        }
        let r = self.stmt_inner();
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self) -> R<Stmt> {
        if self.is("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        let word = match self.peek() {
            Tok::Ident(w) => w.clone(),
            _ => String::new(),
        };
        // A label: `name: statement`.
        if !word.is_empty() && !RESERVED.contains(&word.as_str()) && matches!(self.peek_at(1), Tok::Punct(":")) {
            self.next();
            self.next();
            return Ok(Stmt::Labeled(word.into(), Box::new(self.stmt()?)));
        }
        match word.as_str() {
            "var" | "const" => {
                let s = self.var_decl()?;
                self.semi()?;
                Ok(s)
            }
            "let" if matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) => {
                let s = self.var_decl()?;
                self.semi()?;
                Ok(s)
            }
            "function" => {
                self.next();
                self.next_gen = self.eat("*");
                let name = self.ident()?;
                Ok(Stmt::Func(self.func_rest(Some(name))?))
            }
            "async" if matches!(self.peek_at(1), Tok::Ident(w) if w == "function") && !self.toks[self.i + 1].nl => {
                self.next();
                self.next();
                self.next_gen = self.eat("*");
                self.next_async = true;
                let name = self.ident()?;
                Ok(Stmt::Func(self.func_rest(Some(name))?))
            }
            "class" => {
                self.next();
                let name = self.ident()?;
                let (sup, members) = self.class_rest()?;
                Ok(Stmt::Class(name, sup, members))
            }
            "import" if !matches!(self.peek_at(1), Tok::Punct("(" | ".")) => {
                if !self.module || self.depth > 1 {
                    return Err(format!("line {}: import declarations may only appear at the top of a module", self.line()));
                }
                self.next();
                self.import_decl()
            }
            "export" => {
                if !self.module || self.depth > 1 {
                    return Err(format!("line {}: export declarations may only appear at the top of a module", self.line()));
                }
                self.next();
                self.export_decl()
            }
            "return" => {
                self.next();
                let v = if self.is(";") || self.is("}") || self.nl_before() || matches!(self.peek(), Tok::Eof) { None } else { Some(self.expr()?) };
                self.semi()?;
                Ok(Stmt::Return(v))
            }
            "if" => {
                self.next();
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                let t = self.stmt()?;
                let e = if self.eat_word("else") { Some(Box::new(self.stmt()?)) } else { None };
                Ok(Stmt::If(c, Box::new(t), e))
            }
            "with" => {
                self.next();
                self.expect("(")?;
                let o = self.expr()?;
                self.expect(")")?;
                Ok(Stmt::With(o, Box::new(self.stmt()?)))
            }
            "while" => {
                self.next();
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                Ok(Stmt::While(c, Box::new(self.stmt()?)))
            }
            "do" => {
                self.next();
                let body = self.stmt()?;
                if !self.eat_word("while") {
                    return Err(format!("line {}: expected 'while'", self.line()));
                }
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                self.eat(";");
                Ok(Stmt::DoWhile(Box::new(body), c))
            }
            "for" => self.for_stmt(),
            "break" | "continue" => {
                self.next();
                let label = match self.peek() {
                    Tok::Ident(l) if !self.nl_before() && !RESERVED.contains(&l.as_str()) => {
                        let l: Rc<str> = l.as_str().into();
                        self.next();
                        Some(l)
                    }
                    _ => None,
                };
                self.semi()?;
                Ok(if word == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
            }
            "throw" => {
                self.next();
                let v = self.expr()?;
                self.semi()?;
                Ok(Stmt::Throw(v))
            }
            "try" => {
                self.next();
                let body = self.block()?;
                let catch = if self.eat_word("catch") {
                    let param = if self.eat("(") {
                        let p = self.binding_target()?;
                        self.expect(")")?;
                        Some(p)
                    } else {
                        None
                    };
                    Some((param, self.block()?))
                } else {
                    None
                };
                let finally = if self.eat_word("finally") { Some(self.block()?) } else { None };
                if catch.is_none() && finally.is_none() {
                    return Err(format!("line {}: try needs catch or finally", self.line()));
                }
                Ok(Stmt::Try(body, catch, finally))
            }
            "switch" => {
                self.next();
                self.expect("(")?;
                let d = self.expr()?;
                self.expect(")")?;
                self.expect("{")?;
                let mut cases = Vec::new();
                while !self.eat("}") {
                    let test = if self.eat_word("case") {
                        let t = self.expr()?;
                        Some(t)
                    } else if self.eat_word("default") {
                        None
                    } else {
                        return Err(format!("line {}: expected case or default", self.line()));
                    };
                    self.expect(":")?;
                    let mut body = Vec::new();
                    while !self.is("}") && !self.is_word("case") && !self.is_word("default") {
                        body.push(self.stmt()?);
                    }
                    cases.push((test, body));
                }
                Ok(Stmt::Switch(d, cases))
            }
            _ => {
                let e = self.expr()?;
                self.semi()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn specifier(&mut self) -> R<Rc<str>> {
        match self.next() {
            Tok::Str(s) => Ok(s.into()),
            t => Err(format!("line {}: expected a module specifier, found {}", self.line(), describe(&t))),
        }
    }

    /// A name in an import or export list (strings are allowed: `"a-b" as c`).
    fn module_name(&mut self) -> R<Rc<str>> {
        match self.next() {
            Tok::Ident(n) => Ok(n.into()),
            Tok::Str(s) => Ok(s.into()),
            t => Err(format!("line {}: expected a name, found {}", self.line(), describe(&t))),
        }
    }

    /// `{ a, b as c }` in an import or export: (name, alias) pairs.
    fn name_list(&mut self) -> R<Vec<(Rc<str>, Rc<str>)>> {
        self.expect("{")?;
        let mut out = Vec::new();
        while !self.eat("}") {
            let n = self.module_name()?;
            let alias = if self.eat_word("as") { self.module_name()? } else { n.clone() };
            out.push((n, alias));
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(out)
    }

    /// After `import`: the bindings and the specifier.
    fn import_decl(&mut self) -> R<Stmt> {
        if let Tok::Str(_) = self.peek() {
            let spec = self.specifier()?;
            self.semi()?;
            return Ok(Stmt::Import(spec, Vec::new()));
        }
        let mut names = Vec::new();
        if let Tok::Ident(n) = self.peek().clone() {
            if n != "from" || matches!(self.peek_at(1), Tok::Ident(w) if w == "from") {
                self.next();
                names.push(("default".into(), n.into()));
                self.eat(",");
            }
        }
        if self.eat("*") {
            if !self.eat_word("as") {
                return Err(format!("line {}: expected 'as'", self.line()));
            }
            names.push(("*".into(), self.ident()?));
        } else if self.is("{") {
            names.extend(self.name_list()?);
        }
        if !self.eat_word("from") {
            return Err(format!("line {}: expected 'from'", self.line()));
        }
        let spec = self.specifier()?;
        // Import attributes (`with { type: "json" }`) are read and ignored.
        if (self.is_word("with") || self.is_word("assert")) && !self.nl_before() {
            self.next();
            self.object()?;
        }
        self.semi()?;
        Ok(Stmt::Import(spec, names))
    }

    /// After `export`.
    fn export_decl(&mut self) -> R<Stmt> {
        let e = if self.eat_word("default") {
            let is_fn = self.is_word("function") || (self.is_word("async") && matches!(self.peek_at(1), Tok::Ident(w) if w == "function"));
            if is_fn {
                if self.eat_word("async") {
                    self.next_async = true;
                }
                self.next();
                self.next_gen = self.eat("*");
                let name: Rc<str> = match self.peek().clone() {
                    Tok::Ident(n) if !self.is("(") => {
                        self.next();
                        n.into()
                    }
                    _ => "default".into(),
                };
                let f = self.func_rest(Some(name.clone()))?;
                Export::Decl(Stmt::Func(f), vec![(name, "default".into())])
            } else if self.is_word("class") {
                self.next();
                let name: Rc<str> = match self.peek().clone() {
                    Tok::Ident(n) if n != "extends" => {
                        self.next();
                        n.into()
                    }
                    _ => "default".into(),
                };
                let (sup, members) = self.class_rest()?;
                Export::Decl(Stmt::Class(name.clone(), sup, members), vec![(name, "default".into())])
            } else {
                let v = self.assign()?;
                self.semi()?;
                Export::Default(v)
            }
        } else if self.eat("*") {
            let alias = if self.eat_word("as") { Some(self.module_name()?) } else { None };
            if !self.eat_word("from") {
                return Err(format!("line {}: expected 'from'", self.line()));
            }
            let spec = self.specifier()?;
            self.semi()?;
            match alias {
                Some(a) => Export::From(spec, vec![("*".into(), a)]),
                None => Export::All(spec),
            }
        } else if self.is("{") {
            let names = self.name_list()?;
            if self.eat_word("from") {
                let spec = self.specifier()?;
                self.semi()?;
                Export::From(spec, names)
            } else {
                self.semi()?;
                Export::Names(names)
            }
        } else {
            let s = self.stmt()?;
            let mut names = Vec::new();
            match &s {
                Stmt::Var(_, decls) => {
                    for (p, _) in decls {
                        let mut ns = Vec::new();
                        pattern_names(p, &mut ns);
                        names.extend(ns.into_iter().map(|n| (n.clone(), n)));
                    }
                }
                Stmt::Func(f) => names.extend(f.name.clone().map(|n| (n.clone(), n))),
                Stmt::Class(n, ..) => names.push((n.clone(), n.clone())),
                _ => return Err(format!("line {}: unexpected export", self.line())),
            }
            Export::Decl(s, names)
        };
        Ok(Stmt::Export(Box::new(e)))
    }

    fn var_decl(&mut self) -> R<Stmt> {
        let kind = match self.next() {
            Tok::Ident(k) if k == "var" => "var",
            Tok::Ident(k) if k == "let" => "let",
            _ => "const",
        };
        let mut decls = Vec::new();
        loop {
            let target = self.binding_target()?;
            let init = if self.eat("=") { Some(self.assign()?) } else { None };
            decls.push((target, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(Stmt::Var(kind, decls))
    }

    /// A name or a destructuring pattern.
    fn binding_target(&mut self) -> R<Expr> {
        if self.is("[") || self.is("{") {
            let e = self.primary()?;
            to_pattern(e).ok_or_else(|| format!("line {}: bad destructuring pattern", self.line()))
        } else {
            Ok(Expr::Ident(self.ident()?))
        }
    }

    fn for_stmt(&mut self) -> R<Stmt> {
        self.next();
        let is_await = self.eat_word("await");
        // `for await (… of x)`: the iterable is marked, for the compiler.
        let mark = move |of: bool, iter: Expr| if of && is_await { Expr::Unary("for await", Box::new(iter)) } else { iter };
        self.expect("(")?;
        let mut init = None;
        if !self.is(";") {
            let is_decl = self.is_word("var") || self.is_word("const") || (self.is_word("let") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")));
            if is_decl {
                let kind = match self.next() {
                    Tok::Ident(k) if k == "var" => "var",
                    Tok::Ident(k) if k == "let" => "let",
                    _ => "const",
                };
                let target = self.binding_target()?;
                if self.is_word("of") || self.is_word("in") {
                    let of = self.is_word("of");
                    self.next();
                    let iter = if of { self.assign()? } else { self.expr()? };
                    self.expect(")")?;
                    return Ok(Stmt::ForIn(kind, target, of, mark(of, iter), Box::new(self.stmt()?)));
                }
                let mut decls = vec![(target, if self.eat("=") { Some(self.assign_no_in()?) } else { None })];
                while self.eat(",") {
                    let t = self.binding_target()?;
                    let i = if self.eat("=") { Some(self.assign_no_in()?) } else { None };
                    decls.push((t, i));
                }
                init = Some(Box::new(Stmt::Var(kind, decls)));
            } else if self.is_word("yield") {
                // `for (yield x; …)` in a generator.
                init = Some(Box::new(Stmt::Expr(self.expr()?)));
            } else {
                let e = self.unary_and_postfix()?;
                if self.is_word("of") || self.is_word("in") {
                    let of = self.is_word("of");
                    self.next();
                    let iter = self.expr()?;
                    self.expect(")")?;
                    let target = to_pattern(e).ok_or("bad for-in target")?;
                    return Ok(Stmt::ForIn("", target, of, mark(of, iter), Box::new(self.stmt()?)));
                }
                // Not a for-in after all: finish the expression from what was read.
                let e = self.continue_expr(e)?;
                init = Some(Box::new(Stmt::Expr(e)));
            }
        }
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expr()?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expr()?) };
        self.expect(")")?;
        Ok(Stmt::For(init, test, update, Box::new(self.stmt()?)))
    }

    fn func_rest(&mut self, name: Option<Rc<str>>) -> R<Rc<Func>> {
        let (is_async, is_gen) = (std::mem::take(&mut self.next_async), std::mem::take(&mut self.next_gen));
        let outer = std::mem::replace(&mut self.in_gen, is_gen);
        let line = self.line().packed();
        let from = self.func_start(self.i);
        let r = (|| {
            self.expect("(")?;
            let params = self.params()?;
            let body = self.block()?;
            let src = self.span(from);
            Ok(Rc::new(Func { name, params, body, arrow: false, expr_body: false, is_async, is_gen, line, src }))
        })();
        self.in_gen = outer;
        r
    }

    fn params(&mut self) -> R<Vec<Expr>> {
        let mut params = Vec::new();
        while !self.eat(")") {
            if self.eat("...") {
                params.push(Expr::Rest(Box::new(self.binding_target()?)));
            } else {
                let t = self.binding_target()?;
                params.push(if self.eat("=") { Expr::Default(Box::new(t), Box::new(self.assign()?)) } else { t });
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok(params)
    }

    fn class_rest(&mut self) -> R<(Option<Expr>, Vec<(Expr, Member, bool)>)> {
        let mut from = self.i;
        if from > 0 && matches!(&self.toks[from - 1].tok, Tok::Ident(w) if w != "class") {
            from -= 1;
        }
        if from > 0 && matches!(&self.toks[from - 1].tok, Tok::Ident(w) if w == "class") {
            from -= 1;
        }
        let sup = if self.eat_word("extends") { Some(self.unary_and_postfix()?) } else { None };
        self.expect("{")?;
        let mut members = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let is_static = self.is_word("static") && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"));
            if is_static {
                self.next();
                if self.is("{") {
                    // A static initialisation block.
                    let body = self.block()?;
                    members.push((Expr::Str("".into()), Member::StaticBlock(body), true));
                    continue;
                }
            }
            let mut accessor = None;
            for w in ["get", "set"] {
                if self.is_word(w) && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}")) {
                    self.next();
                    accessor = Some(w);
                }
            }
            let mut is_async = false;
            if !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}")) {
                is_async = self.eat_word("async");
            }
            let is_gen = self.eat("*");
            let key = if self.eat("[") {
                let k = self.assign()?;
                self.expect("]")?;
                k
            } else {
                match self.next() {
                    Tok::Ident(n) => Expr::Str(n.into()),
                    Tok::Str(s) => Expr::Str(s.into()),
                    Tok::Num(n) => Expr::Str(crate::interp::num_to_string(n).into()),
                    Tok::BigInt(n) => Expr::Str(n.into()),
                    t => return Err(format!("line {}: unexpected {} in class", self.line(), describe(&t))),
                }
            };
            if self.is("(") {
                let name = if let Expr::Str(n) = &key { Some(n.clone()) } else { None };
                self.next_async = is_async;
                self.next_gen = is_gen;
                let f = self.func_rest(name)?;
                let m = match accessor {
                    Some("get") => Member::Get(f),
                    Some(_) => Member::Set(f),
                    None => Member::Method(f),
                };
                members.push((key, m, is_static));
            } else {
                let init = if self.eat("=") { Some(self.assign()?) } else { None };
                self.semi()?;
                members.push((key, Member::Field(init), is_static));
            }
        }
        // A class's text is the whole class, kept on its constructor (made
        // here when the class has none).
        let span = self.span(from);
        let has_ctor = members.iter().any(|(k, m, st)| !st && matches!(m, Member::Method(_)) && matches!(k, Expr::Str(n) if &**n == "constructor"));
        if !has_ctor {
            let body = if sup.is_some() {
                vec![Stmt::Expr(Expr::Call(Box::new(Expr::Ident("super".into())), vec![Expr::Spread(Box::new(Expr::Ident("arguments".into())))], false))]
            } else {
                Vec::new()
            };
            let line = self.line().packed();
            let f = Func { name: None, params: Vec::new(), body, arrow: false, expr_body: false, is_async: false, is_gen: false, line, src: None };
            members.push((Expr::Str("constructor".into()), Member::Method(Rc::new(f)), false));
        }
        for (k, m, st) in members.iter_mut() {
            if let (Expr::Str(n), Member::Method(f), false) = (&*k, m, *st) {
                if &**n == "constructor" {
                    if let Some(f) = Rc::get_mut(f) {
                        f.src = span.clone();
                    }
                }
            }
        }
        Ok((sup, members))
    }

    // ---------------------------------------------------------- expressions

    pub fn expr(&mut self) -> R<Expr> {
        let first = self.assign()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = vec![first];
        while self.eat(",") {
            list.push(self.assign()?);
        }
        Ok(Expr::Seq(list))
    }

    fn assign_no_in(&mut self) -> R<Expr> {
        self.assign()
    }

    fn continue_expr(&mut self, left: Expr) -> R<Expr> {
        let assigns = ["=", "+=", "-=", "*=", "/=", "%=", "**=", "<<=", ">>=", ">>>=", "&=", "|=", "^=", "&&=", "||=", "??="].iter().any(|o| self.is(o));
        let e = if assigns {
            self.assign_rest(left)?
        } else {
            let b = self.binary_from(left, 0)?;
            self.conditional_rest(b)?
        };
        if self.is(",") {
            let mut list = vec![e];
            while self.eat(",") {
                list.push(self.assign()?);
            }
            return Ok(Expr::Seq(list));
        }
        Ok(e)
    }

    fn is_arrow_ahead(&self) -> bool {
        // `(` ... `)` `=>` with balanced brackets.
        if !self.is("(") {
            return false;
        }
        let mut depth = 0;
        let mut k = self.i;
        while k < self.toks.len() {
            match &self.toks[k].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.toks.get(k + 1).map(|t| &t.tok), Some(Tok::Punct("=>")));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            k += 1;
        }
        false
    }

    fn assign(&mut self) -> R<Expr> {
        self.depth += 1;
        if self.depth > 400 {
            return Err("the script nests too deeply".into());
        }
        let r = self.assign_inner();
        self.depth -= 1;
        r
    }

    fn assign_inner(&mut self) -> R<Expr> {
        // Arrow functions: `x => ...`, `(a, b) => ...`, `async x => ...` (no event loop yet: async is ignored).
        let mut is_async = false;
        if self.is_word("async") && !self.toks[self.i + 1].nl {
            let save = self.i;
            self.next();
            let arrow = (matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Punct("=>"))) || self.is_arrow_ahead();
            if arrow {
                is_async = true;
            } else {
                self.i = save;
            }
        }
        if self.in_gen && self.is_word("yield") {
            self.next();
            let delegate = self.eat("*");
            let ends = self.nl_before() || matches!(self.peek(), Tok::Punct(")" | "]" | "}" | "," | ";" | ":") | Tok::Eof);
            let arg = if ends && !delegate { None } else { Some(Box::new(self.assign()?)) };
            return Ok(Expr::Yield(arg, delegate));
        }
        self.next_async = is_async;
        if let Tok::Ident(n) = self.peek().clone() {
            if matches!(self.peek_at(1), Tok::Punct("=>")) && !RESERVED.contains(&n.as_str()) {
                self.next();
                self.next();
                return self.arrow_body(vec![Expr::Ident(n.into())]);
            }
        }
        if self.is_arrow_ahead() {
            self.next();
            let params = self.params()?;
            self.expect("=>")?;
            return self.arrow_body(params);
        }
        self.next_async = false;
        let left = self.conditional()?;
        self.assign_rest(left)
    }

    fn assign_rest(&mut self, left: Expr) -> R<Expr> {
        let ops = ["=", "+=", "-=", "*=", "/=", "%=", "**=", "<<=", ">>=", ">>>=", "&=", "|=", "^=", "&&=", "||=", "??="];
        let Some(op) = ops.iter().find(|o| self.is(o)).copied() else {
            // A conditional or binary expression may continue after a parsed prefix.
            return Ok(left);
        };
        self.next();
        let target = if op == "=" { to_pattern(left).ok_or(format!("line {}: can't assign to that", self.line()))? } else { left };
        if !matches!(target, Expr::Ident(_) | Expr::Member(..) | Expr::ArrayPat(_) | Expr::ObjectPat(_)) {
            return Err(format!("line {}: can't assign to that", self.line()));
        }
        Ok(Expr::Assign(intern(op), Box::new(target), Box::new(self.assign()?)))
    }

    /// Where an arrow function starts: walking back from its `=>` over the
    /// parameters (one name, or a bracketed list) and `async`.
    fn arrow_start(&self) -> Option<usize> {
        let arrow = (0..=self.i.min(self.toks.len() - 1)).rev().take(2).find(|&j| matches!(self.toks[j].tok, Tok::Punct("=>")))?;
        let mut j = arrow.checked_sub(1)?;
        if matches!(self.toks[j].tok, Tok::Punct(")")) {
            let mut depth = 0;
            loop {
                match self.toks[j].tok {
                    Tok::Punct(")") => depth += 1,
                    Tok::Punct("(") => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j = j.checked_sub(1)?;
            }
        }
        if j > 0 && matches!(&self.toks[j - 1].tok, Tok::Ident(w) if w == "async") {
            j -= 1;
        }
        Some(j)
    }

    fn arrow_body(&mut self, params: Vec<Expr>) -> R<Expr> {
        let is_async = std::mem::take(&mut self.next_async);
        let outer = std::mem::replace(&mut self.in_gen, false);
        let line = self.line().packed();
        let from = self.arrow_start();
        let r = (|| {
            if self.is("{") {
                let body = self.block()?;
                let src = from.and_then(|f| self.span(f));
                Ok(Expr::Func(Rc::new(Func { name: None, params, body, arrow: true, expr_body: false, is_async, is_gen: false, line, src })))
            } else {
                let e = self.assign()?;
                let src = from.and_then(|f| self.span(f));
                Ok(Expr::Func(Rc::new(Func { name: None, params, body: vec![Stmt::Return(Some(e))], arrow: true, expr_body: true, is_async, is_gen: false, line, src })))
            }
        })();
        self.in_gen = outer;
        r
    }

    fn conditional(&mut self) -> R<Expr> {
        let c = self.binary(0)?;
        self.conditional_rest(c)
    }

    fn conditional_rest(&mut self, c: Expr) -> R<Expr> {
        if self.eat("?") {
            let a = self.assign()?;
            self.expect(":")?;
            let b = self.assign()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let left = self.unary_and_postfix()?;
        self.binary_from(left, min)
    }

    fn binary_from(&mut self, mut left: Expr, min: u8) -> R<Expr> {
        loop {
            let op = match self.peek() {
                Tok::Punct(p) => *p,
                Tok::Ident(w) if w == "instanceof" => "instanceof",
                Tok::Ident(w) if w == "in" => "in",
                _ => break,
            };
            let Some(prec) = binary_prec(op) else { break };
            if prec <= min && !(op == "**" && prec == min) {
                break;
            }
            self.next();
            // `**` is right-associative.
            let right = if op == "**" { self.binary(prec - 1)? } else { self.binary(prec)? };
            left = if matches!(op, "&&" | "||" | "??") { Expr::Logical(op, Box::new(left), Box::new(right)) } else { Expr::Binary(op, Box::new(left), Box::new(right)) };
        }
        Ok(left)
    }

    fn unary_and_postfix(&mut self) -> R<Expr> {
        let op = match self.peek() {
            Tok::Punct(p @ ("!" | "-" | "+" | "~")) => Some(*p),
            Tok::Ident(w) if w == "typeof" || w == "void" || w == "delete" => Some(intern(w)),
            Tok::Ident(w) if w == "await" && !matches!(self.peek_at(1), Tok::Punct(")" | "," | ";" | "=" | "." | "]" | "}" | ":")) => {
                self.next();
                let e = self.unary_and_postfix()?;
                return Ok(Expr::Await(Box::new(e)));
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let e = self.unary_and_postfix()?;
            return Ok(Expr::Unary(op, Box::new(e)));
        }
        if self.is("++") || self.is("--") {
            let op = if self.eat("++") { "++" } else {
                self.next();
                "--"
            };
            let e = self.unary_and_postfix()?;
            return Ok(Expr::Update(op, true, Box::new(e)));
        }
        let e = self.call_member()?;
        if (self.is("++") || self.is("--")) && !self.nl_before() {
            let op = if self.eat("++") { "++" } else {
                self.next();
                "--"
            };
            return Ok(Expr::Update(op, false, Box::new(e)));
        }
        Ok(e)
    }

    fn args(&mut self) -> R<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.eat(")") {
            if self.eat("...") {
                args.push(Expr::Spread(Box::new(self.assign()?)));
            } else {
                args.push(self.assign()?);
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok(args)
    }

    fn call_member(&mut self) -> R<Expr> {
        let mut e = if self.eat_word("new") {
            if self.eat(".") {
                self.prop_name()?; // new.target
                Expr::NewTarget
            } else {
                let callee = self.member_only()?;
                let args = if self.eat("(") { self.args()? } else { Vec::new() };
                Expr::New(Box::new(callee), args)
            }
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let n = self.prop_name()?;
                e = Expr::Member(Box::new(e), Box::new(Expr::Str(n)), false);
            } else if self.eat("?.") {
                if self.eat("(") {
                    e = Expr::Call(Box::new(e), self.args()?, true);
                } else if self.eat("[") {
                    let k = self.expr()?;
                    self.expect("]")?;
                    e = Expr::Member(Box::new(e), Box::new(k), true);
                } else {
                    let n = self.prop_name()?;
                    e = Expr::Member(Box::new(e), Box::new(Expr::Str(n)), true);
                }
            } else if self.eat("[") {
                let k = self.expr()?;
                self.expect("]")?;
                e = Expr::Member(Box::new(e), Box::new(k), false);
            } else if self.eat("(") {
                e = Expr::Call(Box::new(e), self.args()?, false);
            } else if let Tok::Template(strs, exprs, raws) = self.peek().clone() {
                // A tagged template: called with the pieces and the values.
                self.next();
                // The strings array carries `.raw`, the source text of each piece.
                let pieces = Expr::Array(strs.iter().map(|s| Expr::Str(s.as_str().into())).collect());
                let raw_pieces = Expr::Array(raws.iter().map(|s| Expr::Str(s.as_str().into())).collect());
                let raw = Expr::Object(vec![Prop::KeyValue(Expr::Str("raw".into()), raw_pieces)]);
                let assign = Expr::Member(Box::new(Expr::Ident("Object".into())), Box::new(Expr::Str("assign".into())), false);
                let parts = Expr::Call(Box::new(assign), vec![pieces, raw], false);
                let mut args = vec![parts];
                for src in exprs {
                    args.push(self.sub_expr(src)?);
                }
                e = Expr::Call(Box::new(e), args, false);
            } else {
                return Ok(e);
            }
        }
    }

    /// The callee of `new`: member accesses, but no calls.
    fn member_only(&mut self) -> R<Expr> {
        let mut e = if self.eat_word("new") {
            let callee = self.member_only()?;
            let args = if self.eat("(") { self.args()? } else { Vec::new() };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let n = self.prop_name()?;
                e = Expr::Member(Box::new(e), Box::new(Expr::Str(n)), false);
            } else if self.eat("[") {
                let k = self.expr()?;
                self.expect("]")?;
                e = Expr::Member(Box::new(e), Box::new(k), false);
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> R<Expr> {
        let line = self.line();
        match self.next() {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::BigInt(n) => Ok(Expr::BigInt(n.into())),
            Tok::Str(s) => Ok(Expr::Str(s.into())),
            Tok::Template(strs, exprs, _) => {
                let mut es = Vec::new();
                for src in exprs {
                    es.push(self.sub_expr(src)?);
                }
                Ok(Expr::Template(strs, es))
            }
            Tok::Punct("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                let mut items = Vec::new();
                while !self.eat("]") {
                    if self.is(",") {
                        self.next();
                        items.push(Expr::Undefined); // a hole
                        continue;
                    }
                    if self.eat("...") {
                        items.push(Expr::Spread(Box::new(self.assign()?)));
                    } else {
                        items.push(self.assign()?);
                    }
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                Ok(Expr::Array(items))
            }
            Tok::Punct("{") => self.object(),
            Tok::Regex(body, flags) => Ok(Expr::Regex(body.into(), flags.into())),
            Tok::Ident(w) => match w.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "null" => Ok(Expr::Null),
                "undefined" => Ok(Expr::Undefined),
                "this" => Ok(Expr::This),
                "import" => {
                    if self.eat(".") {
                        let w = self.prop_name()?;
                        if &*w != "meta" {
                            return Err(format!("line {line}: unexpected import.{w}"));
                        }
                        return Ok(Expr::ImportMeta);
                    }
                    self.expect("(")?;
                    let spec = self.assign()?;
                    // A second argument (import options) is read and ignored.
                    if self.eat(",") && !self.is(")") {
                        self.assign()?;
                        self.eat(",");
                    }
                    self.expect(")")?;
                    Ok(Expr::Import(Box::new(spec)))
                }
                "function" => {
                    self.next_gen = self.eat("*");
                    let name = if let Tok::Ident(n) = self.peek() {
                        if !self.is("(") {
                            let n: Rc<str> = n.as_str().into();
                            self.next();
                            Some(n)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    Ok(Expr::Func(self.func_rest(name)?))
                }
                "async" if self.is_word("function") => {
                    self.next();
                    self.next_async = true;
                    self.primary_after_function()
                }
                "class" => {
                    let name: Rc<str> = if let Tok::Ident(n) = self.peek().clone() {
                        if n != "extends" {
                            self.next();
                            n.into()
                        } else {
                            "".into()
                        }
                    } else {
                        "".into()
                    };
                    let (sup, members) = self.class_rest()?;
                    // A class expression: a function made by a one-statement program.
                    Ok(Expr::Call(
                        Box::new(Expr::Func(Rc::new(Func {
                            name: None,
                            params: vec![],
                            body: vec![Stmt::Class(if name.is_empty() { "_class".into() } else { name.clone() }, sup, members), Stmt::Return(Some(Expr::Ident(if name.is_empty() { "_class".into() } else { name })))],
                            arrow: true,
                            expr_body: false,
                            is_async: false,
                            is_gen: false,
                            line: 0,
                            src: None,
                        }))),
                        vec![],
                        false,
                    ))
                }
                w if RESERVED.contains(&w) => Err(format!("line {line}: unexpected '{w}'")),
                // `#x in o`, the brand check.
                w if w.starts_with("\0#") => Ok(Expr::Str(w.into())),
                _ => Ok(Expr::Ident(w.into())),
            },
            t => Err(format!("line {line}: unexpected {}", describe(&t))),
        }
    }

    fn primary_after_function(&mut self) -> R<Expr> {
        self.next_gen = self.eat("*");
        let name = match self.peek().clone() {
            Tok::Ident(n) => {
                self.next();
                Some(n.into())
            }
            _ => None,
        };
        Ok(Expr::Func(self.func_rest(name)?))
    }

    fn object(&mut self) -> R<Expr> {
        let mut props = Vec::new();
        while !self.eat("}") {
            if self.eat("...") {
                props.push(Prop::Spread(self.assign()?));
            } else {
                let mut get_set = None;
                if (self.is_word("get") || self.is_word("set")) && !matches!(self.peek_at(1), Tok::Punct(":" | "(" | "," | "}")) {
                    get_set = Some(self.is_word("get"));
                    self.next();
                }
                let mut is_async = false;
                if self.is_word("async") && !matches!(self.peek_at(1), Tok::Punct(":" | "(" | "," | "}")) {
                    self.next();
                    is_async = true;
                }
                let is_gen = self.eat("*");
                let (key, shorthand) = if self.eat("[") {
                    let k = self.assign()?;
                    self.expect("]")?;
                    (k, None)
                } else {
                    match self.next() {
                        Tok::Ident(n) => (Expr::Str(n.as_str().into()), Some(n)),
                        Tok::Str(s) => (Expr::Str(s.into()), None),
                        Tok::Num(n) => (Expr::Str(crate::interp::num_to_string(n).into()), None),
                        Tok::BigInt(n) => (Expr::Str(n.into()), None),
                        t => return Err(format!("line {}: unexpected {} in object", self.line(), describe(&t))),
                    }
                };
                if let Some(getter) = get_set {
                    let f = self.func_rest(None)?;
                    props.push(if getter { Prop::Getter(key, f) } else { Prop::Setter(key, f) });
                } else if self.is("(") {
                    self.next_async = is_async;
                    self.next_gen = is_gen;
                    // A method is named after its key (by the compiler), but doesn't see that name.
                    props.push(Prop::KeyValue(key, Expr::Func(self.func_rest(None)?)));
                } else if self.eat(":") {
                    props.push(Prop::KeyValue(key, self.assign()?));
                } else if let Some(n) = shorthand {
                    // `{ a }`, or `{ a = 1 }` in a pattern.
                    if self.eat("=") {
                        let d = self.assign()?;
                        props.push(Prop::KeyValue(key, Expr::Default(Box::new(Expr::Ident(n.into())), Box::new(d))));
                    } else {
                        props.push(Prop::KeyValue(key, Expr::Ident(n.into())));
                    }
                } else {
                    return Err(format!("line {}: expected ':' in object", self.line()));
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Object(props))
    }
}

/// Reads an array or object literal as a destructuring pattern.
fn to_pattern(e: Expr) -> Option<Expr> {
    Some(match e {
        Expr::Ident(_) | Expr::Member(..) | Expr::ArrayPat(_) | Expr::ObjectPat(_) => e,
        Expr::Array(items) => Expr::ArrayPat(
            items
                .into_iter()
                .map(|it| match it {
                    Expr::Undefined => Some(None),
                    Expr::Spread(t) => to_pattern(*t).map(|t| Some(Expr::Rest(Box::new(t)))),
                    other => to_pattern(other).map(Some),
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        Expr::Object(props) => Expr::ObjectPat(
            props
                .into_iter()
                .map(|p| match p {
                    Prop::KeyValue(k, v) => to_pattern(v).map(|v| (k, v)),
                    Prop::Spread(t) => to_pattern(t).map(|t| (Expr::Undefined, Expr::Rest(Box::new(t)))),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        Expr::Assign("=", t, d) => Expr::Default(Box::new(to_pattern(*t)?), d),
        Expr::Default(t, d) => Expr::Default(Box::new(to_pattern(*t)?), d),
        _ => return None,
    })
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Num(n) => format!("number {n}"),
        Tok::BigInt(n) => format!("BigInt {n}n"),
        Tok::Str(_) => "a string".into(),
        Tok::Template(..) => "a template".into(),
        Tok::Ident(w) => format!("'{w}'"),
        Tok::Punct(p) => format!("'{p}'"),
        Tok::Regex(..) => "a regular expression".into(),
        Tok::Eof => "the end of the script".into(),
    }
}

pub fn parse(src: &str) -> R<Vec<Stmt>> {
    Parser::new(src)?.program()
}

/// Parses a module: `import` and `export` declarations are allowed at its top.
pub fn parse_module(src: &str) -> R<Vec<Stmt>> {
    let mut p = Parser::new(src)?;
    p.module = true;
    p.program()
}

/// The names a pattern binds.
pub fn pattern_names(p: &Expr, out: &mut Vec<Rc<str>>) {
    match p {
        Expr::Ident(n) => out.push(n.clone()),
        Expr::Default(t, _) | Expr::Rest(t) => pattern_names(t, out),
        Expr::ArrayPat(items) => items.iter().flatten().for_each(|i| pattern_names(i, out)),
        Expr::ObjectPat(props) => props.iter().for_each(|(_, v)| pattern_names(v, out)),
        _ => {}
    }
}
