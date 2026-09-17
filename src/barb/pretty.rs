//! Rendering a program as text, so that one can be read.

use std::fmt::Write;

use super::constant::{Shape, as_bytes, as_number, shape};
use super::ir::{Expr, FnId, Program, Symbol, TypeId};

impl Program {
    /// The whole program: its types, then its functions.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for id in 0..self.types().len() {
            self.render_type(TypeId::at(id), &mut out);
        }
        for id in 0..self.functions().len() {
            out.push('\n');
            self.render_function(FnId::at(id), &mut out);
        }
        out
    }

    fn render_type(&self, id: TypeId, out: &mut String) {
        let ctors: Vec<String> = self
            .ctors(id)
            .iter()
            .map(|ctor| {
                let fields: Vec<&str> = self
                    .types_of(ctor.fields)
                    .iter()
                    .map(|field| self.name(self.type_(*field).name))
                    .collect();
                if fields.is_empty() {
                    self.name(ctor.name).to_owned()
                } else {
                    format!("{}({})", self.name(ctor.name), fields.join(", "))
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "{} = {}",
            self.name(self.type_(id).name),
            ctors.join(" | ")
        );
    }

    fn render_function(&self, id: FnId, out: &mut String) {
        let function = self.function(id);
        let params: Vec<String> = self
            .params(id)
            .iter()
            .enumerate()
            .map(|(at, type_)| format!("v{at}: {}", self.name(self.type_(*type_).name)))
            .collect();
        let _ = writeln!(
            out,
            "{}({}) -> {} =",
            self.name(function.name),
            params.join(", "),
            self.name(self.type_(function.result).name)
        );
        let level = u32::try_from(self.params(id).len()).expect("a sane arity");
        self.render_expr(function.body, level, 1, out);
    }

    fn render_expr(&self, expr: super::ir::ExprId, level: u32, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        match self.expr(expr) {
            Expr::Atom(atom) => {
                let _ = writeln!(out, "{pad}v{}", atom.0.0);
            }
            Expr::Con(ctor, args) => {
                let _ = writeln!(out, "{pad}{}", self.render_call(self.ctor(ctor).name, args));
            }
            Expr::Call(function, args) => {
                let name = self.function(function).name;
                let _ = writeln!(out, "{pad}{}", self.render_call(name, args));
            }
            Expr::Static(id, value) => {
                let _ = writeln!(out, "{pad}{}", self.render_known(id, value));
            }
            Expr::Let(value, body) => {
                let mut bound = String::new();
                self.render_expr(value, level, 0, &mut bound);
                let _ = writeln!(out, "{pad}let v{level} = {}", bound.trim());
                self.render_expr(body, level + 1, depth, out);
            }
            Expr::Match(scrutinee, arms) => {
                let _ = writeln!(out, "{pad}match v{} {{", scrutinee.0.0);
                for arm in self.arms(arms).to_vec() {
                    let fields = self.types_of(self.ctor(arm.ctor).fields).len();
                    let bound: Vec<String> = (0..fields)
                        .map(|at| format!("v{}", level + u32::try_from(at).expect("a sane arity")))
                        .collect();
                    let name = self.name(self.ctor(arm.ctor).name);
                    let _ = if bound.is_empty() {
                        writeln!(out, "{pad}  {name} =>")
                    } else {
                        writeln!(out, "{pad}  {name}({}) =>", bound.join(", "))
                    };
                    let inner = level + u32::try_from(fields).expect("a sane arity");
                    self.render_expr(arm.body, inner, depth + 2, out);
                }
                let _ = writeln!(out, "{pad}}}");
            }
        }
    }

    fn render_call(&self, name: Symbol, args: super::ir::Range) -> String {
        let args: Vec<String> = self
            .atoms(args)
            .iter()
            .map(|atom| format!("v{}", atom.0.0))
            .collect();
        if args.is_empty() {
            self.name(name).to_owned()
        } else {
            format!("{}({})", self.name(name), args.join(", "))
        }
    }

    /// A known value as what it stands for, not as its bytes.
    fn render_known(&self, id: TypeId, value: super::ir::ConstId) -> String {
        let bytes = self.known_bytes(value);
        if let Some(number) = as_number(self, id, bytes) {
            return number.to_string();
        }
        if let Some(run) = as_bytes(self, id, bytes)
            && let Ok(text) = std::str::from_utf8(run)
        {
            return format!("{text:?}");
        }
        match shape(self, id) {
            Shape::Spine { .. } => format!("<{} bytes>", bytes.len()),
            Shape::Tagged => format!("<{}>", self.name(self.type_(id).name)),
        }
    }
}
