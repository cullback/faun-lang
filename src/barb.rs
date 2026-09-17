//! barb: the first-order core IR.
//!
//! It decides:
//!
//! - The inductive types a program is built from, and their constructors
//! - Which operations run, over which values, in what order
//! - Totality: that every function is defined for every input and stops
//!
//! It does not decide:
//!
//! - How a value is represented, how wide it is, or where it is laid out
//! - Where a value is retained, released, or reused in place
//! - Anything a target names
//!
//! Every value is an inductive term, and the term language is in A-normal
//! form: an argument is an [`Atom`], an atom is a local, and anything else
//! is let-bound first. Monomorphisation and defunctionalisation happen above
//! this tier, so a type is a name with constructors and nothing more — no
//! parameters, no arrows, no variables.
//!
//! Deliberately absent, each folding into something simpler:
//!
//! - `If` — a [`Expr::Match`] on `Bool`
//! - tuples and records — single-constructor inductives
//! - blocks — chains of [`Expr::Let`]
//! - `absurd` — a match with no arms
//! - `Lam`/`App` — first-order; recursion lives between top-level names
//! - primitives — arithmetic is recursion over a constructor, and the
//!   machine tier is what recognises it

mod constant;

use crate::index::index;

index!(ExprId, FnId, TypeId, CtorId, ConstId, Local);

/// A run of values in one of the pools beside the arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    start: u32,
    len: u32,
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

    fn of(start: usize, len: usize) -> Self {
        Self {
            start: u32::try_from(start).expect("a pool within 4G"),
            len: u32::try_from(len).expect("a run within 4G"),
        }
    }

    fn range(self) -> std::ops::Range<usize> {
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
    pub name: String,
    pub ctors: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ctor {
    pub name: String,
    pub owner: TypeId,
    /// The types of its fields, in order.
    pub fields: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// The types of its parameters, which are the first locals.
    pub params: Range,
    pub result: TypeId,
    /// How many locals the body binds, parameters included.
    pub locals: u32,
    pub body: ExprId,
}

/// A program, and the pools every part of it lives in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    types: Vec<Type>,
    ctors: Vec<Ctor>,
    functions: Vec<Function>,
    /// The term arena.
    exprs: Vec<Expr>,
    /// Argument lists, for `Con` and `Call`.
    atoms: Vec<Atom>,
    arms: Vec<Arm>,
    /// Field and parameter types.
    types_pool: Vec<TypeId>,
    /// Known values, encoded. See [`constant`].
    consts: Vec<Const>,
    bytes: Vec<u8>,
    entry: Option<FnId>,
}

/// Where a known value's bytes are, and how many elements it has when its
/// type is spine-shaped. Peeling a spine hands back a descriptor rather than
/// rewriting its prefix, so a tail costs no bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Const {
    bytes: Range,
    count: u32,
}

impl Program {
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
    pub fn known(&self, id: ConstId) -> Const {
        self.consts[id.index()]
    }

    #[must_use]
    pub fn known_bytes(&self, id: ConstId) -> &[u8] {
        &self.bytes[self.known(id).bytes.range()]
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

pub use constant::{Shape, Value, as_bytes, decode, shape};

impl Program {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve a type. Its constructors follow, so that a field may name the
    /// type being declared.
    pub fn declare_type(&mut self, name: &str) -> TypeId {
        self.types.push(Type {
            name: name.to_owned(),
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
            let at = self.types_pool.len();
            self.types_pool.extend_from_slice(fields);
            self.ctors.push(Ctor {
                name: (*name).to_owned(),
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
        let at = self.types_pool.len();
        self.types_pool.extend_from_slice(params);
        self.functions.push(Function {
            name: name.to_owned(),
            params: Range::of(at, params.len()),
            result,
            locals: u32::try_from(params.len()).expect("a sane arity"),
            body: ExprId(0),
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
        let start = self.bytes.len();
        let mut bytes = Vec::new();
        let count = constant::encode(self, id, value, &mut bytes);
        let len = bytes.len();
        self.bytes.extend_from_slice(&bytes);
        self.consts.push(Const {
            bytes: Range::of(start, len),
            count,
        });
        ConstId::at(self.consts.len() - 1)
    }

    /// Build a declared function's body. The parameters are the first locals.
    ///
    /// # Panics
    ///
    /// If the function takes more parameters than the tier allows.
    pub fn define(&mut self, id: FnId, build: impl FnOnce(&mut Builder, &[Atom]) -> ExprId) {
        let arity = u32::try_from(self.params(id).len()).expect("a sane arity");
        let params: Vec<Atom> = (0..arity).map(|at| Atom(Local(at))).collect();
        let mut builder = Builder {
            program: self,
            locals: arity,
            high: arity,
        };
        let body = build(&mut builder, &params);
        let locals = builder.high;
        self.functions[id.index()].body = body;
        self.functions[id.index()].locals = locals;
    }
}

/// Builds one function's body. Binders are positional, so this is what keeps
/// the levels straight: a `let` or an arm takes the next, and sibling arms
/// take the same ones back.
#[derive(Debug)]
pub struct Builder<'a> {
    program: &'a mut Program,
    locals: u32,
    high: u32,
}

impl Builder<'_> {
    fn push(&mut self, expr: Expr) -> ExprId {
        self.program.exprs.push(expr);
        ExprId::at(self.program.exprs.len() - 1)
    }

    fn atoms(&mut self, atoms: &[Atom]) -> Range {
        let at = self.program.atoms.len();
        self.program.atoms.extend_from_slice(atoms);
        Range::of(at, atoms.len())
    }

    pub fn atom(&mut self, atom: Atom) -> ExprId {
        self.push(Expr::Atom(atom))
    }

    pub fn con(&mut self, ctor: CtorId, args: &[Atom]) -> ExprId {
        let args = self.atoms(args);
        self.push(Expr::Con(ctor, args))
    }

    pub fn call(&mut self, function: FnId, args: &[Atom]) -> ExprId {
        let args = self.atoms(args);
        self.push(Expr::Call(function, args))
    }

    pub fn known(&mut self, id: TypeId, value: ConstId) -> ExprId {
        self.push(Expr::Static(id, value))
    }

    /// Bind `value` to the next local and continue.
    pub fn let_(&mut self, value: ExprId, body: impl FnOnce(&mut Self, Atom) -> ExprId) -> ExprId {
        let bound = Atom(Local(self.locals));
        self.locals += 1;
        self.high = self.high.max(self.locals);
        let body = body(self, bound);
        self.locals -= 1;
        self.push(Expr::Let(value, body))
    }

    /// One arm per constructor of `owner`, in tag order, so a match is
    /// exhaustive by construction. Each arm binds its constructor's fields as
    /// the next locals; sibling arms take the same ones, being alternatives.
    ///
    /// # Panics
    ///
    /// If the type outgrew the constructors it may have.
    pub fn match_(
        &mut self,
        scrutinee: Atom,
        owner: TypeId,
        mut arm: impl FnMut(&mut Self, CtorId, &[Atom]) -> ExprId,
    ) -> ExprId {
        let ctors: Vec<CtorId> = (0..u32::try_from(self.program.type_(owner).ctors.len())
            .expect("a sane constructor count"))
            .map(|tag| self.program.ctor_at(owner, tag))
            .collect();
        let outer = self.locals;
        let mut arms = Vec::new();
        for ctor in ctors {
            let count = u32::try_from(self.program.fields(ctor).len()).expect("sane arity");
            let bound: Vec<Atom> = (0..count).map(|at| Atom(Local(outer + at))).collect();
            self.locals = outer + count;
            self.high = self.high.max(self.locals);
            let body = arm(self, ctor, &bound);
            arms.push(Arm { ctor, body });
        }
        self.locals = outer;
        let at = self.program.arms.len();
        self.program.arms.extend_from_slice(&arms);
        let arms = Range::of(at, arms.len());
        self.push(Expr::Match(scrutinee, arms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Nat`, `U8`, and `List_U8`, which between them cover every shape the
    /// encoding has a case for.
    fn types() -> (Program, TypeId, TypeId, TypeId) {
        let mut program = Program::new();
        let nat = program.declare_type("Nat");
        program.define_type(nat, &[("Zero", &[]), ("Succ", &[nat])]);
        let byte = program.declare_type("U8");
        program.define_type(byte, &[("Zero", &[]), ("Succ", &[byte])]);
        let list = program.declare_type("List_U8");
        program.define_type(list, &[("Nil", &[]), ("Cons", &[byte, list])]);
        (program, nat, byte, list)
    }

    fn nat(program: &Program, id: TypeId, value: u64) -> Value {
        let mut term = Value(program.ctor_at(id, 0), Vec::new());
        for _ in 0..value {
            term = Value(program.ctor_at(id, 1), vec![term]);
        }
        term
    }

    fn list(program: &Program, list: TypeId, byte: TypeId, bytes: &[u8]) -> Value {
        let mut term = Value(program.ctor_at(list, 0), Vec::new());
        for value in bytes.iter().rev() {
            term = Value(
                program.ctor_at(list, 1),
                vec![nat(program, byte, u64::from(*value)), term],
            );
        }
        term
    }

    #[test]
    fn a_term_stays_small() {
        assert_eq!(size_of::<Expr>(), 16);
        assert_eq!(size_of::<Arm>(), 8);
        assert_eq!(size_of::<Atom>(), 4);
    }

    #[test]
    fn a_spine_is_recognised_and_a_branching_type_is_not() {
        let (mut program, nat, _, list) = types();
        assert!(matches!(shape(&program, nat), Shape::Spine { .. }));
        assert!(matches!(shape(&program, list), Shape::Spine { .. }));

        let tree = program.declare_type("Tree");
        program.define_type(tree, &[("Leaf", &[]), ("Node", &[tree, tree])]);
        assert_eq!(shape(&program, tree), Shape::Tagged, "two recursive fields");

        let extra = program.declare_type("Three");
        program.define_type(extra, &[("A", &[]), ("B", &[extra]), ("C", &[])]);
        assert_eq!(shape(&program, extra), Shape::Tagged, "a third constructor");
    }

    #[test]
    fn a_natural_costs_the_logarithm_of_its_value() {
        let (mut program, nat_id, _, _) = types();
        let thousand = nat(&program, nat_id, 1000);
        let id = program.intern(nat_id, &thousand);
        assert_eq!(
            program.known_bytes(id),
            [0xE8, 0x07],
            "1000, seven bits a byte"
        );
        assert_eq!(
            decode(&program, nat_id, program.known_bytes(id)).0,
            thousand
        );
    }

    #[test]
    fn an_ascii_string_is_its_own_bytes_after_the_count() {
        let (mut program, _, byte, list_id) = types();
        let hello = list(&program, list_id, byte, b"Hello");
        let id = program.intern(list_id, &hello);
        assert_eq!(program.known_bytes(id), b"\x05Hello");

        let held = program.known(id);
        let lifted = as_bytes(&program, list_id, program.known_bytes(id), held.count);
        assert_eq!(lifted, Some(&b"Hello"[..]), "liftable as it stands");
        assert_eq!(decode(&program, list_id, program.known_bytes(id)).0, hello);
    }

    /// A byte past 127 takes two bytes to encode, so the run is no longer the
    /// value and a target has to decode it.
    #[test]
    fn a_string_past_ascii_is_not_liftable() {
        let (mut program, _, byte, list_id) = types();
        let cafe = list(&program, list_id, byte, "café".as_bytes());
        let id = program.intern(list_id, &cafe);
        let held = program.known(id);
        assert!(as_bytes(&program, list_id, program.known_bytes(id), held.count).is_none());
        assert_eq!(decode(&program, list_id, program.known_bytes(id)).0, cafe);
    }

    #[test]
    fn a_record_costs_no_tag_and_packs_into_a_list() {
        let (mut program, nat_id, _, _) = types();
        let point = program.declare_type("Point");
        program.define_type(point, &[("MkPoint", &[nat_id, nat_id])]);
        let points = program.declare_type("List_Point");
        program.define_type(points, &[("Nil", &[]), ("Cons", &[point, points])]);

        let at = |program: &Program, x, y| {
            Value(
                program.ctor_at(point, 0),
                vec![nat(program, nat_id, x), nat(program, nat_id, y)],
            )
        };
        let mut value = Value(program.ctor_at(points, 0), Vec::new());
        for (x, y) in [(3u64, 4u64), (1, 2)] {
            value = Value(program.ctor_at(points, 1), vec![at(&program, x, y), value]);
        }
        let id = program.intern(points, &value);
        assert_eq!(
            program.known_bytes(id),
            [2, 1, 2, 3, 4],
            "count, then packed pairs"
        );
        assert_eq!(decode(&program, points, program.known_bytes(id)).0, value);
    }
}
