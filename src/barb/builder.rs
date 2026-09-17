//! Building well-shaped programs.
//!
//! Every operation returns an [`Atom`], and the `Let` that binds it is
//! written for you: A-normal form is the shape of the IR, not a burden on
//! whoever constructs it. A caller never names a local and never sees an
//! [`ExprId`].

use super::constant;
use super::ir::Symbol;
use super::ir::{Arm, Atom, Body, ConstId, Ctor, CtorId, Expr, ExprId, FnId, Function, Local};
use super::ir::{Program, Range, Type, TypeId};
use constant::{Shape, Value};

impl Program {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The symbol for `name`, the same symbol every time.
    ///
    /// A scan rather than a table beside it. This runs once per declaration
    /// and nowhere else: nothing here mints a name, since specialisation
    /// happens above this tier, so the cost is over what a program declares
    /// rather than over its terms. Reading one back is an index and a slice.
    ///
    /// # Panics
    ///
    /// If the program outgrew the names it may have.
    pub fn symbol(&mut self, name: &str) -> Symbol {
        let held = |at: &Range| &self.names[at.range()] == name.as_bytes();
        if let Some(at) = self.symbols.iter().position(held) {
            return Symbol::at(at);
        }
        let at = self.names.len();
        self.names.extend_from_slice(name.as_bytes());
        self.symbols.push(Range::of(at, name.len()));
        Symbol::at(self.symbols.len() - 1)
    }

    /// Reserve a type. Its constructors follow, so that a field may name the
    /// type being declared.
    pub fn declare_type(&mut self, name: &str) -> TypeId {
        let name = self.symbol(name);
        self.types.push(Type {
            name,
            ctors: Range::default(),
        });
        TypeId::at(self.types.len() - 1)
    }

    /// Give a reserved type its constructors, in tag order.
    ///
    /// # Panics
    ///
    /// If the program outgrew the pools it may have.
    pub fn define_type(&mut self, id: TypeId, ctors: &[(&str, &[TypeId])]) {
        let start = self.ctors.len();
        for (name, fields) in ctors {
            let name = self.symbol(name);
            let at = self.types_pool.len();
            self.types_pool.extend_from_slice(fields);
            self.ctors.push(Ctor {
                name,
                owner: id,
                fields: Range::of(at, fields.len()),
            });
        }
        self.types[id.index()].ctors = Range::of(start, ctors.len());
    }

    /// # Panics
    ///
    /// If the program outgrew the pools it may have.
    pub fn declare(&mut self, name: &str, params: &[TypeId], result: TypeId) -> FnId {
        let name = self.symbol(name);
        let at = self.types_pool.len();
        self.types_pool.extend_from_slice(params);
        self.functions.push(Function {
            name,
            params: Range::of(at, params.len()),
            result,
            // Replaced by `define`; a declared function has no body yet.
            body: Body {
                bindings: Range::default(),
                tail: ExprId(0),
            },
        });
        FnId::at(self.functions.len() - 1)
    }

    /// The function a target enters.
    pub const fn set_entry(&mut self, id: FnId) {
        self.entry = Some(id);
    }

    /// Encode a known value into the pool.
    ///
    /// # Panics
    ///
    /// If `value` is not of type `id`.
    pub fn intern(&mut self, id: TypeId, value: &Value) -> ConstId {
        self.hold(|program, out| {
            constant::encode(program, id, value, out);
        })
    }

    /// A number, written straight into the pool. The chain of constructors
    /// it stands for is never built, which for a large one is the difference
    /// between a few bytes and its own magnitude.
    ///
    /// # Panics
    ///
    /// If `id` is not a number: a spine whose elements carry nothing.
    pub fn intern_number(&mut self, id: TypeId, value: u64) -> ConstId {
        let name = self.type_(id).name;
        let Shape::Spine { cons, .. } = constant::shape(self, id) else {
            panic!("`{}` is not a number", self.name(name));
        };
        assert_eq!(
            self.fields(cons).len(),
            1,
            "`{}` carries something per element",
            self.name(name)
        );
        self.hold(|_, out| constant::write_number(value, out))
    }

    /// A run of bytes, likewise.
    ///
    /// # Panics
    ///
    /// If `id` is not a spine of numbers.
    pub fn intern_bytes(&mut self, id: TypeId, value: &[u8]) -> ConstId {
        let name = self.type_(id).name;
        let Shape::Spine { cons, .. } = constant::shape(self, id) else {
            panic!("`{}` is not a sequence", self.name(name));
        };
        let element = self
            .fields(cons)
            .iter()
            .find(|field| **field != id)
            .copied();
        assert!(
            element.is_some_and(|field| constant::as_number(self, field, &[0]).is_some()),
            "`{}` does not hold numbers",
            self.name(name)
        );
        self.hold(|_, out| constant::write_bytes(value, out))
    }

    /// Write into the pool itself, so that nothing is encoded into a buffer
    /// and then copied in.
    fn hold(&mut self, write: impl FnOnce(&Self, &mut Vec<u8>)) -> ConstId {
        let mut bytes = std::mem::take(&mut self.bytes);
        let start = bytes.len();
        write(self, &mut bytes);
        let len = bytes.len() - start;
        self.bytes = bytes;
        self.consts.push(Range::of(start, len));
        ConstId::at(self.consts.len() - 1)
    }

    /// Build a declared function's body. Its parameters are the first locals.
    ///
    /// # Panics
    ///
    /// If the function takes more parameters than the tier allows.
    pub fn define(&mut self, id: FnId, build: impl FnOnce(&mut Builder, &[Atom]) -> Atom) {
        let arity = u32::try_from(self.params(id).len()).expect("a sane arity");
        let mut builder = Builder {
            program: self,
            pending: Vec::new(),
            level: 0,
        };
        let body = builder.scope(arity, build);
        self.functions[id.index()].body = body;
    }
}

/// Builds one function's body.
///
/// Bindings accumulate in order and become nested `Let`s when the scope
/// closes, which is what lets an operation hand back an atom. Locals are de
/// Bruijn levels, so the level a binding takes is fixed the moment it is
/// emitted and no renaming is ever needed.
#[derive(Debug)]
pub struct Builder<'a> {
    program: &'a mut Program,
    pending: Vec<ExprId>,
    level: u32,
}

impl Builder<'_> {
    /// A saturated constructor application.
    pub fn con(&mut self, ctor: CtorId, args: &[Atom]) -> Atom {
        let args = self.atoms(args);
        self.emit(Expr::Con(ctor, args))
    }

    /// A saturated call to a top-level name.
    pub fn call(&mut self, function: FnId, args: &[Atom]) -> Atom {
        let args = self.atoms(args);
        self.emit(Expr::Call(function, args))
    }

    /// A value known outright.
    pub fn known(&mut self, id: TypeId, value: ConstId) -> Atom {
        self.emit(Expr::Static(id, value))
    }

    /// One arm per constructor of `owner`, in tag order, so a match is
    /// exhaustive by construction. Each arm binds its constructor's fields
    /// as the next locals; sibling arms take the same ones, being
    /// alternatives rather than a sequence.
    ///
    /// # Panics
    ///
    /// If the type outgrew the constructors it may have.
    pub fn match_(
        &mut self,
        scrutinee: Atom,
        owner: TypeId,
        mut arm: impl FnMut(&mut Self, CtorId, &[Atom]) -> Atom,
    ) -> Atom {
        let count =
            u32::try_from(self.program.type_(owner).ctors.len()).expect("a sane constructor count");
        let mut arms =
            Vec::with_capacity(usize::try_from(count).expect("a count that fits a pointer"));
        for tag in 0..count {
            let ctor = self.program.ctor_at(owner, tag);
            let fields =
                u32::try_from(self.program.fields(ctor).len()).expect("a sane constructor arity");
            let body = self.scope(fields, |builder, bound| arm(builder, ctor, bound));
            arms.push(Arm { ctor, body });
        }
        let at = self.program.arms.len();
        self.program.arms.extend_from_slice(&arms);
        let arms = Range::of(at, arms.len());
        self.emit(Expr::Match(scrutinee, arms))
    }

    /// Bind an expression to the next local and answer it.
    fn emit(&mut self, expr: Expr) -> Atom {
        let id = self.push(expr);
        self.pending.push(id);
        let bound = Atom(Local(self.level));
        self.level += 1;
        bound
    }

    /// Build a nested scope that first binds `bound` locals of its own.
    fn scope(&mut self, bound: u32, build: impl FnOnce(&mut Self, &[Atom]) -> Atom) -> Body {
        let outer = std::mem::take(&mut self.pending);
        let level = self.level;
        let binders: Vec<Atom> = (0..bound).map(|at| Atom(Local(level + at))).collect();
        self.level = level + bound;

        let result = build(self, &binders);
        let body = self.close(result);

        self.pending = outer;
        self.level = level;
        body
    }

    /// This scope's bindings, with its result as the tail. A result that is
    /// the last binding becomes the tail itself rather than a binding an
    /// atom then reads back, since nothing after it could have read it.
    fn close(&mut self, result: Atom) -> Body {
        let mut pending = std::mem::take(&mut self.pending);
        let last = self.level.checked_sub(1).map(Local);
        let tail = match pending.pop() {
            Some(expr) if last == Some(result.0) => expr,
            Some(expr) => {
                pending.push(expr);
                self.push(Expr::Atom(result))
            }
            None => self.push(Expr::Atom(result)),
        };
        let at = self.program.bindings.len();
        self.program.bindings.extend_from_slice(&pending);
        Body {
            bindings: Range::of(at, pending.len()),
            tail,
        }
    }

    fn push(&mut self, expr: Expr) -> ExprId {
        self.program.exprs.push(expr);
        ExprId::at(self.program.exprs.len() - 1)
    }

    fn atoms(&mut self, atoms: &[Atom]) -> Range {
        let at = self.program.atoms.len();
        self.program.atoms.extend_from_slice(atoms);
        Range::of(at, atoms.len())
    }
}
