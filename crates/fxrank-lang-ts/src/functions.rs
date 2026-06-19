//! Collect function units from a parsed swc `Module`.
//!
//! A "function unit" is any node with a concrete body that detectors can walk:
//! - Function declarations (`function foo() {}`)
//! - Function expressions (`const x = function () {}`)
//! - Arrow functions (`const f = () => {}`, `[1].map(x => x)`)
//! - Class methods, getters, and setters (`class C { m() {} get g() {} }`)
//! - Object-literal methods, getters, and setters (`{ m() {}, get g() {} }`)
//!
//! This is the swc analog of `fxrank-lang-rust`'s `functions::collect`. Each
//! [`FnUnit`] owns the data later detector tasks need (signature + body), so the
//! `SourceMap` and `Module` can be dropped after collection. Because swc's
//! `Function` (params: `Vec<Param>`, body: `Option<BlockStmt>`) and `ArrowExpr`
//! (params: `Vec<Pat>`, body: `BlockStmtOrExpr`) differ, we normalize both into
//! a small owned representation: [`FnSig`] (params as `Vec<Pat>` + return
//! annotation) and [`FnBodyOwned`] (a statement block *or* a bare expression).
//!
//! **Nested functions are their own units.** The collector recurses via
//! `visit_children_with`, so a function defined inside another function yields a
//! separate `FnUnit`; child effects are never rolled into the parent.
//!
//! **Symbol naming.** Declarations and class/object members use their own name
//! (`foo`, `C.method`, `C.get g`, `C.set g`, `C.constructor`). Arrows take the
//! binding name when assigned directly to a `const`/`let`/`var` declarator
//! (`const f = () => {}` -> `f`); otherwise they fall back to `<arrow@L{line}>`
//! (inline callbacks such as `[1].map(x => x)`). Anonymous function expressions
//! use `<fn@L{line}>` as their positional fallback.

use swc_ecma_ast::{
    ArrowExpr, BlockStmtOrExpr, Class, ClassMethod, Constructor, Decl, Expr, FnDecl, FnExpr,
    Function, GetterProp, MethodKind, MethodProp, ParamOrTsParamProp, Pat, PrivateMethod, PropName,
    SetterProp, Stmt, TsTypeAnn, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::source::SpanLines;

/// A normalized, owned function signature.
///
/// `params` is the parameter pattern list with both forms unified: a
/// `Function`'s `Vec<Param>` is mapped to each `Param.pat`, and an `ArrowExpr`'s
/// `Vec<Pat>` is taken as-is. `return_type` is the optional TS return annotation
/// (used by coverage and mutation-seeding tasks). Setters expose their single
/// parameter through `params` too.
#[derive(Clone)]
pub struct FnSig {
    /// Parameter patterns (normalized across function forms).
    pub params: Vec<Pat>,
    /// The TypeScript return-type annotation, if present.
    pub return_type: Option<TsTypeAnn>,
}

/// The owned body of a function unit.
///
/// A block carries its statements; an arrow with an expression body carries that
/// single expression. Detectors walk whichever variant is present.
#[derive(Clone)]
pub enum FnBodyOwned {
    /// A `{ ... }` block (function declarations, methods, block-bodied arrows).
    /// `None`-bodied functions (ambient/overload signatures) yield an empty Vec.
    Block(Vec<Stmt>),
    /// A bare expression body, e.g. the `x` in `x => x`.
    Expr(Box<Expr>),
}

/// A concrete function unit — a named (or positionally-named) node with a body
/// that can be analysed for effects. `sig` and `body` are owned clones so
/// detectors can walk them after the source `Module` is dropped.
pub struct FnUnit {
    /// Display symbol: `foo`, `f`, `C.method`, `C.get g`, `C.set g`,
    /// `C.constructor`, or `<arrow@L{line}>`.
    pub symbol: String,
    /// Collision-resistant id: `path:line:symbol`.
    pub id: String,
    /// Source file path (as passed in).
    pub path: String,
    /// 1-based line number of the function's name / node span.
    pub line: usize,
    /// Whether this function is `async`.
    pub is_async: bool,
    /// Whether this is a class constructor. Task 8's mutation detector uses
    /// this to distinguish contained `this.x = …` (constructor — local init)
    /// from escaping `this.x = …` (normal method — `this.mutation`).
    pub is_constructor: bool,
    /// Normalized signature (params + return annotation).
    pub sig: FnSig,
    /// Owned body for detectors to walk.
    pub body: FnBodyOwned,
}

/// Parse `src` as `lang` and collect its function units.
///
/// A convenience entry point that owns the swc parse plumbing (build a
/// `SourceMap`, lex, parse a module, resolve lines) and returns the units plus
/// any parse error. Detector and integration tests use this so they don't have
/// to repeat the parser setup.
pub fn parse_and_collect(
    src: &str,
    path: &str,
    lang: crate::source::Lang,
) -> Result<Vec<FnUnit>, swc_ecma_parser::error::Error> {
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, lexer::Lexer};

    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(FileName::Custom(path.into()).into(), src.to_string());
    let lexer = Lexer::new(
        lang.syntax(),
        Default::default(),
        StringInput::from(&*fm),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let module = parser.parse_module()?;
    let lines = SpanLines::new(cm);
    Ok(collect(&module, path, &lines))
}

/// Collect all function units from a parsed module at `path`.
///
/// `lines` resolves spans to 1-based line numbers (built from the same
/// `SourceMap` that parsed the module).
pub fn collect(module: &swc_ecma_ast::Module, path: &str, lines: &SpanLines) -> Vec<FnUnit> {
    let mut collector = Collector {
        path,
        lines,
        class_name: None,
        pending_name: None,
        units: Vec::new(),
    };
    module.visit_with(&mut collector);
    collector.units
}

struct Collector<'a> {
    path: &'a str,
    lines: &'a SpanLines,
    /// Name of the enclosing class, threaded while walking class members.
    class_name: Option<String>,
    /// Binding name for an arrow/fn-expr that is the direct `init` of the
    /// declarator currently being walked. Consumed by the next
    /// `visit_arrow_expr` / `visit_fn_expr`.
    pending_name: Option<String>,
    units: Vec<FnUnit>,
}

impl Collector<'_> {
    fn push(
        &mut self,
        symbol: String,
        line: usize,
        is_async: bool,
        is_constructor: bool,
        sig: FnSig,
        body: FnBodyOwned,
    ) {
        let id = format!("{}:{}:{}", self.path, line, symbol);
        self.units.push(FnUnit {
            symbol,
            id,
            path: self.path.to_string(),
            line,
            is_async,
            is_constructor,
            sig,
            body,
        });
    }
}

/// Map a `Function`'s `Vec<Param>` to a `Vec<Pat>`.
fn params_of_function(f: &Function) -> Vec<Pat> {
    f.params.iter().map(|p| p.pat.clone()).collect()
}

/// Build a `FnBodyOwned` from a `Function` body (`None` -> empty block).
fn body_of_function(f: &Function) -> FnBodyOwned {
    match &f.body {
        Some(block) => FnBodyOwned::Block(block.stmts.clone()),
        None => FnBodyOwned::Block(Vec::new()),
    }
}

/// Unwrap an optional boxed `TsTypeAnn` into an owned clone.
fn return_type_of(rt: &Option<Box<TsTypeAnn>>) -> Option<TsTypeAnn> {
    rt.as_ref().map(|t| (**t).clone())
}

/// Extract a method/property name from a `PropName`. Computed keys (`[expr]`)
/// fall back to `"<computed>"`.
fn prop_name(key: &PropName) -> String {
    match key {
        PropName::Ident(i) => i.sym.to_string(),
        // `Wtf8Atom` has no `Display`; `to_atom_lossy()` produces a UTF-8 `Atom`
        // (borrowing if already valid UTF-8, reallocating only for lone surrogates).
        PropName::Str(s) => s.value.to_atom_lossy().to_string(),
        PropName::Num(n) => n.value.to_string(),
        PropName::BigInt(b) => b.value.to_string(),
        PropName::Computed(_) => "<computed>".to_string(),
    }
}

impl Visit for Collector<'_> {
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let symbol = node.ident.sym.to_string();
        let line = self.lines.line(node.ident.span);
        let f = &node.function;
        let sig = FnSig {
            params: params_of_function(f),
            return_type: return_type_of(&f.return_type),
        };
        self.push(symbol, line, f.is_async, false, sig, body_of_function(f));
        // Recurse into the body so nested functions become their own units.
        node.visit_children_with(self);
    }

    fn visit_fn_expr(&mut self, node: &FnExpr) {
        let f = &node.function;
        // Prefer the function expression's own name, then the binding name, then
        // a positional fallback.
        let line = self.lines.line(f.span);
        let symbol = node
            .ident
            .as_ref()
            .map(|i| i.sym.to_string())
            .or_else(|| self.pending_name.take())
            .unwrap_or_else(|| format!("<fn@L{line}>"));
        let sig = FnSig {
            params: params_of_function(f),
            return_type: return_type_of(&f.return_type),
        };
        self.push(symbol, line, f.is_async, false, sig, body_of_function(f));
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        let line = self.lines.line(node.span);
        let symbol = self
            .pending_name
            .take()
            .unwrap_or_else(|| format!("<arrow@L{line}>"));
        let sig = FnSig {
            params: node.params.clone(),
            return_type: return_type_of(&node.return_type),
        };
        let body = match &*node.body {
            BlockStmtOrExpr::BlockStmt(block) => FnBodyOwned::Block(block.stmts.clone()),
            BlockStmtOrExpr::Expr(expr) => FnBodyOwned::Expr(expr.clone()),
        };
        self.push(symbol, line, node.is_async, false, sig, body);
        node.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // When `const f = () => {}` / `const f = function () {}`, hand the
        // binding name to the arrow/fn-expr we're about to walk.
        let name = match &node.name {
            Pat::Ident(b) => Some(b.id.sym.to_string()),
            _ => None,
        };
        let directly_callable = matches!(
            node.init.as_deref(),
            Some(Expr::Arrow(_)) | Some(Expr::Fn(_))
        );
        if directly_callable {
            self.pending_name = name;
        }
        node.visit_children_with(self);
        // Clear in case it wasn't consumed (defensive; the matched child always
        // consumes it via `.take()`).
        self.pending_name = None;
    }

    fn visit_class(&mut self, node: &Class) {
        // The class name is set by `visit_class_decl` / `visit_class_expr`
        // before delegating here; default to "<class>" for anonymous classes.
        let class = self
            .class_name
            .clone()
            .unwrap_or_else(|| "<class>".to_string());

        // Class methods are collected here — not via an overridden
        // `visit_class_method` — because the `Visit` trait gives
        // `visit_class_method` no class-name context. The subsequent
        // `node.visit_children_with(self)` recurses into member BODIES (to
        // catch nested arrows/fn-exprs inside method bodies) and does NOT
        // re-emit the method units: the default `visit_class_method` never
        // calls `visit_fn_decl`/`visit_fn_expr`, so there is no double-emit.
        for member in &node.body {
            match member {
                swc_ecma_ast::ClassMember::Constructor(c) => self.collect_constructor(&class, c),
                swc_ecma_ast::ClassMember::Method(m) => self.collect_class_method(&class, m),
                swc_ecma_ast::ClassMember::PrivateMethod(m) => {
                    self.collect_private_method(&class, m)
                }
                _ => {}
            }
        }

        // Walk children (member bodies, computed keys, nested classes) so nested
        // functions are collected. We clear `class_name` so it doesn't leak into
        // unrelated nested scopes; nested classes re-establish it themselves.
        let saved = self.class_name.take();
        node.visit_children_with(self);
        self.class_name = saved;
    }

    fn visit_class_decl(&mut self, node: &swc_ecma_ast::ClassDecl) {
        let saved = self.class_name.replace(node.ident.sym.to_string());
        node.class.visit_with(self);
        self.class_name = saved;
    }

    fn visit_class_expr(&mut self, node: &swc_ecma_ast::ClassExpr) {
        let name = node
            .ident
            .as_ref()
            .map(|i| i.sym.to_string())
            .or_else(|| self.pending_name.take());
        let saved = std::mem::replace(&mut self.class_name, name);
        node.class.visit_with(self);
        self.class_name = saved;
    }

    fn visit_decl(&mut self, node: &Decl) {
        // Default impl, retained explicitly so the dispatch above is visible.
        node.visit_children_with(self);
    }

    fn visit_method_prop(&mut self, node: &MethodProp) {
        let f = &node.function;
        let line = self.lines.line(f.span);
        let symbol = prop_name(&node.key);
        let sig = FnSig {
            params: params_of_function(f),
            return_type: return_type_of(&f.return_type),
        };
        self.push(symbol, line, f.is_async, false, sig, body_of_function(f));
        node.visit_children_with(self);
    }

    fn visit_getter_prop(&mut self, node: &GetterProp) {
        let line = self.lines.line(node.span);
        let symbol = format!("get {}", prop_name(&node.key));
        let sig = FnSig {
            params: Vec::new(),
            return_type: return_type_of(&node.type_ann),
        };
        let body = match &node.body {
            Some(block) => FnBodyOwned::Block(block.stmts.clone()),
            None => FnBodyOwned::Block(Vec::new()),
        };
        self.push(symbol, line, false, false, sig, body);
        node.visit_children_with(self);
    }

    fn visit_setter_prop(&mut self, node: &SetterProp) {
        let line = self.lines.line(node.span);
        let symbol = format!("set {}", prop_name(&node.key));
        let sig = FnSig {
            params: vec![(*node.param).clone()],
            return_type: None,
        };
        let body = match &node.body {
            Some(block) => FnBodyOwned::Block(block.stmts.clone()),
            None => FnBodyOwned::Block(Vec::new()),
        };
        self.push(symbol, line, false, false, sig, body);
        node.visit_children_with(self);
    }
}

impl Collector<'_> {
    fn collect_class_method(&mut self, class: &str, m: &ClassMethod) {
        let f = &m.function;
        let line = self.lines.line(f.span);
        let name = prop_name(&m.key);
        let symbol = match m.kind {
            MethodKind::Method => format!("{class}.{name}"),
            MethodKind::Getter => format!("{class}.get {name}"),
            MethodKind::Setter => format!("{class}.set {name}"),
        };
        let sig = FnSig {
            params: params_of_function(f),
            return_type: return_type_of(&f.return_type),
        };
        self.push(symbol, line, f.is_async, false, sig, body_of_function(f));
    }

    fn collect_private_method(&mut self, class: &str, m: &PrivateMethod) {
        let f = &m.function;
        let line = self.lines.line(f.span);
        let symbol = format!("{class}.#{}", m.key.name);
        let sig = FnSig {
            params: params_of_function(f),
            return_type: return_type_of(&f.return_type),
        };
        self.push(symbol, line, f.is_async, false, sig, body_of_function(f));
    }

    fn collect_constructor(&mut self, class: &str, c: &Constructor) {
        let line = self.lines.line(c.span);
        let symbol = format!("{class}.constructor");
        // Constructor params may include TS parameter properties (`public x: T`);
        // extract the underlying `Pat` from each variant.
        let params: Vec<Pat> = c
            .params
            .iter()
            .map(|p| match p {
                ParamOrTsParamProp::Param(param) => param.pat.clone(),
                ParamOrTsParamProp::TsParamProp(ts) => match &ts.param {
                    swc_ecma_ast::TsParamPropParam::Ident(b) => Pat::Ident(b.clone()),
                    swc_ecma_ast::TsParamPropParam::Assign(a) => Pat::Assign(a.clone()),
                },
            })
            .collect();
        let sig = FnSig {
            params,
            return_type: None,
        };
        let body = match &c.body {
            Some(block) => FnBodyOwned::Block(block.stmts.clone()),
            None => FnBodyOwned::Block(Vec::new()),
        };
        self.push(symbol, line, false, true, sig, body);
    }
}
