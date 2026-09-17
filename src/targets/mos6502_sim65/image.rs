//! The sim65 container: a twelve-byte header, then the data and the code.

use super::{CSP, Code, LOAD};
use crate::ir::Program;

pub(super) fn image(code: &Code, program: &Program) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + program.data().len() + code.bytes.len());

    out.extend_from_slice(b"sim65");
    out.push(2); // header version
    out.push(0); // CPU: 6502
    out.push(CSP); // where the hooks find their argument stack
    out.extend_from_slice(&LOAD.to_le_bytes());
    out.extend_from_slice(&code.reset.to_le_bytes());

    out.extend_from_slice(program.data());
    out.extend_from_slice(program.globals());
    out.extend_from_slice(&code.bytes);
    out
}
