use crate::ast::*;
use crate::lexer::*;
use std::rc::Rc;

type PResult<T> = Result<T, Diag>;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    is_header: bool,
}

pub fn parse_module(path: &str, src: &str) -> PResult<Module> {
    let is_header = path.ends_with(".clsh");
    let toks = Lexer::new(src, Span { line: 1, col: 1 }).tokenize()?;
    let mut p = Parser { toks, pos: 0, is_header };
    let mut body = Vec::new();
    while !p.at(&Tok::Eof) {
        body.push(p.parse_stmt(true)?);
    }
    Ok(Module { path: path.to_string(), is_header, body })
}

fn parse_expr_source(src: &str, start: Span, is_header: bool) -> PResult<Expr> {
    let toks = Lexer::new(src, start).tokenize()?;
    let mut p = Parser { toks, pos: 0, is_header };
    let e = p.parse_expr()?;
    if !p.at(&Tok::Eof) {
        return Err(Diag::new(p.span(), "unexpected token in template expression"));
    }
    Ok(e)
}

fn mk(kind: ExprKind, span: Span) -> Expr {
    Expr { kind, span }
}

impl Parser {
    // ---------- token helpers ----------
    fn tok(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn tok_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn at(&self, t: &Tok) -> bool {
        self.tok() == t
    }
    fn nl_before(&self) -> bool {
        self.toks[self.pos].nl_before
    }
    fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok, what: &str) -> PResult<Token> {
        if self.at(t) {
            Ok(self.advance())
        } else {
            Err(self.unexpected(what))
        }
    }
    fn unexpected(&self, expected: &str) -> Diag {
        Diag::new(self.span(), format!("expected {}, found {}", expected, describe(self.tok())))
    }
    fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> Option<T> {
        let save = self.pos;
        match f(self) {
            Ok(v) => Some(v),
            Err(_) => {
                self.pos = save;
                None
            }
        }
    }

    /// Identifier, allowing contextual keywords.
    fn ident_name(&self) -> Option<String> {
        match self.tok() {
            Tok::Ident(s) => Some(s.clone()),
            Tok::Type => Some("type".into()),
            Tok::Of => Some("of".into()),
            Tok::From => Some("from".into()),
            Tok::As => Some("as".into()),
            Tok::Test => Some("test".into()),
            Tok::Declare => Some("declare".into()),
            _ => None,
        }
    }
    fn expect_ident(&mut self, what: &str) -> PResult<(String, Span)> {
        let sp = self.span();
        match self.ident_name() {
            Some(s) => {
                self.advance();
                Ok((s, sp))
            }
            None => Err(self.unexpected(what)),
        }
    }
    /// Any identifier-like word including all keywords (property names).
    fn prop_name(&mut self) -> PResult<String> {
        let t = self.tok().clone();
        let s = match t {
            Tok::Ident(s) => s,
            Tok::Str(s) => s,
            Tok::Num(n) => crate::value::fmt_num(n),
            other => match keyword_text(&other) {
                Some(k) => k.to_string(),
                None => return Err(self.unexpected("property name")),
            },
        };
        self.advance();
        Ok(s)
    }
    fn terminator(&mut self) -> PResult<()> {
        if self.eat(&Tok::Semi) {
            return Ok(());
        }
        if self.at(&Tok::RBrace) || self.at(&Tok::Eof) || self.nl_before() {
            return Ok(());
        }
        Err(self.unexpected("';'"))
    }

    // ---------- types ----------
    pub fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.eat(&Tok::Pipe);
        let first = self.parse_type_postfix()?;
        if self.at(&Tok::Pipe) {
            let mut v = vec![first];
            while self.eat(&Tok::Pipe) {
                v.push(self.parse_type_postfix()?);
            }
            Ok(TypeExpr::Union(v))
        } else {
            Ok(first)
        }
    }

    fn parse_type_postfix(&mut self) -> PResult<TypeExpr> {
        let mut t = self.parse_type_primary()?;
        while self.at(&Tok::LBracket) && *self.tok_at(1) == Tok::RBracket && !self.nl_before() {
            self.advance();
            self.advance();
            t = TypeExpr::Array(Box::new(t));
        }
        Ok(t)
    }

    fn parse_type_args(&mut self) -> PResult<Vec<TypeExpr>> {
        self.expect(&Tok::Lt, "'<'")?;
        let mut v = Vec::new();
        loop {
            v.push(self.parse_type()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::Gt, "'>'")?;
        Ok(v)
    }

    fn parse_tparams(&mut self) -> PResult<Vec<TypeParam>> {
        let mut v = Vec::new();
        if !self.eat(&Tok::Lt) {
            return Ok(v);
        }
        loop {
            let (name, _) = self.expect_ident("type parameter name")?;
            let constraint = if self.eat(&Tok::Extends) { Some(self.parse_type()?) } else { None };
            v.push(TypeParam { name, constraint });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::Gt, "'>'")?;
        Ok(v)
    }

    fn parse_fn_type_params(&mut self) -> PResult<Vec<FnTypeParam>> {
        self.expect(&Tok::LParen, "'('")?;
        let mut v = Vec::new();
        while !self.at(&Tok::RParen) {
            let rest = self.eat(&Tok::DotDotDot);
            let (name, _) = self.expect_ident("parameter name")?;
            let optional = self.eat(&Tok::Question);
            self.expect(&Tok::Colon, "':'")?;
            let ty = self.parse_type()?;
            v.push(FnTypeParam { name, ty, optional, rest });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(v)
    }

    fn parse_type_primary(&mut self) -> PResult<TypeExpr> {
        let sp = self.span();
        match self.tok().clone() {
            Tok::Str(s) => {
                self.advance();
                Ok(TypeExpr::LitStr(s))
            }
            Tok::Num(n) => {
                self.advance();
                Ok(TypeExpr::LitNum(n))
            }
            Tok::Minus if matches!(self.tok_at(1), Tok::Num(_)) => {
                self.advance();
                if let Tok::Num(n) = self.advance().tok {
                    Ok(TypeExpr::LitNum(-n))
                } else {
                    unreachable!()
                }
            }
            Tok::True => {
                self.advance();
                Ok(TypeExpr::LitBool(true))
            }
            Tok::False => {
                self.advance();
                Ok(TypeExpr::LitBool(false))
            }
            Tok::Null => {
                self.advance();
                Ok(TypeExpr::Null)
            }
            Tok::LBrace => self.parse_object_type(),
            Tok::LBracket => {
                self.advance();
                let mut v = Vec::new();
                while !self.at(&Tok::RBracket) {
                    v.push(self.parse_type()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket, "']'")?;
                Ok(TypeExpr::Tuple(v))
            }
            Tok::Lt => {
                let tparams = self.parse_tparams()?;
                let params = self.parse_fn_type_params()?;
                self.expect(&Tok::Arrow, "'=>'")?;
                let ret = self.parse_type()?;
                Ok(TypeExpr::Func { tparams, params, ret: Box::new(ret), span: sp })
            }
            Tok::LParen => {
                // function type or parenthesized type
                if let Some(t) = self.attempt(|p| {
                    let params = p.parse_fn_type_params()?;
                    p.expect(&Tok::Arrow, "'=>'")?;
                    let ret = p.parse_type()?;
                    Ok(TypeExpr::Func { tparams: vec![], params, ret: Box::new(ret), span: sp })
                }) {
                    return Ok(t);
                }
                self.advance();
                let t = self.parse_type()?;
                self.expect(&Tok::RParen, "')'")?;
                Ok(t)
            }
            _ => {
                if let Some(mut name) = self.ident_name() {
                    self.advance();
                    while self.at(&Tok::Dot) {
                        self.advance();
                        match self.ident_name() {
                            Some(n) => {
                                self.advance();
                                name = format!("{}.{}", name, n);
                            }
                            None => return Err(self.unexpected("type name")),
                        }
                    }
                    let args = if self.at(&Tok::Lt) { self.parse_type_args()? } else { vec![] };
                    Ok(TypeExpr::Named { name, args, span: sp })
                } else {
                    Err(self.unexpected("type"))
                }
            }
        }
    }

    fn parse_members(&mut self) -> PResult<Vec<PropSig>> {
        self.expect(&Tok::LBrace, "'{'")?;
        let mut props = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::LBracket) {
                // index signature [key: string]: T
                self.advance();
                self.expect_ident("key name")?;
                self.expect(&Tok::Colon, "':'")?;
                self.parse_type()?;
                self.expect(&Tok::RBracket, "']'")?;
                self.expect(&Tok::Colon, "':'")?;
                let ty = self.parse_type()?;
                props.push(PropSig { name: String::new(), ty, optional: false, index: true });
            } else {
                let sp = self.span();
                let name = self.prop_name()?;
                let optional = self.eat(&Tok::Question);
                if self.at(&Tok::LParen) || self.at(&Tok::Lt) {
                    // method signature
                    let tparams = self.parse_tparams()?;
                    let params = self.parse_fn_type_params()?;
                    self.expect(&Tok::Colon, "':' and return type")?;
                    let ret = self.parse_type()?;
                    props.push(PropSig {
                        name,
                        ty: TypeExpr::Func { tparams, params, ret: Box::new(ret), span: sp },
                        optional,
                        index: false,
                    });
                } else {
                    self.expect(&Tok::Colon, "':'")?;
                    let ty = self.parse_type()?;
                    props.push(PropSig { name, ty, optional, index: false });
                }
            }
            if !(self.eat(&Tok::Semi) || self.eat(&Tok::Comma)) && !self.at(&Tok::RBrace) && !self.nl_before() {
                return Err(self.unexpected("';' or '}'"));
            }
        }
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(props)
    }

    fn parse_object_type(&mut self) -> PResult<TypeExpr> {
        let sp = self.span();
        let props = self.parse_members()?;
        Ok(TypeExpr::Object(props, sp))
    }

    // ---------- patterns ----------
    fn parse_pattern(&mut self) -> PResult<Pattern> {
        let sp = self.span();
        match self.tok() {
            Tok::LBrace => {
                self.advance();
                let mut props = Vec::new();
                let mut rest = None;
                while !self.at(&Tok::RBrace) {
                    if self.eat(&Tok::DotDotDot) {
                        rest = Some(self.expect_ident("rest name")?.0);
                    } else {
                        let key_sp = self.span();
                        let key = self.prop_name()?;
                        let target = if self.eat(&Tok::Colon) {
                            self.parse_pattern()?
                        } else {
                            Pattern::Ident(key.clone(), key_sp)
                        };
                        let default = if self.eat(&Tok::Eq) { Some(self.parse_assign()?) } else { None };
                        props.push(PatProp { key, target, default });
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBrace, "'}'")?;
                Ok(Pattern::Object { props, rest, span: sp })
            }
            Tok::LBracket => {
                self.advance();
                let mut elems = Vec::new();
                let mut rest = None;
                while !self.at(&Tok::RBracket) {
                    if self.at(&Tok::Comma) {
                        self.advance();
                        elems.push(None);
                        continue;
                    }
                    if self.eat(&Tok::DotDotDot) {
                        rest = Some(self.expect_ident("rest name")?.0);
                    } else {
                        let target = self.parse_pattern()?;
                        let default = if self.eat(&Tok::Eq) { Some(self.parse_assign()?) } else { None };
                        elems.push(Some(PatElem { target, default }));
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket, "']'")?;
                Ok(Pattern::Array { elems, rest, span: sp })
            }
            _ => {
                let (n, s) = self.expect_ident("identifier or pattern")?;
                Ok(Pattern::Ident(n, s))
            }
        }
    }

    // ---------- functions ----------
    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(&Tok::LParen, "'('")?;
        let mut v = Vec::new();
        while !self.at(&Tok::RParen) {
            let rest = self.eat(&Tok::DotDotDot);
            let pattern = self.parse_pattern()?;
            let optional = self.eat(&Tok::Question);
            let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
            let default = if self.eat(&Tok::Eq) { Some(self.parse_assign()?) } else { None };
            v.push(Param { pattern, ty, optional, default, rest });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(v)
    }

    fn parse_block_body(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&Tok::LBrace, "'{'")?;
        let mut v = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::Eof) {
                return Err(self.unexpected("'}'"));
            }
            v.push(self.parse_stmt(false)?);
        }
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(v)
    }

    fn parse_function(&mut self, is_async: bool, decorators: Vec<Decorator>, exported: bool, require_name: bool) -> PResult<FuncDecl> {
        let sp = self.span();
        self.expect(&Tok::Function, "'function'")?;
        let name = if self.ident_name().is_some() {
            Some(self.expect_ident("function name")?.0)
        } else if require_name {
            return Err(self.unexpected("function name"));
        } else {
            None
        };
        let tparams = self.parse_tparams()?;
        let params = self.parse_params()?;
        let ret = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
        let body = self.parse_block_body()?;
        Ok(FuncDecl {
            name,
            tparams,
            params,
            ret,
            body,
            is_async,
            is_arrow: false,
            decorators,
            exported,
            span: sp,
            meta: Default::default(),
        })
    }

    fn parse_arrow_rest(&mut self, is_async: bool, tparams: Vec<TypeParam>, params: Vec<Param>, ret: Option<TypeExpr>, sp: Span) -> PResult<Expr> {
        self.expect(&Tok::Arrow, "'=>'")?;
        let body = if self.at(&Tok::LBrace) {
            self.parse_block_body()?
        } else {
            let e = self.parse_assign()?;
            let es = e.span;
            vec![Stmt { kind: StmtKind::Return(Some(e)), span: es }]
        };
        Ok(mk(
            ExprKind::Func(Rc::new(FuncDecl {
                name: None,
                tparams,
                params,
                ret,
                body,
                is_async,
                is_arrow: true,
                decorators: vec![],
                exported: false,
                span: sp,
                meta: Default::default(),
            })),
            sp,
        ))
    }

    fn try_arrow(&mut self, is_async: bool) -> Option<Expr> {
        let sp = self.span();
        self.attempt(|p| {
            let tparams = p.parse_tparams()?;
            let params = p.parse_params()?;
            let ret = if p.eat(&Tok::Colon) { Some(p.parse_type()?) } else { None };
            if !p.at(&Tok::Arrow) {
                return Err(p.unexpected("'=>'"));
            }
            p.parse_arrow_rest(is_async, tparams, params, ret, sp)
        })
    }

    // ---------- statements ----------
    fn parse_decorators(&mut self) -> PResult<Vec<Decorator>> {
        let mut v = Vec::new();
        while self.at(&Tok::At) {
            let sp = self.span();
            self.advance();
            let (name, _) = self.expect_ident("decorator name")?;
            let mut args = Vec::new();
            if self.eat(&Tok::LParen) {
                while !self.at(&Tok::RParen) {
                    args.push(self.parse_assign()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen, "')'")?;
            }
            v.push(Decorator { name, args, span: sp });
        }
        Ok(v)
    }

    fn parse_import_specs(&mut self) -> PResult<Vec<ImportSpec>> {
        self.expect(&Tok::LBrace, "'{'")?;
        let mut specs = Vec::new();
        while !self.at(&Tok::RBrace) {
            let name = self.prop_name()?;
            let alias = if self.eat(&Tok::As) { self.expect_ident("alias")?.0 } else { name.clone() };
            specs.push(ImportSpec { name, alias });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(specs)
    }

    fn parse_stmt(&mut self, top: bool) -> PResult<Stmt> {
        let sp = self.span();
        let kind = self.parse_stmt_kind(top)?;
        Ok(Stmt { kind, span: sp })
    }

    fn parse_var_decl(&mut self, exported: bool) -> PResult<StmtKind> {
        let kind = if self.eat(&Tok::Const) {
            VarKind::Const
        } else {
            self.expect(&Tok::Let, "'let' or 'const'")?;
            VarKind::Let
        };
        let pattern = self.parse_pattern()?;
        let ty = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
        let init = if self.eat(&Tok::Eq) { Some(self.parse_expr()?) } else { None };
        Ok(StmtKind::Var { kind, pattern, ty, init, exported })
    }

    fn parse_stmt_kind(&mut self, top: bool) -> PResult<StmtKind> {
        let sp = self.span();
        match self.tok().clone() {
            Tok::At => {
                let decorators = self.parse_decorators()?;
                let exported = self.eat(&Tok::Export);
                let is_async = self.eat(&Tok::Async);
                if !self.at(&Tok::Function) {
                    return Err(Diag::new(self.span(), "decorators can only be applied to functions"));
                }
                if !top {
                    return Err(Diag::new(sp, "decorated functions must be declared at the top level"));
                }
                let f = self.parse_function(is_async, decorators, exported, true)?;
                Ok(StmtKind::Func(Rc::new(f)))
            }
            Tok::Export => {
                if !top {
                    return Err(Diag::new(sp, "'export' is only allowed at the top level"));
                }
                self.advance();
                match self.tok().clone() {
                    Tok::LBrace => {
                        let specs = self.parse_import_specs()?;
                        self.terminator()?;
                        Ok(StmtKind::ExportNames(specs))
                    }
                    Tok::Function => {
                        let f = self.parse_function(false, vec![], true, true)?;
                        Ok(StmtKind::Func(Rc::new(f)))
                    }
                    Tok::Async => {
                        self.advance();
                        let f = self.parse_function(true, vec![], true, true)?;
                        Ok(StmtKind::Func(Rc::new(f)))
                    }
                    Tok::At => {
                        let decorators = self.parse_decorators()?;
                        let is_async = self.eat(&Tok::Async);
                        let f = self.parse_function(is_async, decorators, true, true)?;
                        Ok(StmtKind::Func(Rc::new(f)))
                    }
                    Tok::Const | Tok::Let => {
                        let k = self.parse_var_decl(true)?;
                        self.terminator()?;
                        Ok(k)
                    }
                    Tok::Interface => self.parse_interface(true),
                    Tok::Type => self.parse_type_alias(true),
                    Tok::Declare => {
                        self.advance();
                        self.parse_declare(true)
                    }
                    _ => Err(self.unexpected("declaration after 'export'")),
                }
            }
            Tok::Declare if top => {
                if !self.is_header {
                    return Err(Diag::new(sp, "'declare' is only allowed in .clsh header files"));
                }
                self.advance();
                self.parse_declare(false)
            }
            Tok::Import => {
                if !top {
                    return Err(Diag::new(sp, "'import' is only allowed at the top level"));
                }
                self.advance();
                let mut type_only = false;
                if self.at(&Tok::Type) && matches!(self.tok_at(1), Tok::LBrace) {
                    self.advance();
                    type_only = true;
                }
                if let Tok::Str(from) = self.tok().clone() {
                    self.advance();
                    self.terminator()?;
                    return Ok(StmtKind::Import { specs: vec![], ns: None, from, type_only });
                }
                if self.eat(&Tok::Star) {
                    self.expect(&Tok::As, "'as'")?;
                    let (ns, _) = self.expect_ident("namespace name")?;
                    self.expect(&Tok::From, "'from'")?;
                    let from = self.expect_str("module path")?;
                    self.terminator()?;
                    return Ok(StmtKind::Import { specs: vec![], ns: Some(ns), from, type_only });
                }
                let specs = self.parse_import_specs()?;
                self.expect(&Tok::From, "'from'")?;
                let from = self.expect_str("module path")?;
                self.terminator()?;
                Ok(StmtKind::Import { specs, ns: None, from, type_only })
            }
            Tok::Interface => self.parse_interface(false),
            Tok::Type if matches!(self.tok_at(1), Tok::Ident(_)) => self.parse_type_alias(false),
            Tok::Test if top && matches!(self.tok_at(1), Tok::Str(_)) => {
                self.advance();
                let name = self.expect_str("test name")?;
                let body = self.parse_block_body()?;
                Ok(StmtKind::Test { name, body })
            }
            Tok::Function => {
                let f = self.parse_function(false, vec![], false, true)?;
                Ok(StmtKind::Func(Rc::new(f)))
            }
            Tok::Async if matches!(self.tok_at(1), Tok::Function) => {
                self.advance();
                let f = self.parse_function(true, vec![], false, true)?;
                Ok(StmtKind::Func(Rc::new(f)))
            }
            Tok::Let | Tok::Const => {
                let k = self.parse_var_decl(false)?;
                self.terminator()?;
                Ok(k)
            }
            Tok::LBrace => Ok(StmtKind::Block(self.parse_block_body()?)),
            Tok::Semi => {
                self.advance();
                Ok(StmtKind::Block(vec![]))
            }
            Tok::If => {
                self.advance();
                self.expect(&Tok::LParen, "'('")?;
                let c = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                let then = Box::new(self.parse_stmt(false)?);
                let els = if self.eat(&Tok::Else) { Some(Box::new(self.parse_stmt(false)?)) } else { None };
                Ok(StmtKind::If(c, then, els))
            }
            Tok::While => {
                self.advance();
                self.expect(&Tok::LParen, "'('")?;
                let c = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                let body = Box::new(self.parse_stmt(false)?);
                Ok(StmtKind::While(c, body))
            }
            Tok::Do => {
                self.advance();
                let body = Box::new(self.parse_stmt(false)?);
                self.expect(&Tok::While, "'while'")?;
                self.expect(&Tok::LParen, "'('")?;
                let c = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                self.eat(&Tok::Semi);
                Ok(StmtKind::DoWhile(body, c))
            }
            Tok::For => self.parse_for(),
            Tok::Switch => {
                self.advance();
                self.expect(&Tok::LParen, "'('")?;
                let d = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                self.expect(&Tok::LBrace, "'{'")?;
                let mut cases = Vec::new();
                while !self.at(&Tok::RBrace) {
                    let test = if self.eat(&Tok::Default) {
                        None
                    } else {
                        self.expect(&Tok::Case, "'case' or 'default'")?;
                        Some(self.parse_expr()?)
                    };
                    self.expect(&Tok::Colon, "':'")?;
                    let mut body = Vec::new();
                    while !matches!(self.tok(), Tok::Case | Tok::Default | Tok::RBrace | Tok::Eof) {
                        body.push(self.parse_stmt(false)?);
                    }
                    cases.push(Case { test, body });
                }
                self.expect(&Tok::RBrace, "'}'")?;
                Ok(StmtKind::Switch(d, cases))
            }
            Tok::Return => {
                self.advance();
                let v = if self.at(&Tok::Semi) || self.at(&Tok::RBrace) || self.at(&Tok::Eof) || self.nl_before() {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.terminator()?;
                Ok(StmtKind::Return(v))
            }
            Tok::Break => {
                self.advance();
                self.terminator()?;
                Ok(StmtKind::Break)
            }
            Tok::Continue => {
                self.advance();
                self.terminator()?;
                Ok(StmtKind::Continue)
            }
            Tok::Throw => {
                self.advance();
                let e = self.parse_expr()?;
                self.terminator()?;
                Ok(StmtKind::Throw(e))
            }
            Tok::Try => {
                self.advance();
                let block = self.parse_block_body()?;
                let mut catch = None;
                let mut finally = None;
                if self.eat(&Tok::Catch) {
                    let binding = if self.eat(&Tok::LParen) {
                        let p = self.parse_pattern()?;
                        if self.eat(&Tok::Colon) {
                            self.parse_type()?;
                        }
                        self.expect(&Tok::RParen, "')'")?;
                        Some(p)
                    } else {
                        None
                    };
                    catch = Some((binding, self.parse_block_body()?));
                }
                if self.eat(&Tok::Finally) {
                    finally = Some(self.parse_block_body()?);
                }
                if catch.is_none() && finally.is_none() {
                    return Err(Diag::new(sp, "try requires a catch or finally block"));
                }
                Ok(StmtKind::Try { block, catch, finally })
            }
            _ => {
                let e = self.parse_expr()?;
                self.terminator()?;
                Ok(StmtKind::Expr(e))
            }
        }
    }

    fn expect_str(&mut self, what: &str) -> PResult<String> {
        if let Tok::Str(s) = self.tok().clone() {
            self.advance();
            Ok(s)
        } else {
            Err(self.unexpected(what))
        }
    }

    fn parse_for(&mut self) -> PResult<StmtKind> {
        self.advance();
        self.expect(&Tok::LParen, "'('")?;
        let mut init: Option<Box<Stmt>> = None;
        if self.at(&Tok::Let) || self.at(&Tok::Const) {
            let isp = self.span();
            let kind = if self.at(&Tok::Const) { VarKind::Const } else { VarKind::Let };
            let save = self.pos;
            self.advance();
            let pattern = self.parse_pattern()?;
            if self.eat(&Tok::Of) {
                let iter = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                let body = Box::new(self.parse_stmt(false)?);
                return Ok(StmtKind::ForOf { kind, pattern, iter, body });
            }
            self.pos = save;
            let k = self.parse_var_decl(false)?;
            init = Some(Box::new(Stmt { kind: k, span: isp }));
        } else if !self.at(&Tok::Semi) {
            let e = self.parse_expr()?;
            let es = e.span;
            init = Some(Box::new(Stmt { kind: StmtKind::Expr(e), span: es }));
        }
        self.expect(&Tok::Semi, "';'")?;
        let cond = if self.at(&Tok::Semi) { None } else { Some(self.parse_expr()?) };
        self.expect(&Tok::Semi, "';'")?;
        let update = if self.at(&Tok::RParen) { None } else { Some(self.parse_expr()?) };
        self.expect(&Tok::RParen, "')'")?;
        let body = Box::new(self.parse_stmt(false)?);
        Ok(StmtKind::For { init, cond, update, body })
    }

    fn parse_interface(&mut self, exported: bool) -> PResult<StmtKind> {
        self.expect(&Tok::Interface, "'interface'")?;
        let (name, _) = self.expect_ident("interface name")?;
        let tparams = self.parse_tparams()?;
        let mut extends = Vec::new();
        if self.eat(&Tok::Extends) {
            loop {
                extends.push(self.parse_type_primary()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        let props = self.parse_members()?;
        Ok(StmtKind::Interface(InterfaceDecl { name, tparams, extends, props, exported }))
    }

    fn parse_type_alias(&mut self, exported: bool) -> PResult<StmtKind> {
        self.expect(&Tok::Type, "'type'")?;
        let (name, _) = self.expect_ident("type name")?;
        let tparams = self.parse_tparams()?;
        self.expect(&Tok::Eq, "'='")?;
        let ty = self.parse_type()?;
        self.terminator()?;
        Ok(StmtKind::TypeAlias { name, tparams, ty, exported })
    }

    fn parse_declare(&mut self, exported: bool) -> PResult<StmtKind> {
        if self.eat(&Tok::Function) {
            let (name, _) = self.expect_ident("function name")?;
            let tparams = self.parse_tparams()?;
            let params = self.parse_fn_type_params()?;
            self.expect(&Tok::Colon, "':' and return type")?;
            let ret = self.parse_type()?;
            self.terminator()?;
            Ok(StmtKind::DeclareFunc { name, tparams, params, ret, exported })
        } else if self.eat(&Tok::Const) || self.eat(&Tok::Let) {
            let (name, _) = self.expect_ident("name")?;
            self.expect(&Tok::Colon, "':'")?;
            let ty = self.parse_type()?;
            self.terminator()?;
            Ok(StmtKind::DeclareConst { name, ty, exported })
        } else if self.at(&Tok::Interface) {
            self.parse_interface(exported)
        } else if self.at(&Tok::Type) {
            self.parse_type_alias(exported)
        } else {
            Err(self.unexpected("'function' or 'const' after 'declare'"))
        }
    }

    // ---------- expressions ----------
    pub fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_assign()
    }

    fn parse_assign(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let left = self.parse_ternary()?;
        let op = match self.tok() {
            Tok::Eq => Some(AssignOp::Assign),
            Tok::PlusEq => Some(AssignOp::Bin(BinOp::Add)),
            Tok::MinusEq => Some(AssignOp::Bin(BinOp::Sub)),
            Tok::StarEq => Some(AssignOp::Bin(BinOp::Mul)),
            Tok::StarStarEq => Some(AssignOp::Bin(BinOp::Pow)),
            Tok::SlashEq => Some(AssignOp::Bin(BinOp::Div)),
            Tok::PercentEq => Some(AssignOp::Bin(BinOp::Mod)),
            Tok::QQEq => Some(AssignOp::Log(LogOp::Nullish)),
            Tok::OrOrEq => Some(AssignOp::Log(LogOp::Or)),
            Tok::AndAndEq => Some(AssignOp::Log(LogOp::And)),
            _ => None,
        };
        if let Some(op) = op {
            match left.kind {
                ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. } => {}
                _ => return Err(Diag::new(sp, "invalid assignment target")),
            }
            if let ExprKind::Member { optional: true, .. } | ExprKind::Index { optional: true, .. } = left.kind {
                return Err(Diag::new(sp, "cannot assign through optional chain"));
            }
            self.advance();
            let value = self.parse_assign()?;
            return Ok(mk(ExprKind::Assign { op, target: Box::new(left), value: Box::new(value) }, sp));
        }
        Ok(left)
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let c = self.parse_nullish()?;
        if self.eat(&Tok::Question) {
            let a = self.parse_assign()?;
            self.expect(&Tok::Colon, "':'")?;
            let b = self.parse_assign()?;
            return Ok(mk(ExprKind::Cond(Box::new(c), Box::new(a), Box::new(b)), sp));
        }
        Ok(c)
    }

    fn parse_nullish(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_or()?;
        while self.eat(&Tok::QQ) {
            let r = self.parse_or()?;
            l = mk(ExprKind::Logical(LogOp::Nullish, Box::new(l), Box::new(r)), sp);
        }
        Ok(l)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_and()?;
        while self.eat(&Tok::OrOr) {
            let r = self.parse_and()?;
            l = mk(ExprKind::Logical(LogOp::Or, Box::new(l), Box::new(r)), sp);
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_bitor()?;
        while self.eat(&Tok::AndAnd) {
            let r = self.parse_bitor()?;
            l = mk(ExprKind::Logical(LogOp::And, Box::new(l), Box::new(r)), sp);
        }
        Ok(l)
    }

    fn bin(&self, op: BinOp, l: Expr, r: Expr, sp: Span) -> Expr {
        mk(ExprKind::Binary(op, Box::new(l), Box::new(r)), sp)
    }

    fn parse_bitor(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_bitxor()?;
        while self.at(&Tok::Pipe) {
            self.advance();
            let r = self.parse_bitxor()?;
            l = self.bin(BinOp::BitOr, l, r, sp);
        }
        Ok(l)
    }
    fn parse_bitxor(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_bitand()?;
        while self.eat(&Tok::Caret) {
            let r = self.parse_bitand()?;
            l = self.bin(BinOp::BitXor, l, r, sp);
        }
        Ok(l)
    }
    fn parse_bitand(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_equality()?;
        while self.eat(&Tok::Amp) {
            let r = self.parse_equality()?;
            l = self.bin(BinOp::BitAnd, l, r, sp);
        }
        Ok(l)
    }

    fn parse_equality(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_relational()?;
        loop {
            let op = match self.tok() {
                Tok::EqEqEq => BinOp::Eq,
                Tok::BangEqEq => BinOp::Ne,
                Tok::EqEq => return Err(Diag::new(self.span(), "use '===' instead of '==' (Claudescript has strict equality only)")),
                Tok::BangEq => return Err(Diag::new(self.span(), "use '!==' instead of '!=' (Claudescript has strict equality only)")),
                _ => break,
            };
            self.advance();
            let r = self.parse_relational()?;
            l = self.bin(op, l, r, sp);
        }
        Ok(l)
    }

    fn parse_relational(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_shift()?;
        loop {
            let op = match self.tok() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                Tok::As => {
                    self.advance();
                    let t = self.parse_type()?;
                    l = mk(ExprKind::As(Box::new(l), t), sp);
                    continue;
                }
                _ => break,
            };
            self.advance();
            let r = self.parse_shift()?;
            l = self.bin(op, l, r, sp);
        }
        Ok(l)
    }

    fn adjacent_gt(&self) -> bool {
        if !self.at(&Tok::Gt) || *self.tok_at(1) != Tok::Gt {
            return false;
        }
        let a = &self.toks[self.pos];
        let b = &self.toks[(self.pos + 1).min(self.toks.len() - 1)];
        a.span.line == b.span.line && b.span.col == a.span.col + 1
    }

    fn parse_shift(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_additive()?;
        loop {
            if self.at(&Tok::Shl) {
                self.advance();
                let r = self.parse_additive()?;
                l = self.bin(BinOp::Shl, l, r, sp);
            } else if self.adjacent_gt() {
                self.advance();
                self.advance();
                let r = self.parse_additive()?;
                l = self.bin(BinOp::Shr, l, r, sp);
            } else {
                break;
            }
        }
        Ok(l)
    }

    fn parse_additive(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_mul()?;
        loop {
            let op = match self.tok() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let r = self.parse_mul()?;
            l = self.bin(op, l, r, sp);
        }
        Ok(l)
    }

    fn parse_mul(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut l = self.parse_pow()?;
        loop {
            let op = match self.tok() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let r = self.parse_pow()?;
            l = self.bin(op, l, r, sp);
        }
        Ok(l)
    }

    fn parse_pow(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let l = self.parse_unary()?;
        if self.eat(&Tok::StarStar) {
            let r = self.parse_pow()?;
            return Ok(self.bin(BinOp::Pow, l, r, sp));
        }
        Ok(l)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let sp = self.span();
        match self.tok() {
            Tok::Minus => {
                self.advance();
                let e = self.parse_unary()?;
                if let ExprKind::Num(n) = e.kind {
                    return Ok(mk(ExprKind::Num(-n), sp));
                }
                Ok(mk(ExprKind::Unary(UnOp::Neg, Box::new(e)), sp))
            }
            Tok::Plus => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(mk(ExprKind::Unary(UnOp::Plus, Box::new(e)), sp))
            }
            Tok::Bang => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(mk(ExprKind::Unary(UnOp::Not, Box::new(e)), sp))
            }
            Tok::Tilde => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(mk(ExprKind::Unary(UnOp::BitNot, Box::new(e)), sp))
            }
            Tok::Typeof => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(mk(ExprKind::TypeOf(Box::new(e)), sp))
            }
            Tok::Await => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(mk(ExprKind::Await(Box::new(e)), sp))
            }
            Tok::PlusPlus | Tok::MinusMinus => {
                let inc = self.at(&Tok::PlusPlus);
                self.advance();
                let e = self.parse_unary()?;
                self.check_update_target(&e)?;
                Ok(mk(ExprKind::Update { inc, prefix: true, target: Box::new(e) }, sp))
            }
            _ => self.parse_postfix(),
        }
    }

    fn check_update_target(&self, e: &Expr) -> PResult<()> {
        match e.kind {
            ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. } => Ok(()),
            _ => Err(Diag::new(e.span, "invalid increment/decrement target")),
        }
    }

    fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(&Tok::LParen, "'('")?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            if self.eat(&Tok::DotDotDot) {
                args.push(Arg::Spread(self.parse_assign()?));
            } else {
                args.push(Arg::Item(self.parse_assign()?));
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(args)
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let mut e = self.parse_primary()?;
        let mut has_opt = false;
        loop {
            match self.tok() {
                Tok::Dot => {
                    self.advance();
                    let name = self.prop_name()?;
                    e = mk(ExprKind::Member { obj: Box::new(e), name, optional: false }, sp);
                }
                Tok::QDot => {
                    self.advance();
                    has_opt = true;
                    if self.at(&Tok::LParen) {
                        let args = self.parse_args()?;
                        e = mk(ExprKind::Call { callee: Box::new(e), targs: vec![], args, optional: true }, sp);
                    } else if self.at(&Tok::LBracket) {
                        self.advance();
                        let idx = self.parse_expr()?;
                        self.expect(&Tok::RBracket, "']'")?;
                        e = mk(ExprKind::Index { obj: Box::new(e), idx: Box::new(idx), optional: true }, sp);
                    } else {
                        let name = self.prop_name()?;
                        e = mk(ExprKind::Member { obj: Box::new(e), name, optional: true }, sp);
                    }
                }
                Tok::LBracket if !self.nl_before() => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    self.expect(&Tok::RBracket, "']'")?;
                    e = mk(ExprKind::Index { obj: Box::new(e), idx: Box::new(idx), optional: false }, sp);
                }
                Tok::LParen if !self.nl_before() => {
                    let args = self.parse_args()?;
                    e = mk(ExprKind::Call { callee: Box::new(e), targs: vec![], args, optional: false }, sp);
                }
                Tok::Lt => {
                    // explicit type arguments: f<T>(...)
                    let r = self.attempt(|p| {
                        let targs = p.parse_type_args()?;
                        if !p.at(&Tok::LParen) {
                            return Err(p.unexpected("'('"));
                        }
                        let args = p.parse_args()?;
                        Ok((targs, args))
                    });
                    match r {
                        Some((targs, args)) => {
                            e = mk(ExprKind::Call { callee: Box::new(e), targs, args, optional: false }, sp);
                        }
                        None => break,
                    }
                }
                Tok::Bang if !self.nl_before() => {
                    // non-null assertion, but not `!==`/`!=` (those are separate tokens)
                    self.advance();
                    e = mk(ExprKind::NonNull(Box::new(e)), sp);
                }
                Tok::PlusPlus | Tok::MinusMinus if !self.nl_before() => {
                    let inc = self.at(&Tok::PlusPlus);
                    self.check_update_target(&e)?;
                    self.advance();
                    e = mk(ExprKind::Update { inc, prefix: false, target: Box::new(e) }, sp);
                }
                _ => break,
            }
        }
        if has_opt {
            e = mk(ExprKind::Chain(Box::new(e)), sp);
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let sp = self.span();
        match self.tok().clone() {
            Tok::Num(n) => {
                self.advance();
                Ok(mk(ExprKind::Num(n), sp))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(mk(ExprKind::Str(s), sp))
            }
            Tok::Template(parts) => {
                self.advance();
                let mut segs = Vec::new();
                for p in parts {
                    match p {
                        TplPart::Str(s) => segs.push(TemplateSeg::Str(s)),
                        TplPart::Expr(src, start) => {
                            segs.push(TemplateSeg::Expr(parse_expr_source(&src, start, self.is_header)?))
                        }
                    }
                }
                Ok(mk(ExprKind::Template(segs), sp))
            }
            Tok::True => {
                self.advance();
                Ok(mk(ExprKind::Bool(true), sp))
            }
            Tok::False => {
                self.advance();
                Ok(mk(ExprKind::Bool(false), sp))
            }
            Tok::Null => {
                self.advance();
                Ok(mk(ExprKind::Null, sp))
            }
            Tok::LBracket => {
                self.advance();
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) {
                    if self.eat(&Tok::DotDotDot) {
                        items.push(ArrElem::Spread(self.parse_assign()?));
                    } else {
                        items.push(ArrElem::Item(self.parse_assign()?));
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket, "']'")?;
                Ok(mk(ExprKind::Array(items), sp))
            }
            Tok::LBrace => self.parse_object_literal(),
            Tok::Function => {
                let f = self.parse_function(false, vec![], false, false)?;
                Ok(mk(ExprKind::Func(Rc::new(f)), sp))
            }
            Tok::Async => {
                if matches!(self.tok_at(1), Tok::Function) {
                    self.advance();
                    let f = self.parse_function(true, vec![], false, false)?;
                    return Ok(mk(ExprKind::Func(Rc::new(f)), sp));
                }
                self.advance();
                if let Some((name, nsp)) = self.ident_name().map(|n| (n, self.span())) {
                    if *self.tok_at(1) == Tok::Arrow {
                        self.advance();
                        let params = vec![Param { pattern: Pattern::Ident(name, nsp), ty: None, optional: false, default: None, rest: false }];
                        return self.parse_arrow_rest(true, vec![], params, None, sp);
                    }
                }
                if let Some(e) = self.try_arrow(true) {
                    return Ok(e);
                }
                Err(Diag::new(sp, "expected arrow function after 'async'"))
            }
            Tok::Lt => match self.try_arrow(false) {
                Some(e) => Ok(e),
                None => Err(self.unexpected("expression")),
            },
            Tok::LParen => {
                if let Some(e) = self.try_arrow(false) {
                    return Ok(e);
                }
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen, "')'")?;
                Ok(e)
            }
            _ => {
                if let Some(name) = self.ident_name() {
                    if *self.tok_at(1) == Tok::Arrow {
                        let nsp = self.span();
                        self.advance();
                        let params = vec![Param { pattern: Pattern::Ident(name, nsp), ty: None, optional: false, default: None, rest: false }];
                        return self.parse_arrow_rest(false, vec![], params, None, sp);
                    }
                    self.advance();
                    return Ok(mk(ExprKind::Ident(name), sp));
                }
                Err(self.unexpected("expression"))
            }
        }
    }

    fn parse_object_literal(&mut self) -> PResult<Expr> {
        let sp = self.span();
        self.expect(&Tok::LBrace, "'{'")?;
        let mut props = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.eat(&Tok::DotDotDot) {
                props.push(ObjProp::Spread(self.parse_assign()?));
            } else {
                let key_sp = self.span();
                let is_ident = self.ident_name().is_some();
                let is_async_method = self.at(&Tok::Async) && !matches!(self.tok_at(1), Tok::Colon | Tok::Comma | Tok::RBrace | Tok::LParen);
                let is_async = if is_async_method {
                    self.advance();
                    true
                } else {
                    false
                };
                let key = self.prop_name()?;
                if self.eat(&Tok::Colon) {
                    let v = self.parse_assign()?;
                    props.push(ObjProp::KV(key, v));
                } else if self.at(&Tok::LParen) || self.at(&Tok::Lt) {
                    let tparams = self.parse_tparams()?;
                    let params = self.parse_params()?;
                    let ret = if self.eat(&Tok::Colon) { Some(self.parse_type()?) } else { None };
                    let body = self.parse_block_body()?;
                    let f = FuncDecl {
                        name: Some(key.clone()),
                        tparams,
                        params,
                        ret,
                        body,
                        is_async,
                        is_arrow: false,
                        decorators: vec![],
                        exported: false,
                        span: key_sp,
                        meta: Default::default(),
                    };
                    props.push(ObjProp::KV(key, mk(ExprKind::Func(Rc::new(f)), key_sp)));
                } else if is_ident {
                    props.push(ObjProp::KV(key.clone(), mk(ExprKind::Ident(key), key_sp)));
                } else {
                    return Err(self.unexpected("':'"));
                }
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(mk(ExprKind::Object(props), sp))
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("identifier '{}'", s),
        Tok::Num(n) => format!("number {}", n),
        Tok::Str(s) => format!("string \"{}\"", s),
        Tok::Template(_) => "template literal".into(),
        Tok::Eof => "end of file".into(),
        other => match keyword_text(other) {
            Some(k) => format!("'{}'", k),
            None => format!("'{}'", punct_text(other)),
        },
    }
}

fn keyword_text(t: &Tok) -> Option<&'static str> {
    Some(match t {
        Tok::Let => "let",
        Tok::Const => "const",
        Tok::Function => "function",
        Tok::Return => "return",
        Tok::If => "if",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::Do => "do",
        Tok::For => "for",
        Tok::Of => "of",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Null => "null",
        Tok::Import => "import",
        Tok::Export => "export",
        Tok::From => "from",
        Tok::As => "as",
        Tok::Type => "type",
        Tok::Interface => "interface",
        Tok::Declare => "declare",
        Tok::Async => "async",
        Tok::Await => "await",
        Tok::Try => "try",
        Tok::Catch => "catch",
        Tok::Finally => "finally",
        Tok::Throw => "throw",
        Tok::Typeof => "typeof",
        Tok::Switch => "switch",
        Tok::Case => "case",
        Tok::Default => "default",
        Tok::Extends => "extends",
        Tok::Test => "test",
        _ => return None,
    })
}

fn punct_text(t: &Tok) -> &'static str {
    match t {
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::Comma => ",",
        Tok::Semi => ";",
        Tok::Colon => ":",
        Tok::Dot => ".",
        Tok::DotDotDot => "...",
        Tok::Question => "?",
        Tok::QDot => "?.",
        Tok::At => "@",
        Tok::Arrow => "=>",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        Tok::Eq => "=",
        Tok::Lt => "<",
        Tok::Gt => ">",
        Tok::Bang => "!",
        _ => "token",
    }
}
