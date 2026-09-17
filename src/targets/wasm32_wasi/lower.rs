//! `fd_write` takes its `ciovec` in linear memory rather than in registers.
//! Every IR value here is constant, so the vector is known at compile time
//! and ships as initialised data instead of stores at startup.
//!
//! The peephole that matters for size is `proc_exit`: returning from `_start`
//! already exits zero, so a program ending in `exit 0` calls nothing -- and
//! then need not import `proc_exit`, which is most of an import section.

use super::{CALL, Code, DROP, I32_CONST, Segment};
use crate::ir::{Inst, Program, Value};
use crate::targets::bytes::{Bytes, len32};

/// Imported function indices, in declaration order.
pub(super) const FD_WRITE: u32 = 0;
pub(super) const PROC_EXIT: u32 = 1;

const STDOUT: i32 = 1;

/// Linear memory: the cell `fd_write` reports its byte count into and which
/// nothing reads, then the vectors, four-byte aligned as WASI requires, then
/// the data.
const NWRITTEN: i32 = 0;
const IOVECS: u32 = 8;
const IOVEC_LEN: u32 = 8;

pub(super) fn lower(program: &Program) -> Code {
    let values = program.values();
    let imports_proc_exit = needs_proc_exit(program);

    let prints = program
        .insts()
        .iter()
        .filter(|inst| matches!(inst, Inst::Print { .. }))
        .count();

    let data_start = IOVECS + IOVEC_LEN * len32(prints);
    let mut blob = Vec::new();
    let mut addrs = Vec::with_capacity(program.data().len());
    for bytes in program.data() {
        addrs.push(data_start + len32(blob.len()));
        blob.extend_from_slice(bytes);
    }

    let mut body = Bytes::default();
    let mut iovecs = Vec::new();

    for inst in program.insts() {
        match *inst {
            // Pulled in by the effects below.
            Inst::Imm { .. } | Inst::DataAddr { .. } => {}
            Inst::Print { buf, len } => {
                let base = match values[buf.0] {
                    Value::Addr(data) => addrs[data.0],
                    Value::Const(value) => u32::try_from(value).expect("an address"),
                };
                let length = u32::try_from(expect_const(values[len.0])).expect("a length");

                let iovec = IOVECS + IOVEC_LEN * len32(iovecs.len());
                iovecs.extend_from_slice(&base.to_le_bytes());
                iovecs.extend_from_slice(&length.to_le_bytes());

                body.byte(I32_CONST);
                body.sleb(STDOUT);
                body.byte(I32_CONST);
                body.sleb(iovec.cast_signed());
                body.byte(I32_CONST);
                body.sleb(1);
                body.byte(I32_CONST);
                body.sleb(NWRITTEN);
                body.byte(CALL);
                body.uleb(FD_WRITE);
                body.byte(DROP); // The errno, which there is nobody to tell.
            }
            Inst::Exit { status } => {
                if imports_proc_exit {
                    body.byte(I32_CONST);
                    let status = expect_const(values[status.0]);
                    body.sleb(i32::try_from(status).expect("an exit status"));
                    body.byte(CALL);
                    body.uleb(PROC_EXIT);
                }
            }
        }
    }

    let mut data = Vec::new();
    if !iovecs.is_empty() {
        data.push(Segment {
            offset: IOVECS,
            bytes: iovecs,
        });
    }
    if !blob.is_empty() {
        data.push(Segment {
            offset: data_start,
            bytes: blob,
        });
    }

    Code {
        body: body.finish(),
        data,
        imports_proc_exit,
    }
}

/// Falling off the end of `_start` already means success, so a trailing
/// `exit 0` costs nothing and needs no import.
fn needs_proc_exit(program: &Program) -> bool {
    let values = program.values();
    let last = program.insts().len().saturating_sub(1);

    program
        .insts()
        .iter()
        .enumerate()
        .any(|(i, inst)| match *inst {
            Inst::Exit { status } => i != last || expect_const(values[status.0]) != 0,
            _ => false,
        })
}

fn expect_const(value: Value) -> u64 {
    match value {
        Value::Const(value) => value,
        Value::Addr(_) => panic!("an address cannot be used where a number is expected"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_world_needs_no_exit_call() {
        let code = lower(&crate::hello_world());
        assert!(!code.imports_proc_exit);

        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x41, 0x01, // i32.const 1   (stdout)
            0x41, 0x08, // i32.const 8   (the one ciovec)
            0x41, 0x01, // i32.const 1   (vector count)
            0x41, 0x00, // i32.const 0   (where the byte count goes)
            0x10, 0x00, // call 0        (fd_write)
            0x1A,       // drop
        ];
        assert_eq!(code.body, expected);
    }

    #[test]
    fn the_vector_points_at_the_message() {
        let code = lower(&crate::hello_world());
        let iovec = &code.data[0];
        assert_eq!(iovec.offset, IOVECS);
        // Base 16 is the first byte past the vector itself, length 14.
        assert_eq!(iovec.bytes, [16, 0, 0, 0, 14, 0, 0, 0]);
        assert_eq!(code.data[1].bytes, b"Hello, World!\n");
    }

    #[test]
    fn a_nonzero_status_has_to_be_called_in() {
        let mut program = Program::new();
        let status = program.imm(3);
        program.exit(status);

        let code = lower(&program);
        assert!(code.imports_proc_exit);
        assert_eq!(code.body, [0x41, 0x03, 0x10, 0x01]);
    }

    #[test]
    fn an_early_exit_has_to_be_called_in() {
        let mut program = Program::new();
        let status = program.imm(0);
        program.exit(status);
        let data = program.intern(b"unreachable".as_slice());
        let buf = program.data_addr(data);
        let len = program.imm(11);
        program.print(buf, len);

        assert!(lower(&program).imports_proc_exit);
    }
}
