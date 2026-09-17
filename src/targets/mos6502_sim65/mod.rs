//! MOS 6502 under cc65's sim65.
//!
//! A word is sixteen bits, the width of an address, and the registers are
//! eight, so every word operation is two byte operations with a carry.
//! Values live in a frame on a software stack, reached through a zero-page
//! pointer; the hardware stack carries only return addresses, which bounds
//! recursion at 128 deep until those move too.
//!
//! The image is a twelve-byte header, the data, and the code, loaded at
//! `$0200`. The platform is the simulator's hooks, and `grow` is a bump
//! upwards from the end of the image toward the stack.

mod asm;
mod image;
mod lower;

use crate::ir::Program;
use crate::targets::Artifact;

/// Where sim65 loads the image, and the zero-page pairs below it.
const LOAD: u16 = 0x0200;

/// The C stack the simulator's hooks take their arguments on. Its address
/// goes in the header, and nothing else uses it.
const CSP: u8 = 0x02;
const CSTACK_TOP: u16 = 0xFE00;

/// The frame pointer, and the stack of frames it walks down.
const FP: u8 = 0x04;
const FRAMES_TOP: u16 = 0xFD00;

/// Scratch pairs: every two-operand form works through these.
const T0: u8 = 0x06;
const T1: u8 = 0x08;
/// The pair an indirect load or store reaches its address through.
const PTR: u8 = 0x0A;
/// Where a function leaves its result, and where a caller puts arguments.
const R0: u8 = 0x0C;
const ARGS: u8 = 0x0E;
const ARG_COUNT: u8 = 4;
/// The bump allocator's cursor, set at reset to the end of the image.
const HEAP: u8 = 0x16;

/// The simulator's hooks: a `jmp` or `jsr` to one is answered by the host.
const HOOK_WRITE: u16 = 0xFFF7;
const HOOK_EXIT: u16 = 0xFFF9;

#[must_use]
pub fn emit(program: &Program) -> Vec<Artifact> {
    vec![Artifact {
        extension: ".bin",
        bytes: image::image(&lower::lower(program), program),
        executable: false,
    }]
}

struct Code {
    bytes: Vec<u8>,
    /// Where the code begins, which is after the data, and so where the
    /// machine has to be started.
    reset: u16,
}
