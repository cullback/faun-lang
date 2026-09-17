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
    Binary, DataId, Function, FunctionId, Instruction, Offset, Op, PlatformId, Program, Region,
    Relation, Terminator, ValueId, Width,
};

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;
const SYS_MMAP: u64 = 9;
const STDOUT: u64 = 1;

/// `mmap`: readable and writable, private, and backed by nothing.
const PROT_READ_WRITE: u64 = 0x3;
const MAP_PRIVATE_ANONYMOUS: u64 = 0x22;

/// A word in bytes, which is what [`Offset::Words`] counts.
const WORD: i64 = 8;

/// A syscall's fourth argument goes in r10, where the ordinary convention
/// would use rcx.
const SYSCALL_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::R10, Reg::R8, Reg::R9];

/// System V, so that adding C interop later changes nothing here.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

/// Condition codes, as the low nibble of a `jcc` or `setcc`.
const EQUAL: u8 = 0x4;
const BELOW: u8 = 0x2;

/// `syscall` returns in rax and destroys rcx and r11.
const CLOBBERED: [Reg; 3] = [Reg::Rax, Reg::Rcx, Reg::R11];

const TRACKED: [Reg; 9] = [
    Reg::Rax,
    Reg::Rcx,
    Reg::Rdx,
    Reg::Rsi,
    Reg::Rdi,
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
];

#[derive(Clone, Copy)]
enum Source {
    Const(u64),
    Addr(DataId),
    Slot(u32),
}

pub(super) fn lower(program: &Program) -> Code {
    let entry = program.entry();
    let mut state = Lowering {
        program,
        entry,
        in_entry: true,
        plan: Plan::empty(program),
        asm: Encoder::default(),
        relocs: Vec::new(),
        calls: Vec::new(),
        starts: vec![0; program.functions().len()],
        known: [None; 16],
        ifs: Vec::new(),
        loops: Vec::new(),
    };

    // The entry goes first, so the image starts where the kernel jumps.
    let rest = (0..program.functions().len())
        .map(FunctionId)
        .filter(|id| id.0 != entry.0);
    for id in std::iter::once(entry).chain(rest) {
        state.function(id);
    }
    for (hole, id) in std::mem::take(&mut state.calls) {
        let target = state.starts[id.0];
        state.asm.patch(hole, target);
    }

    state.finish()
}

struct Plan {
    source: Vec<Source>,
    scratch: u32,
    frame: u32,
}

impl Plan {
    fn empty(program: &Program) -> Self {
        Self {
            source: vec![Source::Slot(0); program.values()],
            scratch: 0,
            frame: 0,
        }
    }

    /// Slots restart at every function, since a value never leaves the one
    /// that defined it. The body's region parameters are the function's, so
    /// walking the body gives them slots too.
    fn new(program: &Program, function: &Function) -> Self {
        let mut plan = Self::empty(program);
        let mut widest = function.params.len();
        plan.walk(&function.body, &mut widest);
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
        *widest = (*widest).max(transferred(&region.terminator).len());
    }

    fn give_slot(&mut self, value: ValueId) {
        self.source[value.0] = Source::Slot(self.frame);
        self.frame += 8;
    }
}

/// A byte distance, which is what the machine addresses in.
fn displacement(offset: Offset) -> i32 {
    let bytes = match offset {
        Offset::Bytes(bytes) => bytes,
        Offset::Words(words) => words * WORD,
    };
    i32::try_from(bytes).expect("an offset within 2 GiB")
}

/// The values a terminator moves through scratch. `Return` is not among
/// them: it reads straight into the register it leaves by, so counting it
/// would buy a frame that nothing writes to.
fn transferred(terminator: &Terminator) -> &[ValueId] {
    match terminator {
        Terminator::Yield(values) | Terminator::Continue(values) | Terminator::Break(values) => {
            values
        }
        Terminator::Return(_) | Terminator::Unreachable => &[],
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
    entry: FunctionId,
    in_entry: bool,
    plan: Plan,
    asm: Encoder,
    relocs: Vec<Reloc>,
    /// Each call, and the function it is waiting to be aimed at.
    calls: Vec<(usize, FunctionId)>,
    starts: Vec<usize>,
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

    fn function(&mut self, id: FunctionId) {
        let function = &self.program.functions()[id.0];
        self.in_entry = id.0 == self.entry.0;
        self.starts[id.0] = self.asm.here();
        self.plan = Plan::new(self.program, function);
        self.forget();

        if self.plan.frame > 0 {
            self.asm.open_frame(self.plan.frame);
        }
        for (&param, reg) in function.params.iter().zip(ARG_REGS) {
            self.store(param, reg);
        }
        self.region(&function.body);
    }

    fn region(&mut self, region: &Region) {
        for inst in &region.instructions {
            self.instruction(inst);
        }
        self.terminator(&region.terminator);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one call per op; the length is the patterns"
    )]
    fn instruction(&mut self, inst: &Instruction) {
        match &inst.op {
            // Materialised where they are read, never where they are written.
            Op::Constant { .. } | Op::AddressOf(_) => {}
            Op::Binary { op, left, right } => {
                self.binary(*op, *left, *right, inst.results[0]);
            }
            Op::Compare {
                relation,
                left,
                right,
            } => self.compare(*relation, *left, *right, inst.results[0]),
            Op::Load {
                width,
                address,
                offset,
            } => self.load_at(*width, *address, *offset, inst.results[0]),
            Op::Store {
                width,
                address,
                offset,
                value,
            } => self.store_at(*width, *address, *offset, *value),
            Op::PlatformCall { platform, args } => {
                self.platform_call(*platform, args, &inst.results);
            }
            Op::Call { function, args } => {
                for (&arg, reg) in args.iter().zip(ARG_REGS) {
                    self.read(reg, arg);
                }
                let hole = self.asm.call();
                self.calls.push((hole, *function));
                // Every register this tracks is caller-saved.
                self.forget();
                if let Some(&result) = inst.results.first() {
                    self.keep(result);
                }
            }
            Op::If {
                condition,
                then_region,
                else_region,
            } => self.conditional(*condition, then_region, else_region, &inst.results),
            Op::Loop { initial, body } => self.repeat(initial, body, &inst.results),
        }
    }

    /// The flags the comparison set, widened back into a word.
    fn compare(&mut self, relation: Relation, left: ValueId, right: ValueId, result: ValueId) {
        let cc = match relation {
            Relation::Equal => EQUAL,
            Relation::Less => BELOW,
        };
        self.pair(left, right);
        self.asm.alu(0x39, Reg::Rax, Reg::Rcx);
        self.asm.set_if(cc, Reg::Rax);
        self.keep(result);
    }

    fn binary(&mut self, op: Binary, left: ValueId, right: ValueId, result: ValueId) {
        let opcode = match op {
            Binary::Add => 0x01,
            Binary::Sub => 0x29,
        };
        self.pair(left, right);
        self.asm.alu(opcode, Reg::Rax, Reg::Rcx);
        self.keep(result);
    }

    fn load_at(&mut self, width: Width, address: ValueId, offset: Offset, result: ValueId) {
        self.read(Reg::Rcx, address);
        let disp = displacement(offset);
        match width {
            Width::Byte => self.asm.load_byte(Reg::Rax, Reg::Rcx, disp),
            Width::Word => self.asm.load_word(Reg::Rax, Reg::Rcx, disp),
        }
        self.keep(result);
    }

    fn store_at(&mut self, width: Width, address: ValueId, offset: Offset, value: ValueId) {
        self.read(Reg::Rax, value);
        self.read(Reg::Rcx, address);
        match width {
            Width::Byte => self
                .asm
                .store_byte(Reg::Rax, Reg::Rcx, displacement(offset)),
            Width::Word => self
                .asm
                .store_word(Reg::Rax, Reg::Rcx, displacement(offset)),
        }
        self.known[Reg::Rax.index()] = None;
    }

    fn platform_call(&mut self, platform: PlatformId, args: &[ValueId], results: &[ValueId]) {
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
            // The kernel picks the address, so nothing here has to.
            "alloc" => {
                self.load_const(Reg::Rax, SYS_MMAP);
                self.load_const(SYSCALL_REGS[0], 0);
                self.read(SYSCALL_REGS[1], args[0]);
                self.load_const(SYSCALL_REGS[2], PROT_READ_WRITE);
                self.load_const(SYSCALL_REGS[3], MAP_PRIVATE_ANONYMOUS);
                self.load_const(SYSCALL_REGS[4], u64::MAX);
                self.load_const(SYSCALL_REGS[5], 0);
            }
            other => panic!("this target provides no `{other}`"),
        }
        self.syscall();
        if let Some(&result) = results.first() {
            self.keep(result);
        }
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
            Terminator::Return(values) => self.leave(values),
            // Emitting nothing would fall into whatever was laid out next.
            Terminator::Unreachable => self.asm.trap(),
        }
    }

    /// Nothing calls the entry, so leaving it is exiting.
    fn leave(&mut self, values: &[ValueId]) {
        if self.in_entry {
            self.load_const(Reg::Rax, SYS_EXIT);
            match values.first() {
                Some(&status) => self.read(Reg::Rdi, status),
                None => self.load_const(Reg::Rdi, 0),
            }
            self.syscall();
            return;
        }

        if let Some(&value) = values.first() {
            self.read(Reg::Rax, value);
        }
        if self.plan.frame > 0 {
            self.asm.close_frame(self.plan.frame);
        }
        self.asm.ret();
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

    /// rax and rcx, which is what every two-operand form here works on.
    fn pair(&mut self, left: ValueId, right: ValueId) {
        self.read(Reg::Rax, left);
        self.read(Reg::Rcx, right);
    }

    /// Write rax to wherever `value` lives.
    fn keep(&mut self, value: ValueId) {
        self.store(value, Reg::Rax);
    }

    fn store(&mut self, value: ValueId, from: Reg) {
        let Source::Slot(offset) = self.plan.source[value.0] else {
            panic!("a computed value needs a slot");
        };
        self.asm.store_slot(from, offset);
        self.known[from.index()] = None;
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

    fn entry(build: impl FnOnce(&mut crate::ir::Builder) -> Terminator) -> Program {
        let (mut program, main) = Program::new("main");
        program.define(main, |b, _| build(b));
        program
    }

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
        let program = crate::hello_world();
        let main = &program.functions()[program.entry().0];
        assert_eq!(Plan::new(&program, main).frame, 0);
    }

    #[test]
    fn a_loop_jumps_backwards_to_its_own_head() {
        let program = entry(|b| {
            let start = b.constant(Class::Word, 1);
            b.loop_(vec![start], Vec::new(), |b, params| {
                let one = b.constant(Class::Word, 1);
                let next = b.binary(Binary::Sub, params[0], one);
                Terminator::Continue(vec![next])
            });
            Terminator::Unreachable
        });

        let bytes = lower(&program).bytes;
        // `ud2` for the unreachable tail, and before it a `jmp rel32` back to
        // the loop head, which is behind us, so the displacement is negative.
        let end = bytes.len() - 2;
        assert_eq!(&bytes[end..], [0x0F, 0x0B]);
        assert_eq!(bytes[end - 5], 0xE9);
        let displacement = i32::from_le_bytes(bytes[end - 4..end].try_into().unwrap());
        assert!(displacement < 0, "a loop must branch backwards");
    }

    #[test]
    fn a_syscall_invalidates_the_registers_it_clobbers() {
        let (mut program, main) = Program::new("main");
        let data = program.intern(b"x".as_slice());
        let write = program.platform("write", vec![Class::Address, Class::Word], Vec::new());
        program.define(main, |b, _| {
            let buf = b.address_of(data);
            let len = b.constant(Class::Word, 1);
            b.platform_call(write, vec![buf, len]);
            // Leave with the status stdout had, so the two registers that
            // both held 1 across the syscall can be told apart.
            let status = b.constant(Class::Word, 1);
            Terminator::Return(vec![status])
        });

        let bytes = lower(&program).bytes;
        assert!(bytes.ends_with(&[
            0x6A, 0x3C, 0x58, // push 60; pop rax -- the syscall destroyed rax
            0x0F, 0x05, //       nothing for rdi  -- it survived still holding 1
        ]));
    }
}
