//! JavaScript source → syntax tree: a lexer (with the newline tracking that
//! automatic semicolon insertion needs) and a recursive-descent parser with
//! precedence climbing for expressions.

use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(String),
    /// A template literal: its string pieces and the source of each `${...}`.
    Template(Vec<String>, Vec<String>),
    Ident(String),
    Punct(&'static str),
    /// A regular expression literal: (pattern, flags)
    Regex(String, String),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    /// A line break came before it (for automatic semicolon insertion).
    pub nl: bool,
    pub line: u32,
}

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^",
    "!", "~", "?", ":", "=", ".",
];

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut nl = false;
    while i < s.len() {
        let c = s[i];
        if c == '\n' || c == '\u{2028}' || c == '\u{2029}' {
            nl = true;
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '/' && s.get(i + 1) == Some(&'/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && s.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < s.len() && !(s[i] == '*' && s[i + 1] == '/') {
                if s[i] == '\n' {
                    nl = true;
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        let start_nl = nl;
        nl = false;
        let tok_line = line;
        let push = |out: &mut Vec<Token>, tok: Tok| out.push(Token { tok, nl: start_nl, line: tok_line });
        if c.is_ascii_digit() || (c == '.' && s.get(i + 1).map(|d| d.is_ascii_digit()).unwrap_or(false)) {
            let start = i;
            if c == '0' && matches!(s.get(i + 1), Some('x' | 'X' | 'b' | 'B' | 'o' | 'O')) {
                let radix = match s[i + 1] {
                    'x' | 'X' => 16,
                    'b' | 'B' => 2,
                    _ => 8,
                };
                i += 2;
                let ds = i;
                while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == '_') {
                    i += 1;
                }
                let digits: String = s[ds..i].iter().filter(|&&c| c != '_').collect();
                let n = u64::from_str_radix(&digits, radix).map_err(|_| format!("line {line}: bad number"))?;
                push(&mut out, Tok::Num(n as f64));
                continue;
            }
            while i < s.len() && (s[i].is_ascii_digit() || s[i] == '.' || s[i] == '_') {
                i += 1;
            }
            if i < s.len() && (s[i] == 'e' || s[i] == 'E') {
                i += 1;
                if i < s.len() && (s[i] == '+' || s[i] == '-') {
                    i += 1;
                }
                while i < s.len() && s[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = s[start..i].iter().filter(|&&c| c != '_').collect();
            if s.get(i) == Some(&'n') {
                i += 1; // BigInt literals read as plain numbers
            }
            push(&mut out, Tok::Num(text.parse().map_err(|_| format!("line {line}: bad number {text}"))?));
            continue;
        }
        if c == '"' || c == '\'' {
            i += 1;
            let mut v = String::new();
            while i < s.len() && s[i] != c {
                if s[i] == '\n' {
                    return Err(format!("line {line}: unterminated string"));
                }
                if s[i] == '\\' {
                    i += 1;
                    escape(&s, &mut i, &mut v, &mut line);
                } else {
                    v.push(s[i]);
                    i += 1;
                }
            }
            if i >= s.len() {
                return Err(format!("line {line}: unterminated string"));
            }
            i += 1;
            push(&mut out, Tok::Str(v));
            continue;
        }
        if c == '`' {
            i += 1;
            let (mut strs, mut exprs) = (vec![String::new()], Vec::new());
            while i < s.len() && s[i] != '`' {
                if s[i] == '\\' {
                    i += 1;
                    escape(&s, &mut i, strs.last_mut().expect("a piece"), &mut line);
                } else if s[i] == '$' && s.get(i + 1) == Some(&'{') {
                    i += 2;
                    let start = i;
                    let mut depth = 1;
                    while i < s.len() {
                        match s[i] {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            '\n' => line += 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    exprs.push(s[start..i].iter().collect());
                    strs.push(String::new());
                    i += 1;
                } else {
                    if s[i] == '\n' {
                        line += 1;
                    }
                    strs.last_mut().expect("a piece").push(s[i]);
                    i += 1;
                }
            }
            i += 1;
            push(&mut out, Tok::Template(strs, exprs));
            continue;
        }
        let private = c == '#' && s.get(i + 1).map(|d| d.is_alphabetic() || *d == '_' || *d == '$').unwrap_or(false);
        if c.is_alphabetic() || c == '_' || c == '$' || private {
            let start = i;
            i += 1;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_' || s[i] == '$') {
                i += 1;
            }
            push(&mut out, Tok::Ident(s[start..i].iter().collect()));
            continue;
        }
        if c == '/' && regex_allowed(out.last().map(|t: &Token| &t.tok)) {
            let mut j = i + 1;
            let mut class = false;
            while j < s.len() {
                match s[j] {
                    '\\' => j += 1,
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => break,
                    '\n' => return Err(format!("line {line}: unterminated regular expression")),
                    _ => {}
                }
                j += 1;
            }
            if j >= s.len() {
                return Err(format!("line {line}: unterminated regular expression"));
            }
            let body: String = s[i + 1..j].iter().collect();
            j += 1;
            let fstart = j;
            while j < s.len() && s[j].is_alphanumeric() {
                j += 1;
            }
            let flags: String = s[fstart..j].iter().collect();
            i = j;
            push(&mut out, Tok::Regex(body, flags));
            continue;
        }
        let rest: String = s[i..(i + 4).min(s.len())].iter().collect();
        match PUNCTS.iter().find(|p| rest.starts_with(**p)) {
            Some(p) => {
                // `?.5` is a ternary and a number, not optional chaining.
                if *p == "?." && s.get(i + 2).map(|d| d.is_ascii_digit()).unwrap_or(false) {
                    push(&mut out, Tok::Punct("?"));
                    i += 1;
                } else {
                    push(&mut out, Tok::Punct(p));
                    i += p.len();
                }
            }
            None => return Err(format!("line {line}: unexpected character {c:?}")),
        }
    }
    out.push(Token { tok: Tok::Eof, nl: true, line });
    Ok(out)
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
                if let Ok(lo) = u32::from_str_radix(&lo, 16) {
                    code = 0x10000 + ((code - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
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
    Str(Rc<str>),
    Template(Vec<String>, Vec<Expr>),
    Bool(bool),
    Null,
    Undefined,
    Ident(Rc<str>),
    This,
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
    /// (name, extends, [(key, member, static)])
    Class(Rc<str>, Option<Expr>, Vec<(Expr, Member, bool)>),
    Empty,
}

// ------------------------------------------------------------------ parser

pub struct Parser {
    toks: Vec<Token>,
    i: usize,
    /// Nesting depth, to stop pathological input before it overflows the stack.
    depth: u32,
    /// The next function parsed is `async` / a generator.
    next_async: bool,
    next_gen: bool,
    /// Inside a generator body (`yield` is a keyword).
    in_gen: bool,
}

type R<T> = Result<T, String>;

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
];

impl Parser {
    pub fn new(src: &str) -> R<Self> {
        Ok(Parser { toks: lex(src)?, i: 0, depth: 0, next_async: false, next_gen: false, in_gen: false })
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
    fn line(&self) -> u32 {
        self.toks[self.i].line
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
        self.eat_word("await");
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
                    return Ok(Stmt::ForIn(kind, target, of, iter, Box::new(self.stmt()?)));
                }
                let mut decls = vec![(target, if self.eat("=") { Some(self.assign_no_in()?) } else { None })];
                while self.eat(",") {
                    let t = self.binding_target()?;
                    let i = if self.eat("=") { Some(self.assign_no_in()?) } else { None };
                    decls.push((t, i));
                }
                init = Some(Box::new(Stmt::Var(kind, decls)));
            } else {
                let e = self.unary_and_postfix()?;
                if self.is_word("of") || self.is_word("in") {
                    let of = self.is_word("of");
                    self.next();
                    let iter = self.expr()?;
                    self.expect(")")?;
                    let target = to_pattern(e).ok_or("bad for-in target")?;
                    return Ok(Stmt::ForIn("", target, of, iter, Box::new(self.stmt()?)));
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
        let r = (|| {
            self.expect("(")?;
            let params = self.params()?;
            let body = self.block()?;
            Ok(Rc::new(Func { name, params, body, arrow: false, expr_body: false, is_async, is_gen }))
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
        let sup = if self.eat_word("extends") { Some(self.unary_and_postfix()?) } else { None };
        self.expect("{")?;
        let mut members = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let is_static = self.is_word("static") && !matches!(self.peek_at(1), Tok::Punct("("));
            if is_static {
                self.next();
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
        let e = self.assign_rest(left)?;
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

    fn arrow_body(&mut self, params: Vec<Expr>) -> R<Expr> {
        let is_async = std::mem::take(&mut self.next_async);
        let outer = std::mem::replace(&mut self.in_gen, false);
        let r = (|| {
            if self.is("{") {
                let body = self.block()?;
                Ok(Expr::Func(Rc::new(Func { name: None, params, body, arrow: true, expr_body: false, is_async, is_gen: false })))
            } else {
                let e = self.assign()?;
                Ok(Expr::Func(Rc::new(Func { name: None, params, body: vec![Stmt::Return(Some(e))], arrow: true, expr_body: true, is_async, is_gen: false })))
            }
        })();
        self.in_gen = outer;
        r
    }

    fn conditional(&mut self) -> R<Expr> {
        let c = self.binary(0)?;
        if self.eat("?") {
            let a = self.assign()?;
            self.expect(":")?;
            let b = self.assign()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let mut left = self.unary_and_postfix()?;
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
                Expr::Undefined
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
            } else if let Tok::Template(strs, exprs) = self.peek().clone() {
                // A tagged template: called with the pieces and the values.
                self.next();
                let parts = Expr::Array(strs.into_iter().map(|s| Expr::Str(s.into())).collect());
                let mut args = vec![parts];
                for src in exprs {
                    args.push(Parser::new(&src)?.expr()?);
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
            Tok::Str(s) => Ok(Expr::Str(s.into())),
            Tok::Template(strs, exprs) => {
                let mut es = Vec::new();
                for src in exprs {
                    es.push(Parser::new(&src)?.expr()?);
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
                        }))),
                        vec![],
                        false,
                    ))
                }
                w if RESERVED.contains(&w) => Err(format!("line {line}: unexpected '{w}'")),
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
                        t => return Err(format!("line {}: unexpected {} in object", self.line(), describe(&t))),
                    }
                };
                if let Some(getter) = get_set {
                    let f = self.func_rest(None)?;
                    props.push(if getter { Prop::Getter(key, f) } else { Prop::Setter(key, f) });
                } else if self.is("(") {
                    self.next_async = is_async;
                    self.next_gen = is_gen;
                    let name = shorthand.as_ref().map(|n| n.as_str().into());
                    props.push(Prop::KeyValue(key, Expr::Func(self.func_rest(name)?)));
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
