use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TplPart {
    Str(String),
    /// Raw expression source plus the span where it begins.
    Expr(String, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Template(Vec<TplPart>),
    // keywords
    Let,
    Const,
    Function,
    Return,
    If,
    Else,
    While,
    Do,
    For,
    Of,
    Break,
    Continue,
    True,
    False,
    Null,
    Import,
    Export,
    From,
    As,
    Type,
    Interface,
    Declare,
    Async,
    Await,
    Try,
    Catch,
    Finally,
    Throw,
    Typeof,
    Switch,
    Case,
    Default,
    Extends,
    Test,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    DotDotDot,
    Question,
    QDot,
    At,
    Arrow,
    // operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Eq,
    EqEq,
    EqEqEq,
    Bang,
    BangEq,
    BangEqEq,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    QQ,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    PlusEq,
    MinusEq,
    StarEq,
    StarStarEq,
    SlashEq,
    PercentEq,
    QQEq,
    OrOrEq,
    AndAndEq,
    PlusPlus,
    MinusMinus,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// True when a newline occurred between the previous token and this one.
    pub nl_before: bool,
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub span: Span,
    pub msg: String,
}

impl Diag {
    pub fn new(span: Span, msg: impl Into<String>) -> Diag {
        Diag { span, msg: msg.into() }
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    line: u32,
    col: u32,
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "let" => Tok::Let,
        "const" => Tok::Const,
        "function" => Tok::Function,
        "return" => Tok::Return,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "do" => Tok::Do,
        "for" => Tok::For,
        "of" => Tok::Of,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "true" => Tok::True,
        "false" => Tok::False,
        "null" => Tok::Null,
        "import" => Tok::Import,
        "export" => Tok::Export,
        "from" => Tok::From,
        "as" => Tok::As,
        "type" => Tok::Type,
        "interface" => Tok::Interface,
        "declare" => Tok::Declare,
        "async" => Tok::Async,
        "await" => Tok::Await,
        "try" => Tok::Try,
        "catch" => Tok::Catch,
        "finally" => Tok::Finally,
        "throw" => Tok::Throw,
        "typeof" => Tok::Typeof,
        "switch" => Tok::Switch,
        "case" => Tok::Case,
        "default" => Tok::Default,
        "extends" => Tok::Extends,
        "test" => Tok::Test,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a str, start: Span) -> Lexer<'a> {
        Lexer { src: text.as_bytes(), text, pos: 0, line: start.line, col: start.col }
    }

    fn peek(&self) -> u8 {
        *self.src.get(self.pos).unwrap_or(&0)
    }
    fn peek_at(&self, n: usize) -> u8 {
        *self.src.get(self.pos + n).unwrap_or(&0)
    }
    fn span(&self) -> Span {
        Span { line: self.line, col: self.col }
    }
    fn bump(&mut self) -> u8 {
        let c = self.src[self.pos];
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if c & 0xC0 != 0x80 {
            self.col += 1;
        }
        c
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, Diag> {
        let mut out = Vec::new();
        loop {
            let nl = self.skip_ws()?;
            let span = self.span();
            if self.pos >= self.src.len() {
                out.push(Token { tok: Tok::Eof, span, nl_before: nl });
                break;
            }
            let tok = self.next_tok()?;
            out.push(Token { tok, span, nl_before: nl });
        }
        Ok(out)
    }

    fn skip_ws(&mut self) -> Result<bool, Diag> {
        let mut nl = false;
        loop {
            if self.pos >= self.src.len() {
                return Ok(nl);
            }
            let c = self.peek();
            if c == b'\n' {
                nl = true;
                self.bump();
            } else if c == b' ' || c == b'\t' || c == b'\r' {
                self.bump();
            } else if c == b'/' && self.peek_at(1) == b'/' {
                while self.pos < self.src.len() && self.peek() != b'\n' {
                    self.bump();
                }
            } else if c == b'/' && self.peek_at(1) == b'*' {
                let sp = self.span();
                self.bump();
                self.bump();
                loop {
                    if self.pos >= self.src.len() {
                        return Err(Diag::new(sp, "unterminated block comment"));
                    }
                    if self.peek() == b'*' && self.peek_at(1) == b'/' {
                        self.bump();
                        self.bump();
                        break;
                    }
                    if self.peek() == b'\n' {
                        nl = true;
                    }
                    self.bump();
                }
            } else {
                return Ok(nl);
            }
        }
    }

    fn next_tok(&mut self) -> Result<Tok, Diag> {
        let sp = self.span();
        let c = self.peek();
        if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 {
            let start = self.pos;
            while self.pos < self.src.len() {
                let d = self.peek();
                if d.is_ascii_alphanumeric() || d == b'_' || d == b'$' || d >= 0x80 {
                    self.bump();
                } else {
                    break;
                }
            }
            let s = &self.text[start..self.pos];
            return Ok(keyword(s).unwrap_or_else(|| Tok::Ident(s.to_string())));
        }
        if c.is_ascii_digit() || (c == b'.' && self.peek_at(1).is_ascii_digit()) {
            return self.number(sp);
        }
        if c == b'"' || c == b'\'' {
            return self.string(c, sp);
        }
        if c == b'`' {
            return self.template(sp);
        }
        self.bump();
        let n = self.peek();
        macro_rules! two {
            ($ch:expr, $yes:expr, $no:expr) => {
                if self.peek() == $ch {
                    self.bump();
                    $yes
                } else {
                    $no
                }
            };
        }
        Ok(match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b',' => Tok::Comma,
            b';' => Tok::Semi,
            b':' => Tok::Colon,
            b'@' => Tok::At,
            b'~' => Tok::Tilde,
            b'^' => Tok::Caret,
            b'.' => {
                if n == b'.' && self.peek_at(1) == b'.' {
                    self.bump();
                    self.bump();
                    Tok::DotDotDot
                } else {
                    Tok::Dot
                }
            }
            b'?' => {
                if n == b'?' {
                    self.bump();
                    if self.peek() == b'=' {
                        self.bump();
                        Tok::QQEq
                    } else {
                        Tok::QQ
                    }
                } else if n == b'.' && !self.peek_at(1).is_ascii_digit() {
                    self.bump();
                    Tok::QDot
                } else {
                    Tok::Question
                }
            }
            b'+' => {
                if n == b'+' {
                    self.bump();
                    Tok::PlusPlus
                } else {
                    two!(b'=', Tok::PlusEq, Tok::Plus)
                }
            }
            b'-' => {
                if n == b'-' {
                    self.bump();
                    Tok::MinusMinus
                } else {
                    two!(b'=', Tok::MinusEq, Tok::Minus)
                }
            }
            b'*' => {
                if n == b'*' {
                    self.bump();
                    two!(b'=', Tok::StarStarEq, Tok::StarStar)
                } else {
                    two!(b'=', Tok::StarEq, Tok::Star)
                }
            }
            b'/' => two!(b'=', Tok::SlashEq, Tok::Slash),
            b'%' => two!(b'=', Tok::PercentEq, Tok::Percent),
            b'=' => {
                if n == b'=' {
                    self.bump();
                    two!(b'=', Tok::EqEqEq, Tok::EqEq)
                } else if n == b'>' {
                    self.bump();
                    Tok::Arrow
                } else {
                    Tok::Eq
                }
            }
            b'!' => {
                if n == b'=' {
                    self.bump();
                    two!(b'=', Tok::BangEqEq, Tok::BangEq)
                } else {
                    Tok::Bang
                }
            }
            b'<' => {
                if n == b'<' {
                    self.bump();
                    Tok::Shl
                } else {
                    two!(b'=', Tok::Le, Tok::Lt)
                }
            }
            b'>' => {
                // `>>` is intentionally lexed as two `>` tokens would break generics;
                // we lex `>=` and a single `>` and let the parser combine for shifts.
                two!(b'=', Tok::Ge, Tok::Gt)
            }
            b'&' => {
                if n == b'&' {
                    self.bump();
                    two!(b'=', Tok::AndAndEq, Tok::AndAnd)
                } else {
                    Tok::Amp
                }
            }
            b'|' => {
                if n == b'|' {
                    self.bump();
                    two!(b'=', Tok::OrOrEq, Tok::OrOr)
                } else {
                    Tok::Pipe
                }
            }
            _ => return Err(Diag::new(sp, format!("unexpected character '{}'", c as char))),
        })
    }

    fn number(&mut self, sp: Span) -> Result<Tok, Diag> {
        let start = self.pos;
        if self.peek() == b'0' && (self.peek_at(1) == b'x' || self.peek_at(1) == b'X') {
            self.bump();
            self.bump();
            let s = self.pos;
            while self.peek().is_ascii_hexdigit() || self.peek() == b'_' {
                self.bump();
            }
            let digits: String = self.text[s..self.pos].chars().filter(|c| *c != '_').collect();
            return u64::from_str_radix(&digits, 16)
                .map(|v| Tok::Num(v as f64))
                .map_err(|_| Diag::new(sp, "invalid hex literal"));
        }
        if self.peek() == b'0' && (self.peek_at(1) == b'b' || self.peek_at(1) == b'B') {
            self.bump();
            self.bump();
            let s = self.pos;
            while matches!(self.peek(), b'0' | b'1' | b'_') {
                self.bump();
            }
            let digits: String = self.text[s..self.pos].chars().filter(|c| *c != '_').collect();
            return u64::from_str_radix(&digits, 2)
                .map(|v| Tok::Num(v as f64))
                .map_err(|_| Diag::new(sp, "invalid binary literal"));
        }
        while self.peek().is_ascii_digit() || self.peek() == b'_' {
            self.bump();
        }
        if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
            self.bump();
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.bump();
            }
        } else if self.peek() == b'.' && !self.peek_at(1).is_ascii_alphabetic() && self.peek_at(1) != b'.' {
            // trailing dot like `1.`
            self.bump();
        }
        if self.peek() == b'e' || self.peek() == b'E' {
            let save = (self.pos, self.line, self.col);
            self.bump();
            if self.peek() == b'+' || self.peek() == b'-' {
                self.bump();
            }
            if self.peek().is_ascii_digit() {
                while self.peek().is_ascii_digit() {
                    self.bump();
                }
            } else {
                self.pos = save.0;
                self.line = save.1;
                self.col = save.2;
            }
        }
        let s: String = self.text[start..self.pos].chars().filter(|c| *c != '_').collect();
        s.parse::<f64>().map(Tok::Num).map_err(|_| Diag::new(sp, format!("invalid number '{}'", s)))
    }

    fn escape(&mut self, out: &mut String, sp: Span) -> Result<(), Diag> {
        let c = self.bump();
        match c {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'0' => out.push('\0'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'v' => out.push('\u{b}'),
            b'\\' => out.push('\\'),
            b'\'' => out.push('\''),
            b'"' => out.push('"'),
            b'`' => out.push('`'),
            b'$' => out.push('$'),
            b'\n' => {}
            b'x' => {
                let h = self.hex(2, sp)?;
                out.push(char::from_u32(h).ok_or_else(|| Diag::new(sp, "invalid \\x escape"))?);
            }
            b'u' => {
                let code = if self.peek() == b'{' {
                    self.bump();
                    let s = self.pos;
                    while self.peek() != b'}' && self.pos < self.src.len() {
                        self.bump();
                    }
                    let h = u32::from_str_radix(&self.text[s..self.pos], 16)
                        .map_err(|_| Diag::new(sp, "invalid \\u{} escape"))?;
                    self.bump();
                    h
                } else {
                    let hi = self.hex(4, sp)?;
                    if (0xD800..0xDC00).contains(&hi) && self.peek() == b'\\' && self.peek_at(1) == b'u' {
                        self.bump();
                        self.bump();
                        let lo = self.hex(4, sp)?;
                        0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF)
                    } else {
                        hi
                    }
                };
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            }
            other => {
                return Err(Diag::new(sp, format!("unknown escape sequence '\\{}'", other as char)));
            }
        }
        Ok(())
    }

    fn hex(&mut self, n: usize, sp: Span) -> Result<u32, Diag> {
        let s = self.pos;
        for _ in 0..n {
            if !self.peek().is_ascii_hexdigit() {
                return Err(Diag::new(sp, "invalid hex escape"));
            }
            self.bump();
        }
        u32::from_str_radix(&self.text[s..self.pos], 16).map_err(|_| Diag::new(sp, "invalid hex escape"))
    }

    fn string(&mut self, quote: u8, sp: Span) -> Result<Tok, Diag> {
        self.bump();
        let mut out = String::new();
        loop {
            if self.pos >= self.src.len() || self.peek() == b'\n' {
                return Err(Diag::new(sp, "unterminated string literal"));
            }
            let c = self.peek();
            if c == quote {
                self.bump();
                break;
            }
            if c == b'\\' {
                self.bump();
                self.escape(&mut out, sp)?;
            } else {
                let start = self.pos;
                self.bump();
                while self.pos < self.src.len() && (self.peek() & 0xC0) == 0x80 {
                    self.bump();
                }
                out.push_str(&self.text[start..self.pos]);
            }
        }
        Ok(Tok::Str(out))
    }

    fn template(&mut self, sp: Span) -> Result<Tok, Diag> {
        self.bump();
        let mut parts = Vec::new();
        let mut cur = String::new();
        loop {
            if self.pos >= self.src.len() {
                return Err(Diag::new(sp, "unterminated template literal"));
            }
            let c = self.peek();
            if c == b'`' {
                self.bump();
                break;
            }
            if c == b'\\' {
                self.bump();
                self.escape(&mut cur, sp)?;
            } else if c == b'$' && self.peek_at(1) == b'{' {
                self.bump();
                self.bump();
                if !cur.is_empty() {
                    parts.push(TplPart::Str(std::mem::take(&mut cur)));
                }
                let espan = self.span();
                let start = self.pos;
                let mut depth = 1;
                while depth > 0 {
                    if self.pos >= self.src.len() {
                        return Err(Diag::new(sp, "unterminated template expression"));
                    }
                    match self.peek() {
                        b'{' => {
                            depth += 1;
                            self.bump();
                        }
                        b'}' => {
                            depth -= 1;
                            if depth > 0 {
                                self.bump();
                            }
                        }
                        q @ (b'"' | b'\'') => {
                            self.bump();
                            while self.pos < self.src.len() && self.peek() != q {
                                if self.peek() == b'\\' {
                                    self.bump();
                                }
                                self.bump();
                            }
                            self.bump();
                        }
                        b'`' => {
                            // nested template: skip it, honoring nested ${}
                            self.skip_nested_template()?;
                        }
                        _ => {
                            self.bump();
                        }
                    }
                }
                let src = self.text[start..self.pos].to_string();
                self.bump(); // closing }
                parts.push(TplPart::Expr(src, espan));
            } else {
                let start = self.pos;
                self.bump();
                while self.pos < self.src.len() && (self.peek() & 0xC0) == 0x80 {
                    self.bump();
                }
                cur.push_str(&self.text[start..self.pos]);
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(TplPart::Str(cur));
        }
        Ok(Tok::Template(parts))
    }

    fn skip_nested_template(&mut self) -> Result<(), Diag> {
        let sp = self.span();
        self.bump();
        loop {
            if self.pos >= self.src.len() {
                return Err(Diag::new(sp, "unterminated template literal"));
            }
            match self.peek() {
                b'`' => {
                    self.bump();
                    return Ok(());
                }
                b'\\' => {
                    self.bump();
                    self.bump();
                }
                b'$' if self.peek_at(1) == b'{' => {
                    self.bump();
                    self.bump();
                    let mut depth = 1;
                    while depth > 0 && self.pos < self.src.len() {
                        match self.peek() {
                            b'{' => depth += 1,
                            b'}' => depth -= 1,
                            b'`' => {
                                self.skip_nested_template()?;
                                continue;
                            }
                            _ => {}
                        }
                        self.bump();
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
    }
}
