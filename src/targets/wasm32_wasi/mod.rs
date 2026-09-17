//! [`lower`] decides what `_start` does and where things sit in linear
//! memory; [`module`] wraps that in the sections a wasm file is made of.

mod lower;
mod module;

use crate::ir::Program;
use crate::targets::Artifact;

const I32: u8 = 0x7F;
const FUNC_TYPE: u8 = 0x60;
const I32_CONST: u8 = 0x41;
const CALL: u8 = 0x10;
const DROP: u8 = 0x1A;
const END: u8 = 0x0B;

#[must_use]
pub fn emit(program: &Program) -> Vec<Artifact> {
    vec![Artifact {
        extension: ".wasm",
        bytes: module::module(&lower::lower(program)),
        executable: false,
    }]
}

struct Code {
    bodies: Vec<Body>,
    entry: usize,
    data: Vec<Segment>,
    /// The WASI functions this module calls, in index order. A module that
    /// calls none imports none, and declares no memory for them to read.
    imports: Vec<&'static str>,
    memory: bool,
}

struct Segment {
    offset: u32,
    bytes: Vec<u8>,
}

/// Instructions without the local declarations or the trailing `end`.
struct Body {
    params: u32,
    locals: u32,
    returns: u32,
    code: Vec<u8>,
}
