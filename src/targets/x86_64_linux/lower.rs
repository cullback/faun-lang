//! No register allocator:
//!
//! - A constant or an address is materialised into the register that wants
//!   it, and lowering tracks what each register holds so a copy can beat a
//!   reload.
//! - Everything else, region parameters included, lives in a stack slot.
//!   Slow, correct, and replaceable.

use super::encode::{Encoder, Reg};
use super::{Code, Reloc};
use crate::ir::{
    Binary, DataId, Instruction, Op, PlatformId, Program, Region, Relation, Terminator, ValueId,
};

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;
const STDOUT: u64 = 1;

/// Condition codes, as the low nibble of a `jcc` or `setcc`.
const EQUAL: u8 = 0x4;
const BELOW: u8 = 0x2;

/// `syscall` returns in rax and destroys rcx and r11.
const CLOBBERED: [Reg; 3] = [Reg::Rax, Reg::Rcx, Reg::R11];

const TRACKED: [Reg; 6] = [Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::Rsi, Reg::Rdi, Reg::R11];

/// Where a value comes from when something needs it.
#[derive(Clone, Copy)]
enum Source {
    Const(u64),
    Addr(DataId),
    Slot(u32),
}

pub(super) fn lower(program: &Program) -> Code {
    let plan = Plan::new(program);
    let mut state = Lowering {
        program,
        plan,
        asm: Encoder::default(),
        relocs: Vec::new(),
        known: [None; 16],
        ifs: Vec::new(),
        loops: Vec::new(),
    };

    if state.plan.frame > 0 {
        state.asm.open_frame(state.plan.frame);
    }
    state.region(program.body());
    state.finish()
}

/// Which values need a stack slot, and how big the frame has to be.
struct Plan {
    source: Vec<Source>,
    /// Where the scratch a transfer passes through begins.
    scratch: u32,
    frame: u32,
}

impl Plan {
    fn new(program: &Program) -> Self {
        let mut plan = Self {
            source: vec![Source::Slot(0); program.values()],
            scratch: 0,
            frame: 0,
        };
        let mut widest = 0;
        plan.walk(program.body(), &mut widest);
        plan.scratch = plan.frame;
        plan.frame += 8 * u32::try_from(widest).expect("a sane region width");
        plan
    }

    fn walk(&mut self, region: &Region, widest: &mut usize) {
        for &param in &region.params {
            self.give_slot(param);
        }
        *widest = (*widest).max(region.params.len());

        for inst in &region.instructions {
            match &inst.op {
                Op::Constant { value, .. } => {
                    self.source[inst.results[0].0] = Source::Const(*value);
                }
                Op::AddressOf(data) => self.source[inst.results[0].0] = Source::Addr(*data),
                _ => {
                    for &result in &inst.results {
                        self.give_slot(result);
                    }
                }
            }
            match &inst.op {
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.walk(then_region, widest);
                    self.walk(else_region, widest);
                }
                Op::Loop { body, .. } => self.walk(body, widest),
                _ => {}
            }
        }
        *widest = (*widest).max(carried(&region.terminator).len());
    }

    fn give_slot(&mut self, value: ValueId) {
        self.source[value.0] = Source::Slot(self.frame);
        self.frame += 8;
    }
}

/// The values a terminator hands to whatever it leaves for.
fn carried(terminator: &Terminator) -> &[ValueId] {
    match terminator {
        Terminator::Yield(values)
        | Terminator::Continue(values)
        | Terminator::Break(values)
        | Terminator::Return(values) => values,
        Terminator::Unreachable => &[],
    }
}

struct IfFrame {
    results: Vec<ValueId>,
    ends: Vec<usize>,
}

struct LoopFrame {
    params: Vec<ValueId>,
    head: usize,
    results: Vec<ValueId>,
    ends: Vec<usize>,
}

struct Lowering<'a> {
    program: &'a Program,
    plan: Plan,
    asm: Encoder,
    relocs: Vec<Reloc>,
    /// The constant each register is known to hold. `None` is unknown, which
    /// is not the same as zero.
    known: [Option<u64>; 16],
    ifs: Vec<IfFrame>,
    loops: Vec<LoopFrame>,
}

impl Lowering<'_> {
    fn finish(self) -> Code {
        Code {
            bytes: self.asm.finish(),
            relocs: self.relocs,
        }
    }

    fn region(&mut self, region: &Region) {
        for inst in &region.instructions {
            self.instruction(inst);
        }
        self.terminator(&region.terminator);
    }

    fn instruction(&mut self, inst: &Instruction) {
        match &inst.op {
            // Materialised where they are read, never where they are written.
            Op::Constant { .. } | Op::AddressOf(_) => {}
            Op::Binary { op, left, right } => {
                let opcode = match op {
                    Binary::Add => 0x01,
                    Binary::Sub => 0x29,
                };
                self.pair(*left, *right);
                self.asm.alu(opcode, Reg::Rax, Reg::Rcx);
                self.keep(inst.results[0]);
            }
            Op::Compare {
                relation,
                left,
                right,
            } => {
                let cc = match relation {
                    Relation::Equal => EQUAL,
                    Relation::Less => BELOW,
                };
                self.pair(*left, *right);
                self.asm.alu(0x39, Reg::Rax, Reg::Rcx);
                self.asm.set_if(cc, Reg::Rax);
                self.keep(inst.results[0]);
            }
            Op::PlatformCall { platform, args } => self.platform_call(*platform, args),
            Op::If {
                condition,
                then_region,
                else_region,
            } => self.conditional(*condition, then_region, else_region, &inst.results),
            Op::Loop { initial, body } => self.repeat(initial, body, &inst.results),
        }
    }

    fn platform_call(&mut self, platform: PlatformId, args: &[ValueId]) {
        match self.program.platforms()[platform.0].name.as_str() {
            "write" => {
                // rax first, so stdout can be copied from it.
                self.load_const(Reg::Rax, SYS_WRITE);
                self.load_const(Reg::Rdi, STDOUT);
                self.read(Reg::Rsi, args[0]);
                self.read(Reg::Rdx, args[1]);
            }
            "exit" => {
                self.load_const(Reg::Rax, SYS_EXIT);
                self.read(Reg::Rdi, args[0]);
            }
            other => panic!("this target provides no `{other}`"),
        }
        self.syscall();
    }

    fn conditional(
        &mut self,
        condition: ValueId,
        then_region: &Region,
        else_region: &Region,
        results: &[ValueId],
    ) {
        self.read(Reg::Rax, condition);
        self.asm.alu(0x85, Reg::Rax, Reg::Rax); // test rax, rax
        let otherwise = self.asm.jump_if(EQUAL);

        self.ifs.push(IfFrame {
            results: results.to_vec(),
            ends: Vec::new(),
        });
        self.region(then_region);
        let here = self.asm.here();
        self.asm.patch(otherwise, here);
        self.forget();
        self.region(else_region);

        let frame = self.ifs.pop().expect("the frame this if pushed");
        self.land(frame.ends);
    }

    fn repeat(&mut self, initial: &[ValueId], body: &Region, results: &[ValueId]) {
        self.transfer(initial, &body.params);
        self.forget();

        self.loops.push(LoopFrame {
            params: body.params.clone(),
            head: self.asm.here(),
            results: results.to_vec(),
            ends: Vec::new(),
        });
        self.region(body);

        let frame = self.loops.pop().expect("the frame this loop pushed");
        self.land(frame.ends);
    }

    /// Aim every jump that was waiting for the end of a construct at here.
    fn land(&mut self, ends: Vec<usize>) {
        let end = self.asm.here();
        for hole in ends {
            self.asm.patch(hole, end);
        }
        self.forget();
    }

    fn terminator(&mut self, terminator: &Terminator) {
        match terminator {
            Terminator::Yield(values) => {
                let frame = self.ifs.last().expect("a yield inside an if");
                let results = frame.results.clone();
                self.transfer(values, &results);
                let hole = self.asm.jump();
                self.ifs.last_mut().expect("an if").ends.push(hole);
            }
            Terminator::Continue(values) => {
                let frame = self.loops.last().expect("a continue inside a loop");
                let (params, head) = (frame.params.clone(), frame.head);
                self.transfer(values, &params);
                let hole = self.asm.jump();
                self.asm.patch(hole, head);
            }
            Terminator::Break(values) => {
                let frame = self.loops.last().expect("a break inside a loop");
                let results = frame.results.clone();
                self.transfer(values, &results);
                let hole = self.asm.jump();
                self.loops.last_mut().expect("a loop").ends.push(hole);
            }
            // Nothing calls into this program, so leaving it is exiting.
            Terminator::Return(_) => {
                self.load_const(Reg::Rax, SYS_EXIT);
                self.load_const(Reg::Rdi, 0);
                self.syscall();
            }
            Terminator::Unreachable => {}
        }
    }

    /// Hand `sources` over as `destinations`, through scratch so that a swap
    /// between two slots cannot overwrite its own input.
    fn transfer(&mut self, sources: &[ValueId], destinations: &[ValueId]) {
        if sources.is_empty() {
            return;
        }
        for (index, &source) in sources.iter().enumerate() {
            self.read(Reg::Rax, source);
            self.asm.store_slot(Reg::Rax, self.scratch(index));
        }
        for (index, &destination) in destinations.iter().enumerate() {
            self.asm.load_slot(Reg::Rax, self.scratch(index));
            self.keep(destination);
        }
        self.forget();
    }

    fn scratch(&self, index: usize) -> u32 {
        self.plan.scratch + 8 * u32::try_from(index).expect("a sane region width")
    }

    /// Put `left` in rax and `right` in rcx, which is what every two-operand
    /// form here works on.
    fn pair(&mut self, left: ValueId, right: ValueId) {
        self.read(Reg::Rax, left);
        self.read(Reg::Rcx, right);
    }

    /// Write rax to wherever `value` lives.
    fn keep(&mut self, value: ValueId) {
        let Source::Slot(offset) = self.plan.source[value.0] else {
            panic!("a computed value needs a slot");
        };
        self.asm.store_slot(Reg::Rax, offset);
        self.known[Reg::Rax.index()] = None;
    }

    fn read(&mut self, dst: Reg, value: ValueId) {
        match self.plan.source[value.0] {
            Source::Const(value) => self.load_const(dst, value),
            Source::Addr(data) => self.load_addr(dst, data),
            Source::Slot(offset) => {
                self.asm.load_slot(dst, offset);
                self.known[dst.index()] = None;
            }
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

    /// Addressed from the instruction pointer, so the program does not care
    /// where it was loaded and needs no relocation at startup.
    fn load_addr(&mut self, dst: Reg, data: DataId) {
        let offset = self.asm.lea_rip(dst);
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

    /// Control joins here from somewhere else, so nothing is known.
    const fn forget(&mut self) {
        self.known = [None; 16];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Class;

    #[test]
    fn hello_world_uses_the_short_encodings() {
        let code = lower(&crate::hello_world());
        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x6A, 0x01,                   // push 1
            0x58,                         // pop rax          (sys_write)
            0x89, 0xC7,                   // mov edi, eax     (stdout, rax is already 1)
            0x48, 0x8D, 0x35,             // lea rsi, [rip+d] (the message,
            0x00, 0x00, 0x00, 0x00,       //                   distance filled in later)
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
        assert_eq!(code.relocs[0].offset, 8);
    }

    /// A program of nothing but constants needs no frame, which is what keeps
    /// hello world at the size it is.
    #[test]
    fn constants_never_reach_the_stack() {
        assert_eq!(Plan::new(&crate::hello_world()).frame, 0);
    }

    #[test]
    fn a_loop_jumps_backwards_to_its_own_head() {
        let mut program = Program::new();
        program.build(|b| {
            let start = b.constant(Class::Word, 1);
            b.loop_(vec![start], Vec::new(), |b, params| {
                let one = b.constant(Class::Word, 1);
                let next = b.binary(Binary::Sub, params[0], one);
                Terminator::Continue(vec![next])
            });
            Terminator::Unreachable
        });

        let bytes = lower(&program).bytes;
        // The last five bytes are `jmp rel32` back to the loop head, which is
        // behind us, so the displacement is negative.
        let (jump, displacement) = bytes.split_at(bytes.len() - 4);
        assert_eq!(jump.last(), Some(&0xE9));
        let displacement = i32::from_le_bytes(displacement.try_into().unwrap());
        assert!(displacement < 0, "a loop must branch backwards");
    }

    #[test]
    fn a_syscall_invalidates_the_registers_it_clobbers() {
        let mut program = Program::new();
        let data = program.intern(b"x".as_slice());
        let write = program.platform("write", vec![Class::Address, Class::Word], Vec::new());
        let exit = program.platform("exit", vec![Class::Word], Vec::new());
        program.build(|b| {
            let buf = b.address_of(data);
            let len = b.constant(Class::Word, 1);
            b.call(write, vec![buf, len]);
            // Exit with the status stdout had, so the two registers that both
            // held 1 across the syscall can be told apart.
            let status = b.constant(Class::Word, 1);
            b.call(exit, vec![status]);
            Terminator::Unreachable
        });

        let bytes = lower(&program).bytes;
        assert!(bytes.ends_with(&[
            0x6A, 0x3C, 0x58, // push 60; pop rax -- the syscall destroyed rax
            0x0F, 0x05, //       nothing for rdi  -- it survived still holding 1
        ]));
    }
}
