//! The data model: terms, declarations, and the pools they live in.

use crate::index::index;

// `Symbol` is a name. The IR never reads the characters of one: names are
// compared, hashed and copied as integers, and the string is read back only
// to render it. The table belongs to the program, like every other pool
// here, so a program is a self-contained value and two of them cannot
// disagree about what a symbol means.
index!(ExprId, FnId, TypeId, CtorId, ConstId, Local, Symbol);

/// A run of values in one of the pools beside the arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub(super) start: u32,
    pub(super) len: u32,
}

impl Range {
    /// # Panics
    ///
    /// Never, on any machine whose pointers reach 32 bits.
    #[must_use]
    pub fn len(self) -> usize {
        usize::try_from(self.len).expect("a length that fits a pointer")
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub(super) fn of(start: usize, len: usize) -> Self {
        Self {
            start: u32::try_from(start).expect("a pool within 4G"),
            len: u32::try_from(len).expect("a run within 4G"),
        }
    }

    pub(super) fn range(self) -> std::ops::Range<usize> {
        let start = usize::try_from(self.start).expect("an index that fits a pointer");
        start..start + self.len()
    }
}

/// An argument: a local, and nothing else. That is what A-normal form buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Atom(pub Local);

/// A term.
///
/// Binders are implicit. Locals are de Bruijn levels: a function's parameters
/// are the first of them, a [`Expr::Let`] binds the next, and a [`Arm`] binds
/// its constructor's fields in order. Two sibling arms reuse the same levels
/// because they are alternative paths, so a term carries no names and two
/// structurally equal terms are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expr {
    /// A bare atom.
    Atom(Atom),
    /// A saturated constructor application.
    Con(CtorId, Range),
    /// A value known outright, held encoded rather than as a chain of `Con`.
    Static(TypeId, ConstId),
    /// A saturated call to a top-level name. Every recursive edge is one.
    Call(FnId, Range),
    /// Binds the next local to the first, and continues with the second.
    Let(ExprId, ExprId),
    /// Destructuring. An arm per constructor of the scrutinee's type.
    Match(Atom, Range),
}

/// One branch of a match. Binds its constructor's fields as the next locals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arm {
    pub ctor: CtorId,
    pub body: ExprId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Type {
    pub name: Symbol,
    pub ctors: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ctor {
    pub name: Symbol,
    pub owner: TypeId,
    /// The types of its fields, in order.
    pub fields: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: Symbol,
    /// The types of its parameters, which are the first locals.
    pub params: Range,
    pub result: TypeId,
    pub body: ExprId,
}

/// A program, and the pools every part of it lives in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub(super) types: Vec<Type>,
    pub(super) ctors: Vec<Ctor>,
    pub(super) functions: Vec<Function>,
    /// The term arena.
    pub(super) exprs: Vec<Expr>,
    /// Argument lists, for `Con` and `Call`.
    pub(super) atoms: Vec<Atom>,
    pub(super) arms: Vec<Arm>,
    /// Field and parameter types.
    pub(super) types_pool: Vec<TypeId>,
    /// Known values, encoded. See [`super::constant`].
    pub(super) consts: Vec<Range>,
    pub(super) bytes: Vec<u8>,
    /// Every name the program uses, each once, end to end. A [`Symbol`] is
    /// an index into `symbols`, which says where in here its characters are,
    /// so naming something allocates nothing.
    pub(super) names: Vec<u8>,
    pub(super) symbols: Vec<Range>,
    pub(super) entry: Option<FnId>,
}

impl Program {
    /// The name a symbol stands for.
    ///
    /// # Panics
    ///
    /// If the symbol came from another program.
    #[must_use]
    pub fn name(&self, symbol: Symbol) -> &str {
        let at = self.symbols[symbol.index()];
        std::str::from_utf8(&self.names[at.range()]).expect("a name was written as text")
    }

    /// Every name the program uses, end to end. For tests and rendering.
    ///
    /// # Panics
    ///
    /// Never: names are written as text.
    #[must_use]
    pub fn names_text(&self) -> &str {
        std::str::from_utf8(&self.names).expect("names were written as text")
    }

    /// How many locals a body binds, parameters included: the deepest chain
    /// of binders in it, since sibling arms take the same levels back and a
    /// `let`'s value is bound outside its own binding.
    ///
    /// # Panics
    ///
    /// If a constructor outgrew the fields it may have.
    #[must_use]
    pub fn locals(&self, id: FnId) -> u32 {
        fn deepest(program: &Program, expr: ExprId, at: u32) -> u32 {
            match program.expr(expr) {
                Expr::Let(value, body) => {
                    deepest(program, value, at).max(deepest(program, body, at + 1))
                }
                Expr::Match(_, arms) => program
                    .arms(arms)
                    .iter()
                    .map(|arm| {
                        let bound = u32::try_from(program.fields(arm.ctor).len())
                            .expect("a sane constructor arity");
                        deepest(program, arm.body, at + bound)
                    })
                    .max()
                    .unwrap_or(at),
                _ => at,
            }
        }
        let params = u32::try_from(self.params(id).len()).expect("a sane arity");
        deepest(self, self.function(id).body, params)
    }

    #[must_use]
    pub fn type_(&self, id: TypeId) -> &Type {
        &self.types[id.index()]
    }

    #[must_use]
    pub fn ctor(&self, id: CtorId) -> &Ctor {
        &self.ctors[id.index()]
    }

    #[must_use]
    pub fn function(&self, id: FnId) -> &Function {
        &self.functions[id.index()]
    }

    #[must_use]
    pub fn expr(&self, id: ExprId) -> Expr {
        self.exprs[id.index()]
    }

    #[must_use]
    pub fn ctors(&self, id: TypeId) -> &[Ctor] {
        &self.ctors[self.type_(id).ctors.range()]
    }

    /// Where `ctor` sits among its type's constructors, which is the tag a
    /// known value carries.
    #[must_use]
    pub fn tag(&self, ctor: CtorId) -> u32 {
        ctor.0 - self.type_(self.ctor(ctor).owner).ctors.start
    }

    #[must_use]
    pub fn ctor_at(&self, owner: TypeId, tag: u32) -> CtorId {
        CtorId(self.type_(owner).ctors.start + tag)
    }

    /// The types a range of the pool holds.
    #[must_use]
    pub fn types_of(&self, range: Range) -> &[TypeId] {
        &self.types_pool[range.range()]
    }

    #[must_use]
    pub fn fields(&self, ctor: CtorId) -> &[TypeId] {
        &self.types_pool[self.ctor(ctor).fields.range()]
    }

    #[must_use]
    pub fn params(&self, id: FnId) -> &[TypeId] {
        &self.types_pool[self.function(id).params.range()]
    }

    #[must_use]
    pub fn atoms(&self, range: Range) -> &[Atom] {
        &self.atoms[range.range()]
    }

    #[must_use]
    pub fn arms(&self, range: Range) -> &[Arm] {
        &self.arms[range.range()]
    }

    #[must_use]
    pub fn known_bytes(&self, id: ConstId) -> &[u8] {
        &self.bytes[self.consts[id.index()].range()]
    }

    #[must_use]
    pub const fn entry(&self) -> Option<FnId> {
        self.entry
    }

    #[must_use]
    pub fn types(&self) -> &[Type] {
        &self.types
    }

    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }
}
