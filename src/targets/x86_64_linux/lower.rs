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
    Binary, Class, DataId, Function, FunctionId, Instruction, Offset, Op, PlatformId, Program,
    Region, Relation, Terminator, ValueId, Width,
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

/// A word here, in bits, which is what an unfixed class is held at.
const WORD_BITS: u16 = 64;

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

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Const(u64),
    Addr(DataId),
    Slot(u32),
}

/// Assembled until nothing changes. Two facts are only knowable once the
/// addresses are: which jumps reach in a single byte, and whether a frame
/// was ever written to. Shortening a jump can pull another into reach, so
/// this repeats rather than assuming one pass settles it.
pub(super) fn lower(program: &Program) -> Code {
    let mut distant: Vec<bool> = Vec::new();
    let mut framed = vec![true; program.functions().len()];
    loop {
        let (code, overflowed, spilled) = assemble(program, distant.clone(), &framed);
        if !overflowed.is_empty() {
            let widest = overflowed.iter().copied().max().expect("a branch");
            distant.resize(widest + 1, false);
            for branch in overflowed {
                distant[branch] = true;
            }
            continue;
        }
        if framed != spilled {
            framed = spilled;
            continue;
        }
        return code;
    }
}

fn assemble(
    program: &Program,
    distant: Vec<bool>,
    framed: &[bool],
) -> (Code, Vec<usize>, Vec<bool>) {
    let entry = program.entry();
    let mut state = Lowering {
        program,
        entry,
        in_entry: true,
        plan: Plan::empty(program),
        asm: Encoder::new(distant),
        framed: framed.to_vec(),
        spilled: vec![false; program.functions().len()],
        frame: 0,
        relocs: Vec::new(),
        calls: Vec::new(),
        starts: vec![0; program.functions().len()],
        known: [None; 16],
        holds: [None; 16],
        pending: None,
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

    let spilled = state.spilled.clone();
    let (code, overflowed) = state.finish();
    (code, overflowed, spilled)
}

struct Plan {
    source: Vec<Source>,
    /// How many times each value is read, which is what decides whether a
    /// result is worth writing to its slot at all.
    uses: Vec<u32>,
    scratch: u32,
    frame: u32,
}

impl Plan {
    fn empty(program: &Program) -> Self {
        Self {
            source: vec![Source::Slot(0); program.values()],
            uses: vec![0; program.values()],
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
        plan.count(&function.body);
        plan.walk(program, &function.body, &mut widest);
        plan.scratch = plan.frame;
        plan.frame += 8 * u32::try_from(widest).expect("a sane region width");
        plan
    }

    fn walk(&mut self, program: &Program, region: &Region, widest: &mut usize) {
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
                // A conversion that narrows nothing leaves the bits alone,
                // so the result can live wherever the operand does.
                Op::Convert { class, value }
                    if !Encoder::narrows(bits(*class).min(bits(program.class(*value)))) =>
                {
                    let result = inst.results[0];
                    self.source[result.0] = self.source[value.0];
                    // The storage is shared, so its readers are too, and
                    // this conversion is no longer one of them.
                    self.uses[value.0] += self.uses[result.0] - 1;
                }
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
                    self.walk(program, then_region, widest);
                    self.walk(program, else_region, widest);
                }
                Op::Loop { body, .. } => self.walk(program, body, widest),
                _ => {}
            }
        }
        *widest = (*widest).max(transferred(&region.terminator).len());
    }

    /// Every read of every value, so that a result nothing reads is never
    /// written and one read once may stay in a register.
    fn count(&mut self, region: &Region) {
        let mut read = |values: &[ValueId]| {
            for &value in values {
                self.uses[value.0] += 1;
            }
        };
        for inst in &region.instructions {
            match &inst.op {
                Op::Constant { .. } | Op::AddressOf(_) => {}
                Op::Binary { left, right, .. } | Op::Compare { left, right, .. } => {
                    read(&[*left, *right]);
                }
                Op::Load { address, .. } => read(&[*address]),
                Op::Store { address, value, .. } => read(&[*address, *value]),
                Op::Convert { value, .. } => read(&[*value]),
                Op::PlatformCall { args, .. } | Op::Call { args, .. } => read(args),
                Op::If { condition, .. } => read(&[*condition]),
                Op::Loop { initial, .. } => read(initial),
            }
        }
        read(transferred(&region.terminator));
        if let Terminator::Return(values) = &region.terminator {
            read(values);
        }

        for inst in &region.instructions {
            match &inst.op {
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.count(then_region);
                    self.count(else_region);
                }
                Op::Loop { body, .. } => self.count(body),
                _ => {}
            }
        }
    }

    fn give_slot(&mut self, value: ValueId) {
        self.source[value.0] = Source::Slot(self.frame);
        self.frame += 8;
    }
}

/// How wide a class is held, which for anything unfixed is a word.
const fn bits(class: Class) -> u16 {
    match class {
        Class::Fixed { bits } => bits,
        Class::Word | Class::Address => WORD_BITS,
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
    /// And which value, so reading one already in place costs nothing.
    holds: [Option<ValueId>; 16],
    /// A result sitting in rax whose slot has not been written, because its
    /// one reader may take it from the register instead.
    pending: Option<ValueId>,
    /// Which functions were given a frame, and which turned out to write to
    /// one. A function that never spills does not need one opened.
    framed: Vec<bool>,
    spilled: Vec<bool>,
    frame: u32,
    ifs: Vec<IfFrame>,
    loops: Vec<LoopFrame>,
}

impl Lowering<'_> {
    fn finish(self) -> (Code, Vec<usize>) {
        let relocs = self.relocs;
        let (bytes, overflowed) = self.asm.finish();
        (Code { bytes, relocs }, overflowed)
    }

    fn function(&mut self, id: FunctionId) {
        let function = &self.program.functions()[id.0];
        self.in_entry = id.0 == self.entry.0;
        self.starts[id.0] = self.asm.here();
        self.plan = Plan::new(self.program, function);
        self.forget();

        self.frame = if self.framed[id.0] {
            self.plan.frame
        } else {
            0
        };
        if self.frame > 0 {
            self.asm.open_frame(self.frame);
        }
        let opened = self.asm.spilled();
        for (&param, reg) in function.params.iter().zip(ARG_REGS) {
            self.store(param, reg);
        }
        self.region(&function.body);
        self.spilled[id.0] = self.asm.spilled() && !opened;
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
            Op::Convert { class, value } => {
                // Whatever the source made meaningful, kept as far as the
                // target is wide. When that takes no instruction, the plan
                // has already given the result the operand's storage.
                let kept = bits(*class).min(bits(self.program.class(*value)));
                if Encoder::narrows(kept) {
                    self.read(Reg::Rax, *value);
                    self.asm.narrow(kept);
                    self.keep(inst.results[0]);
                }
            }
            Op::PlatformCall { platform, args } => {
                self.platform_call(*platform, args, &inst.results);
            }
            Op::Call { function, args } => {
                for (&arg, reg) in args.iter().zip(ARG_REGS) {
                    self.read(reg, arg);
                }
                self.flush();
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
        // At the operands' width: anything above it is undefined until a
        // `Convert` says otherwise.
        self.pair(left, right);
        self.asm
            .alu_sized(0x39, Reg::Rax, Reg::Rcx, bits(self.program.class(left)));
        self.asm.set_if(cc, Reg::Rax);
        self.keep(result);
    }

    fn binary(&mut self, op: Binary, left: ValueId, right: ValueId, result: ValueId) {
        let opcode = match op {
            Binary::Add => 0x01,
            Binary::Sub => 0x29,
        };
        self.pair(left, right);
        self.asm
            .alu_sized(opcode, Reg::Rax, Reg::Rcx, bits(self.program.class(result)));
        self.keep(result);
    }

    fn load_at(&mut self, width: Width, address: ValueId, offset: Offset, result: ValueId) {
        self.read(Reg::Rcx, address);
        self.flush();
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
            "grow" => {
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
            // The status first: loading rax would settle any result still
            // waiting there, and the status is often that result.
            match values.first() {
                Some(&status) => self.read(Reg::Rdi, status),
                None => self.load_const(Reg::Rdi, 0),
            }
            self.load_const(Reg::Rax, SYS_EXIT);
            self.syscall();
            return;
        }

        if let Some(&value) = values.first() {
            self.read(Reg::Rax, value);
        }
        if self.frame > 0 {
            self.asm.close_frame(self.frame);
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
        self.flush();
        for (index, &destination) in destinations.iter().enumerate() {
            self.asm.load_slot(Reg::Rax, self.scratch(index));
            self.store(destination, Reg::Rax);
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

    /// Write rax to wherever `value` lives -- or not yet, when the value is
    /// read exactly once and that reader may find it still in the register.
    fn keep(&mut self, value: ValueId) {
        debug_assert!(self.pending.is_none(), "a result left unwritten");
        if self.plan.uses[value.0] == 0 {
            self.holds[Reg::Rax.index()] = Some(value);
            return;
        }
        if self.plan.uses[value.0] == 1 {
            self.known[Reg::Rax.index()] = None;
            self.holds[Reg::Rax.index()] = Some(value);
            self.pending = Some(value);
            return;
        }
        self.store(value, Reg::Rax);
    }

    /// Whether a register's occupant and `value` are the same as far as
    /// storage goes, which an aliased conversion makes possible.
    fn shares(&self, held: Option<ValueId>, value: ValueId) -> bool {
        held.is_some_and(|held| self.plan.source[held.0] == self.plan.source[value.0])
    }

    /// Write out whatever rax is still holding on behalf of its slot.
    fn flush(&mut self) {
        if let Some(value) = self.pending.take() {
            self.store(value, Reg::Rax);
        }
    }

    fn store(&mut self, value: ValueId, from: Reg) {
        let Source::Slot(offset) = self.plan.source[value.0] else {
            panic!("a computed value needs a slot");
        };
        self.asm.store_slot(from, offset);
        // The register is unchanged, and now demonstrably holds `value`.
        self.known[from.index()] = None;
        self.holds[from.index()] = Some(value);
    }

    fn read(&mut self, dst: Reg, value: ValueId) {
        if self.shares(self.holds[dst.index()], value) {
            if self.shares(self.pending, value) {
                self.pending = None;
            }
            return;
        }
        // The one reader of a deferred result, wanting it elsewhere: a copy
        // out of rax still beats writing and reading a slot.
        if self.shares(self.pending, value) {
            self.pending = None;
            self.asm.mov_reg(dst, Reg::Rax, true);
            self.known[dst.index()] = None;
            self.holds[dst.index()] = Some(value);
            return;
        }
        if dst.index() == Reg::Rax.index() {
            self.flush();
        }
        match self.plan.source[value.0] {
            Source::Const(constant) => self.load_const(dst, constant),
            Source::Addr(data) => self.load_addr(dst, data),
            Source::Slot(offset) => {
                self.asm.load_slot(dst, offset);
                self.known[dst.index()] = None;
            }
        }
        self.holds[dst.index()] = Some(value);
    }

    /// The shortest encoding that leaves `value` in `dst`. The forms differ
    /// in how they fill the upper bits, so the choice is made over the whole
    /// 64-bit word rather than over any reading of it.
    fn load_const(&mut self, dst: Reg, value: u64) {
        if dst.index() == Reg::Rax.index() {
            self.flush();
        }
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
        self.holds[dst.index()] = None;
    }

    /// Addressed from the instruction pointer, so the program does not care
    /// where it was loaded and needs no relocation at startup.
    fn load_addr(&mut self, dst: Reg, data: DataId) {
        if dst.index() == Reg::Rax.index() {
            self.flush();
        }
        let offset = self.asm.lea_rip(dst);
        self.relocs.push(Reloc { offset, data });
        self.known[dst.index()] = None;
        self.holds[dst.index()] = None;
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
            self.holds[reg.index()] = None;
        }
    }

    /// Control joins here from somewhere else, so nothing is known.
    fn forget(&mut self) {
        self.flush();
        self.wipe();
    }

    const fn wipe(&mut self) {
        self.known = [None; 16];
        self.holds = [None; 16];
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
            0x31, 0xFF,                   // xor edi, edi     (status 0, read
            0x6A, 0x3C,                   //                   before rax, so
            0x58,                         //                   a result still
                                          //                   there survives)
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
        // `ud2` for the unreachable tail, and before it a jump back to the
        // loop head. The head is a few bytes behind, so the short form
        // reaches it and the displacement is negative.
        let end = bytes.len() - 2;
        assert_eq!(&bytes[end..], [0x0F, 0x0B]);
        assert_eq!(bytes[end - 2], 0xEB, "a near loop takes the short jump");
        assert!(
            bytes[end - 1].cast_signed() < 0,
            "a loop must branch backwards"
        );
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
