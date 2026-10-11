//! Statement translation.

use super::*;

impl Tr<'_> {
    pub fn stmt(&mut self, s: &Stmt) -> R<()> {
        match s {
            Stmt::Empty => Ok(()),
            Stmt::Block(v) => {
                self.open("{");
                self.push_scope();
                for x in v {
                    self.stmt(x)?;
                }
                self.pop_scope();
                self.close();
                Ok(())
            }
            Stmt::Decl(ds) => {
                for d in ds {
                    if matches!(d.ty, Ty::Sampler(_) | Ty::Texture) {
                        return Err(TranslateError::Unsupported(
                            "local sampler declarations".into(),
                        ));
                    }
                    let init = match &d.init {
                        Some(e) => Some(self.init_value(&d.ty, e)?),
                        None => None,
                    };
                    let w = self.declare(&d.name, &d.ty);
                    match init {
                        Some(i) => self.line(format!("var {w}: {} = {i};", wgsl_ty(&d.ty))),
                        None if self.cx.is_ps && self.undef_nan.contains(&d.name) => {
                            let nan = self.nan_value(&d.ty);
                            self.line(format!("var {w}: {} = {nan};", wgsl_ty(&d.ty)));
                        }
                        None => self.line(format!("var {w}: {};", wgsl_ty(&d.ty))),
                    }
                }
                Ok(())
            }
            Stmt::Expr(e) => self.expr_stmt(e),
            Stmt::If(c, a, b) => {
                let cv = self.expr(c)?;
                let cs = self.to_bool(&cv)?;
                self.open(format!("if ({cs}) {{"));
                self.block_body(a)?;
                match b {
                    Some(b) => {
                        self.close_with("} else {");
                        self.cx.indent += 1;
                        self.block_body(b)?;
                        self.close();
                    }
                    None => self.close(),
                }
                Ok(())
            }
            Stmt::For(init, cond, step, body) => {
                self.open("{");
                self.push_scope();
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                self.open("loop {");
                if let Some(c) = cond {
                    let cv = self.expr(c)?;
                    let cs = self.to_bool(&cv)?;
                    self.line(format!("if (!{}) {{ break; }}", paren_s(&cs)));
                }
                self.open("{");
                self.push_scope();
                self.stmt(body)?;
                self.pop_scope();
                self.close();
                if let Some(st) = step {
                    self.open("continuing {");
                    self.expr_stmt(st)?;
                    self.close();
                }
                self.close();
                self.pop_scope();
                self.close();
                Ok(())
            }
            Stmt::While(c, body) => {
                self.open("loop {");
                let cv = self.expr(c)?;
                let cs = self.to_bool(&cv)?;
                self.line(format!("if (!{}) {{ break; }}", paren_s(&cs)));
                self.open("{");
                self.push_scope();
                self.stmt(body)?;
                self.pop_scope();
                self.close();
                self.close();
                Ok(())
            }
            Stmt::DoWhile(body, c) => {
                self.open("loop {");
                self.open("{");
                self.push_scope();
                self.stmt(body)?;
                self.pop_scope();
                self.close();
                self.open("continuing {");
                let cv = self.expr(c)?;
                let cs = self.to_bool(&cv)?;
                self.line(format!("break if !{};", paren_s(&cs)));
                self.close();
                self.close();
                Ok(())
            }
            Stmt::Return(e) => {
                let ret = self.cx.ret.clone().unwrap_or(Ty::Void);
                match (e, &ret) {
                    (None, Ty::Void) => self.line("return;"),
                    (Some(e), Ty::Void) => {
                        // `return f();` in a void function
                        self.expr_stmt(e)?;
                        self.line("return;");
                    }
                    (Some(e), t) => {
                        let v = self.expr(e)?;
                        let s = self.conv(&v, t)?;
                        self.line(format!("return {s};"));
                    }
                    (None, t) => {
                        let z = zero_value(t);
                        self.line(format!("return {z};"));
                    }
                }
                Ok(())
            }
            Stmt::Break => {
                self.line("break;");
                Ok(())
            }
            Stmt::Continue => {
                self.line("continue;");
                Ok(())
            }
            Stmt::Discard => {
                self.line("discard;");
                Ok(())
            }
        }
    }

    /// Body of an `if` branch: contents without an extra `{ }` pair (already opened).
    fn block_body(&mut self, s: &Stmt) -> R<()> {
        self.push_scope();
        match s {
            Stmt::Block(v) => {
                for x in v {
                    self.stmt(x)?;
                }
            }
            other => self.stmt(other)?,
        }
        self.pop_scope();
        Ok(())
    }
}

fn paren_s(s: &str) -> String {
    if s.starts_with('(') && s.ends_with(')') {
        s.to_string()
    } else {
        format!("({s})")
    }
}
