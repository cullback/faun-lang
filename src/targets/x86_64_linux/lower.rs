//! There is no register allocator. Every IR value is a constant or an
//! address read only by an effect, so lowering materialises each one straight
//! into the register the kernel expects. Tracking the constant each register
//! already holds is what buys the short encodings: a copy beats a reload, and
//! `push imm8`/`pop` beats `mov r32, imm32`.

use super::encode::{Encoder, Reg};
use super::{Code, Reloc};
use crate::ir::{DataId, Inst, Program, Value};

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;
const STDOUT: u64 = 1;

/// `syscall` returns in rax and destroys rcx and r11.
const CLOBBERED: [Reg; 3] = [Reg::Rax, Reg::Rcx, Reg::R11];

const TRACKED: [Reg; 6] = [Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::Rsi, Reg::Rdi, Reg::R11];

pub(super) fn lower(program: &Program) -> Code {
    let values = program.values();
    let mut state = Lowering::default();

    for inst in program.insts() {
        match *inst {
            // Pulled in by the effects below.
            Inst::Imm { .. } | Inst::DataAddr { .. } => {}
            Inst::Print { buf, len } => {
                // rax first, so stdout can be copied from it in two bytes.
                state.load_const(Reg::Rax, SYS_WRITE);
                state.load_const(Reg::Rdi, STDOUT);
                state.load(Reg::Rsi, values[buf.0]);
                state.load(Reg::Rdx, values[len.0]);
                state.syscall();
            }
            Inst::Exit { status } => {
                state.load_const(Reg::Rax, SYS_EXIT);
                state.load(Reg::Rdi, values[status.0]);
                state.syscall();
            }
        }
    }

    state.finish()
}

#[derive(Default)]
struct Lowering {
    asm: Encoder,
    relocs: Vec<Reloc>,
    /// `None` is unknown, which is not the same as zero.
    known: [Option<u64>; 16],
}

impl Lowering {
    fn finish(self) -> Code {
        Code {
            bytes: self.asm.finish(),
            relocs: self.relocs,
        }
    }

    fn load(&mut self, dst: Reg, value: Value) {
        match value {
            Value::Const(value) => self.load_const(dst, value),
            Value::Addr(data) => self.load_addr(dst, data),
        }
    }

    /// The shortest encoding that leaves `value` in `dst`. The forms differ
    /// in how they fill the upper bits, so the choice is made over the whole
    /// 64-bit word rather than over any reading of it.
    fn load_const(&mut self, dst: Reg, value: u64) {
        if self.known[dst.index()] == Some(value) {
            // Already there. Nothing to emit.
        } else if value == 0 {
            self.asm.xor_self(dst);
        } else if let Some(src) = self.holding(value) {
            self.asm.mov_reg(dst, src, u32::try_from(value).is_err());
        } else if let Ok(small) = i8::try_from(value.cast_signed()) {
            self.asm.push_imm8(small);
            self.asm.pop(dst);
        } else if let Ok(unsigned) = u32::try_from(value) {
            self.asm.mov_imm32(dst, unsigned);
        } else if let Ok(signed) = i32::try_from(value.cast_signed()) {
            self.asm.mov_sext_imm32(dst, signed);
        } else {
            self.asm.mov_imm64(dst, value);
        }
        self.known[dst.index()] = Some(value);
    }

    /// The executable loads at a fixed low address, so a zero-extended
    /// 32-bit immediate always reaches the data.
    fn load_addr(&mut self, dst: Reg, data: DataId) {
        let offset = self.asm.mov_imm32(dst, 0);
        self.relocs.push(Reloc { offset, data });
        self.known[dst.index()] = None;
    }

    fn holding(&self, value: u64) -> Option<Reg> {
        TRACKED
            .into_iter()
            .find(|reg| self.known[reg.index()] == Some(value))
    }

    fn syscall(&mut self) {
        self.asm.syscall();
        for reg in CLOBBERED {
            self.known[reg.index()] = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Vreg;

    #[test]
    fn hello_world_uses_the_short_encodings() {
        let code = lower(&crate::hello_world());
        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x6A, 0x01,                   // push 1
            0x58,                         // pop rax          (sys_write)
            0x89, 0xC7,                   // mov edi, eax     (stdout, rax is already 1)
            0xBE, 0x00, 0x00, 0x00, 0x00, // mov esi, <msg>   (address filled in later)
            0x6A, 0x0E,                   // push 14
            0x5A,                         // pop rdx          (message length)
            0x0F, 0x05,                   // syscall
            0x6A, 0x3C,                   // push 60
            0x58,                         // pop rax          (sys_exit)
            0x31, 0xFF,                   // xor edi, edi     (status 0)
            0x0F, 0x05,                   // syscall
        ];
        assert_eq!(code.bytes, expected);
        assert_eq!(code.relocs.len(), 1);
        assert_eq!(code.relocs[0].offset, 6);
    }

    #[test]
    fn a_value_too_wide_for_an_immediate_becomes_a_movabs() {
        let mut program = Program::new();
        let data = program.intern(b"x".as_slice());
        let buf = program.data_addr(data);
        let len = program.imm_signed(i64::MAX);
        program.print(buf, len);

        let bytes = lower(&program).bytes;
        // rdx is loaded last, so the movabs is the tail before `syscall`.
        assert!(bytes.ends_with(&[
            0x48, 0xBA, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, 0x0F, 0x05,
        ]));
    }

    #[test]
    fn signedness_is_only_a_reading_of_the_same_word() {
        let encode = |build: fn(&mut Program) -> Vreg| {
            let mut program = Program::new();
            let status = build(&mut program);
            program.exit(status);
            lower(&program).bytes
        };
        assert_eq!(
            encode(|p| p.imm_signed(-1)),
            encode(|p| p.imm(u64::MAX)),
            "the same bits must encode the same way"
        );
    }

    #[test]
    fn a_syscall_invalidates_the_registers_it_clobbers() {
        let mut program = Program::new();
        let data = program.intern(b"x".as_slice());
        let buf = program.data_addr(data);
        let len = program.imm(1);
        program.print(buf, len);
        // Exit with the same status stdout had, so the two registers that
        // both held 1 across the syscall can be told apart.
        let status = program.imm(1);
        program.exit(status);

        let bytes = lower(&program).bytes;
        assert!(bytes.ends_with(&[
            0x6A, 0x3C, 0x58, // push 60; pop rax -- the syscall destroyed rax
            0x0F, 0x05, //       nothing for rdi  -- it survived still holding 1
        ]));
    }
}
