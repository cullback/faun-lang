//! The machine IR. Names no register, syscall, instruction set, or word
//! width.
//!
//! It decides:
//!
//! - Representation: the form each value takes, and the width it needs
//! - Ownership: where a value is retained and released, and when storage is
//!   unique enough to be reused in place
//! - Layout: what is data, what is global, and what an offset means
//! - Function signatures: arity, and the class of each parameter and result
//!
//! It does not decide:
//!
//! - Which values live in registers and which in memory
//! - Calling conventions, stacks, and frames
//! - Instruction selection and encoding
//! - The target's word size, and what its platform provides
//!
//! Instructions live in one flat array, and everything of variable length --
//! operand lists, the regions a construct holds -- is a range into a pool
//! beside it. Nothing about an instruction is boxed or owned, so replacing
//! one is a store and a pass over them is a linear scan.
//!
//! Control flow is nested and never flattened to blocks and jumps:
//!
//! - Structure lowers to jumps in about fifteen lines. The reverse needs a
//!   Relooper, which emscripten and LLVM's wasm backend each carry.
//! - Regions take parameters, so a loop stays the tail call it was written
//!   as and no target reconstructs one.
//! - A region's instructions are contiguous, so walking one stays sequential
//!   where a graph of blocks would scatter.
//!
//! A constant is 64 bits of pattern, not a number:
//!
//! - `-1` and `u64::MAX` are one value. [`Builder::constant_signed`] is a
//!   spelling, not a second representation.
//! - Signedness picks `div` against `idiv` and `jb` against `jl`, so it
//!   belongs to operations. LLVM dropped signed and unsigned integer types;
//!   wasm never had them.
mod builder;
mod model;
mod validate;

pub use builder::Builder;
pub use model::{
    Binary, Class, DataId, Exit, Function, FunctionId, Offset, Op, OpId, Operands, Origin,
    Platform, PlatformId, Program, Region, RegionId, Relation, Span, Terminator, ValueId, Width,
    wrap,
};
pub use validate::validate;

#[cfg(test)]
mod tests;
