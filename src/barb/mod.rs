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

mod builder;
mod constant;
mod ir;

pub use builder::Builder;
pub use constant::{Shape, Value, as_bytes, as_number, decode, shape};
pub use ir::{
    Arm, Atom, ConstId, Ctor, CtorId, Expr, ExprId, FnId, Function, Local, Program, Range, Symbol,
    Type, TypeId,
};

#[cfg(test)]
mod tests;
