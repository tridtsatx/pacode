//! Lexer and parser for the supported `jq` subset.

use serde_json::Value;

use crate::jq_lex::{Tok, lex};

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Alt,
}

#[derive(Debug, Clone)]
pub(crate) enum Ast {
    Identity,
    Field(Box<Ast>, String),
    Index(Box<Ast>, Box<Ast>),
    Slice(Box<Ast>, Option<Box<Ast>>, Option<Box<Ast>>),
    Iterate(Box<Ast>),
    Try(Box<Ast>),
    Literal(Value),
    Var(String),
    Pipe(Box<Ast>, Box<Ast>),
    Comma(Box<Ast>, Box<Ast>),
    Neg(Box<Ast>),
    Binary(BinOp, Box<Ast>, Box<Ast>),
    If(Box<Ast>, Box<Ast>, Option<Box<Ast>>),
    Array(Option<Box<Ast>>),
    Object(Vec<(Ast, Ast)>),
    Call(String, Vec<Ast>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

type PResult = Result<Ast, String>;

fn boxed(ast: Ast) -> Box<Ast> {
    Box::new(ast)
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, offset: usize) -> Option<&Tok> {
        self.toks.get(self.pos + offset)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> Result<(), String> {
        if self.eat(tok) {
            Ok(())
        } else {
            Err(format!("expected {what}"))
        }
    }

    fn is_keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(w)) if w == word)
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err("filter is nested too deeply".to_string())
        } else {
            Ok(())
        }
    }

    fn pipe(&mut self) -> PResult {
        self.enter()?;
        let lhs = self.comma()?;
        let result = if self.eat(&Tok::Pipe) {
            Ast::Pipe(boxed(lhs), boxed(self.pipe()?))
        } else {
            lhs
        };
        self.depth -= 1;
        Ok(result)
    }

    fn comma(&mut self) -> PResult {
        let mut lhs = self.alt()?;
        while self.eat(&Tok::Comma) {
            lhs = Ast::Comma(boxed(lhs), boxed(self.alt()?));
        }
        Ok(lhs)
    }

    fn alt(&mut self) -> PResult {
        let lhs = self.or()?;
        if self.eat(&Tok::Alt) {
            let rhs = self.alt()?;
            return Ok(Ast::Binary(BinOp::Alt, boxed(lhs), boxed(rhs)));
        }
        Ok(lhs)
    }

    fn or(&mut self) -> PResult {
        let mut lhs = self.and()?;
        while self.is_keyword("or") {
            self.pos += 1;
            lhs = Ast::Binary(BinOp::Or, boxed(lhs), boxed(self.and()?));
        }
        Ok(lhs)
    }

    fn and(&mut self) -> PResult {
        let mut lhs = self.cmp()?;
        while self.is_keyword("and") {
            self.pos += 1;
            lhs = Ast::Binary(BinOp::And, boxed(lhs), boxed(self.cmp()?));
        }
        Ok(lhs)
    }

    fn cmp(&mut self) -> PResult {
        let lhs = self.add()?;
        let op = match self.peek() {
            Some(Tok::EqEq) => BinOp::Eq,
            Some(Tok::Ne) => BinOp::Ne,
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.pos += 1;
        let rhs = self.add()?;
        Ok(Ast::Binary(op, boxed(lhs), boxed(rhs)))
    }

    fn add(&mut self) -> PResult {
        let mut lhs = self.mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.pos += 1;
            lhs = Ast::Binary(op, boxed(lhs), boxed(self.mul()?));
        }
    }

    fn mul(&mut self) -> PResult {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => return Ok(lhs),
            };
            self.pos += 1;
            lhs = Ast::Binary(op, boxed(lhs), boxed(self.unary()?));
        }
    }

    fn unary(&mut self) -> PResult {
        if self.eat(&Tok::Minus) {
            self.enter()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Ok(Ast::Neg(boxed(inner)));
        }
        let primary = self.primary()?;
        self.postfix(primary)
    }

    fn postfix(&mut self, mut base: Ast) -> PResult {
        loop {
            match self.peek() {
                Some(Tok::Field(name)) => {
                    let name = name.clone();
                    self.pos += 1;
                    base = Ast::Field(boxed(base), name);
                }
                Some(Tok::Dot)
                    if matches!(self.peek_at(1), Some(Tok::Str(_)) | Some(Tok::LBracket)) =>
                {
                    self.pos += 1;
                    if let Some(Tok::Str(s)) = self.peek().cloned() {
                        self.pos += 1;
                        base = Ast::Field(boxed(base), s);
                    }
                }
                Some(Tok::LBracket) => {
                    self.pos += 1;
                    base = self.bracket_suffix(base)?;
                }
                Some(Tok::Question) => {
                    self.pos += 1;
                    base = Ast::Try(boxed(base));
                }
                _ => return Ok(base),
            }
        }
    }

    /// After `[`: `]`, `expr]`, `a:b]`, `:b]`, `a:]`.
    fn bracket_suffix(&mut self, base: Ast) -> PResult {
        if self.eat(&Tok::RBracket) {
            return Ok(Ast::Iterate(boxed(base)));
        }
        if self.eat(&Tok::Colon) {
            let to = self.pipe()?;
            self.expect(&Tok::RBracket, "`]`")?;
            return Ok(Ast::Slice(boxed(base), None, Some(boxed(to))));
        }
        let first = self.pipe()?;
        if self.eat(&Tok::Colon) {
            let to = if self.peek() == Some(&Tok::RBracket) {
                None
            } else {
                Some(boxed(self.pipe()?))
            };
            self.expect(&Tok::RBracket, "`]`")?;
            return Ok(Ast::Slice(boxed(base), Some(boxed(first)), to));
        }
        self.expect(&Tok::RBracket, "`]`")?;
        Ok(Ast::Index(boxed(base), boxed(first)))
    }

    fn primary(&mut self) -> PResult {
        let Some(tok) = self.bump() else {
            return Err("unexpected end of filter".to_string());
        };
        match tok {
            Tok::Dot => {
                if let Some(Tok::Str(s)) = self.peek().cloned() {
                    self.pos += 1;
                    return Ok(Ast::Field(boxed(Ast::Identity), s));
                }
                Ok(Ast::Identity)
            }
            Tok::DotDot => Ok(Ast::Call("recurse".to_string(), Vec::new())),
            Tok::Field(name) => Ok(Ast::Field(boxed(Ast::Identity), name)),
            Tok::Num(n) => Ok(Ast::Literal(crate::jq_eval::num(n))),
            Tok::Str(s) => Ok(Ast::Literal(Value::String(s))),
            Tok::Var(name) => Ok(Ast::Var(name)),
            Tok::LParen => {
                let inner = self.pipe()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(inner)
            }
            Tok::LBracket => {
                if self.eat(&Tok::RBracket) {
                    return Ok(Ast::Array(None));
                }
                let inner = self.pipe()?;
                self.expect(&Tok::RBracket, "`]`")?;
                Ok(Ast::Array(Some(boxed(inner))))
            }
            Tok::LBrace => self.object(),
            Tok::Ident(word) => self.ident(word),
            other => Err(format!("unexpected token {other:?}")),
        }
    }

    fn ident(&mut self, word: String) -> PResult {
        match word.as_str() {
            "true" => return Ok(Ast::Literal(Value::Bool(true))),
            "false" => return Ok(Ast::Literal(Value::Bool(false))),
            "null" => return Ok(Ast::Literal(Value::Null)),
            "if" => return self.if_expr(),
            "def" | "as" | "reduce" | "foreach" | "try" | "label" | "import" | "include" => {
                return Err(format!("`{word}` is not supported"));
            }
            _ => {}
        }
        let mut args = Vec::new();
        if self.eat(&Tok::LParen) {
            loop {
                args.push(self.pipe()?);
                if self.eat(&Tok::Semi) {
                    continue;
                }
                self.expect(&Tok::RParen, "`)` after function arguments")?;
                break;
            }
        }
        Ok(Ast::Call(word, args))
    }

    fn if_expr(&mut self) -> PResult {
        let cond = self.pipe()?;
        if !self.is_keyword("then") {
            return Err("expected `then`".to_string());
        }
        self.pos += 1;
        let then = self.pipe()?;
        let otherwise = if self.is_keyword("elif") {
            self.pos += 1;
            Some(boxed(self.if_expr()?))
        } else if self.is_keyword("else") {
            self.pos += 1;
            let e = self.pipe()?;
            if !self.is_keyword("end") {
                return Err("expected `end`".to_string());
            }
            self.pos += 1;
            Some(boxed(e))
        } else {
            if !self.is_keyword("end") {
                return Err("expected `end`".to_string());
            }
            self.pos += 1;
            None
        };
        Ok(Ast::If(boxed(cond), boxed(then), otherwise))
    }

    fn object_value(&mut self) -> PResult {
        let mut value = self.alt()?;
        while self.eat(&Tok::Pipe) {
            value = Ast::Pipe(boxed(value), boxed(self.alt()?));
        }
        Ok(value)
    }

    fn object(&mut self) -> PResult {
        let mut entries = Vec::new();
        if self.eat(&Tok::RBrace) {
            return Ok(Ast::Object(entries));
        }
        loop {
            let Some(tok) = self.bump() else {
                return Err("unterminated object construction".to_string());
            };
            let (key, shorthand) = match tok {
                Tok::Ident(name) => (
                    Ast::Literal(Value::String(name.clone())),
                    Ast::Field(boxed(Ast::Identity), name),
                ),
                Tok::Str(s) => (
                    Ast::Literal(Value::String(s.clone())),
                    Ast::Field(boxed(Ast::Identity), s),
                ),
                Tok::Var(name) => (Ast::Literal(Value::String(name.clone())), Ast::Var(name)),
                Tok::LParen => {
                    let k = self.pipe()?;
                    self.expect(&Tok::RParen, "`)`")?;
                    (k, Ast::Identity)
                }
                other => return Err(format!("invalid object key {other:?}")),
            };
            if self.eat(&Tok::Colon) {
                entries.push((key, self.object_value()?));
            } else if matches!(key, Ast::Literal(_)) {
                entries.push((key, shorthand));
            } else {
                return Err("expected `:` after a computed object key".to_string());
            }
            if self.eat(&Tok::Comma) {
                continue;
            }
            self.expect(&Tok::RBrace, "`}` or `,`")?;
            return Ok(Ast::Object(entries));
        }
    }
}

/// Parse a jq filter.
pub(crate) fn parse_filter(source: &str) -> Result<Ast, String> {
    let toks = lex(source)?;
    if toks.is_empty() {
        return Ok(Ast::Identity);
    }
    let mut parser = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let ast = parser.pipe()?;
    if parser.pos < parser.toks.len() {
        return Err(format!("unexpected token {:?}", parser.toks[parser.pos]));
    }
    Ok(ast)
}
