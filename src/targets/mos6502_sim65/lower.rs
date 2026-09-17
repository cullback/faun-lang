//! Lowering the IR to 6502 code.
//!
//! Every value that is not a constant or an address lives in a two-byte
//! frame slot, reached as `(FP),Y`, so a frame holds at most 128 of them.
//! Two-operand forms read their operands into the scratch pairs and work
//! there, which is unhurried and obviously correct.

use super::asm::{Assembler, Cc};
use super::{
    ARG_COUNT, ARGS, CSP, CSTACK_TOP, Code, FP, FRAMES_TOP, HEAP, HOOK_EXIT, HOOK_WRITE, LOAD, PTR,
    R0, T0, T1,
};
use crate::ir::{
    Binary, Class, DataId, Exit, FunctionId, Offset, Op, Origin, Program, RegionId, Relation,
    Terminator, ValueId, Width,
};

/// A word here is the width of an address.
const WORD: i32 = 2;
const WORD_BITS: u16 = 16;

#[derive(Clone, Copy)]
enum Source {
    Const(u64),
    Addr(DataId),
    Slot(u8),
}

/// Assembled until nothing changes. Two things are only knowable once the
/// addresses are: which branches reach their target in two bytes, and where
/// the image ends, since with no kernel and no mapping the heap is whatever
/// address space the image does not occupy. Lengthening a branch can push
/// another out of reach, so this repeats rather than assuming one pass.
pub(super) fn lower(program: &Program) -> Code {
    let mut far: Vec<bool> = Vec::new();
    let mut heap = LOAD;
    loop {
        let (code, overflowed) = assemble(program, heap, far.clone());
        if !overflowed.is_empty() {
            let widest = overflowed.iter().copied().max().expect("a branch");
            far.resize(widest + 1, false);
            for branch in overflowed {
                far[branch] = true;
            }
            continue;
        }

        let end = code.reset + u16::try_from(code.bytes.len()).expect("an image within 64 KiB");
        if end <= heap {
            return code;
        }
        heap = end;
    }
}

fn assemble(program: &Program, heap: u16, far: Vec<bool>) -> (Code, Vec<usize>) {
    // The data sits at the front of the image and the code after it, so the
    // code's own addresses depend on how much data there is.
    let data = LOAD;
    let blob = program.data().len() + program.globals().len();
    let origin = data + u16::try_from(blob).expect("data within 64 KiB");

    let mut state = Lowering {
        program,
        asm: Assembler::new(origin, far),
        data,
        source: vec![Source::Slot(0); program.values_count()],
        frame: 0,
        starts: Vec::new(),
        loops: Vec::new(),
        ifs: Vec::new(),
    };

    state.starts = (0..program.functions().len())
        .map(|_| state.asm.label())
        .collect();
    state.reset(heap);
    for id in 0..program.functions().len() {
        state.function(FunctionId::at(id));
    }

    let (bytes, overflowed) = state.asm.finish();
    (
        Code {
            bytes,
            reset: origin,
        },
        overflowed,
    )
}

struct Lowering<'a> {
    program: &'a Program,
    asm: Assembler,
    data: u16,
    source: Vec<Source>,
    frame: u8,
    starts: Vec<super::asm::Label>,
    loops: Vec<(
        Vec<ValueId>,
        super::asm::Label,
        Vec<ValueId>,
        super::asm::Label,
    )>,
    ifs: Vec<(Vec<ValueId>, super::asm::Label)>,
}

impl Lowering<'_> {
    /// The hardware stack, the two software stacks, the heap past the end
    /// of the image, then the entry; its result is the program's status.
    fn reset(&mut self, heap: u16) {
        self.asm.ldx_imm(0xFF);
        self.asm.txs();
        self.set_pair(CSP, CSTACK_TOP);
        self.set_pair(FP, FRAMES_TOP);
        self.set_pair(HEAP, heap);

        let entry = self.starts[self.program.entry().index()];
        self.asm.jsr(entry);
        self.asm.lda_zp(R0);
        self.asm.jmp_abs(HOOK_EXIT);
    }

    fn set_pair(&mut self, pair: u8, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.asm.lda_imm(low);
        self.asm.sta_zp(pair);
        self.asm.lda_imm(high);
        self.asm.sta_zp(pair + 1);
    }

    fn function(&mut self, id: FunctionId) {
        let function = &self.program.functions()[id.index()];
        let start = self.starts[id.index()];
        self.asm.bind(start);

        self.frame = 0;
        self.plan(function.body);
        let frame = self.frame;
        if frame > 0 {
            self.adjust_frame(frame, true);
        }

        // Arguments arrive in fixed pairs, so a callee takes its own copies
        // before anything it calls can overwrite them.
        let params = self.program.values(function.params);
        for (index, &param) in params.to_vec().iter().enumerate() {
            let pair = ARGS + 2 * u8::try_from(index).expect("a few arguments");
            self.write_slot(param, pair);
        }
        self.region(function.body);
    }

    /// `FP` down by the frame, or back up again.
    fn adjust_frame(&mut self, frame: u8, open: bool) {
        self.asm.lda_zp(FP);
        if open {
            self.asm.sec();
            self.asm.sbc_imm(frame);
        } else {
            self.asm.clc();
            self.asm.adc_imm(frame);
        }
        self.asm.sta_zp(FP);
        self.asm.lda_zp(FP + 1);
        if open {
            self.asm.sbc_imm(0);
        } else {
            self.asm.adc_imm(0);
        }
        self.asm.sta_zp(FP + 1);
    }

    /// Give a slot to every value the frame has to hold.
    fn plan(&mut self, region: RegionId) {
        let program = self.program;
        for &param in program.values(program.region(region).params) {
            self.give_slot(param);
        }

        for (op, results) in program.walk(region) {
            match *op {
                Op::Constant { value, .. } => {
                    self.source[results[0].index()] = Source::Const(value);
                }
                Op::AddressOf(data) => self.source[results[0].index()] = Source::Addr(data),
                _ => {
                    for &result in results {
                        self.give_slot(result);
                    }
                }
            }
        }

        // The nested regions after the run they sit in, so each stays one
        // scan rather than a walk interleaved with recursion.
        for op in program.ops(region) {
            match *op {
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.plan(then_region);
                    self.plan(else_region);
                }
                Op::Loop { body, .. } => self.plan(body),
                _ => {}
            }
        }
    }

    fn give_slot(&mut self, value: ValueId) {
        self.source[value.index()] = Source::Slot(self.frame);
        self.frame = self
            .frame
            .checked_add(2)
            .expect("a frame within 128 values");
    }

    /// Read `value` into a zero-page pair.
    fn read(&mut self, pair: u8, value: ValueId) {
        match self.source[value.index()] {
            Source::Const(word) => {
                let word = u16::try_from(word).expect("a word this target can hold");
                self.set_pair(pair, word);
            }
            Source::Addr(id) => {
                let at = self.address(id);
                self.set_pair(pair, at);
            }
            Source::Slot(offset) => {
                self.asm.ldy_imm(offset);
                self.asm.lda_ind_y(FP);
                self.asm.sta_zp(pair);
                self.asm.iny();
                self.asm.lda_ind_y(FP);
                self.asm.sta_zp(pair + 1);
            }
        }
    }

    /// Where a datum sits. The image is loaded into RAM, so both regions
    /// are writable and they differ only in where they begin.
    fn address(&self, data: DataId) -> u16 {
        let span = self.program.datum(data);
        let base = match span.origin {
            Origin::Data => self.data,
            Origin::Globals => {
                self.data + u16::try_from(self.program.data().len()).expect("in range")
            }
        };
        base + u16::try_from(span.start).expect("in range")
    }

    /// Write a zero-page pair into `value`'s slot.
    fn write_slot(&mut self, value: ValueId, pair: u8) {
        let Source::Slot(offset) = self.source[value.index()] else {
            panic!("a computed value needs a slot");
        };
        self.asm.ldy_imm(offset);
        self.asm.lda_zp(pair);
        self.asm.sta_ind_y(FP);
        self.asm.iny();
        self.asm.lda_zp(pair + 1);
        self.asm.sta_ind_y(FP);
    }
}

impl Lowering<'_> {
    fn region(&mut self, region: RegionId) {
        // The program outlives this, so the walk borrows it rather than
        // `self`, and the loop stays one pass over a contiguous run.
        let program = self.program;
        for (op, results) in program.walk(region) {
            self.instruction(op, results);
        }
        self.terminator(program.region(region).terminator);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one call per op; the length is the patterns"
    )]
    fn instruction(&mut self, op: &Op, results: &[ValueId]) {
        let program = self.program;
        match op {
            Op::Constant { .. } | Op::AddressOf(_) => {}
            Op::Binary { op, left, right } => {
                self.pair(*left, *right);
                let one_byte = bits(self.program.class(results[0])) <= 8;
                self.arithmetic(*op, one_byte);
                self.write_slot(results[0], T0);
            }
            Op::Compare {
                relation,
                left,
                right,
            } => {
                self.pair(*left, *right);
                self.comparison(*relation);
                self.write_slot(results[0], T0);
            }
            Op::Load {
                width,
                address,
                offset,
            } => self.load_at(*width, *address, *offset, results[0]),
            Op::Store {
                width,
                address,
                offset,
                value,
            } => self.store_at(*width, *address, *offset, *value),
            Op::Convert { class, value } => {
                self.convert(*class, *value, results[0]);
            }
            Op::PlatformCall { platform, args } => {
                let name = &program.platforms()[platform.index()].name;
                self.platform_call(name, program.values(*args), results);
            }
            Op::Call { function, args } => {
                self.call(*function, program.values(*args), results);
            }
            Op::If {
                condition,
                then_region,
                else_region,
            } => self.conditional(*condition, *then_region, *else_region, results),
            Op::Loop { initial, body } => {
                self.repeat(program.values(*initial), *body, results);
            }
        }
    }

    /// Whatever the source made meaningful, kept as far as the target is
    /// wide -- which here only ever means dropping the high byte.
    fn convert(&mut self, class: Class, value: ValueId, result: ValueId) {
        self.read(T0, value);
        if bits(class).min(bits(self.program.class(value))) <= 8 {
            self.asm.lda_imm(0);
            self.asm.sta_zp(T0 + 1);
        }
        self.write_slot(result, T0);
    }

    /// Arguments go in fixed pairs, so the callee has to take its copies
    /// before anything it calls overwrites them.
    fn call(&mut self, function: FunctionId, args: &[ValueId], results: &[ValueId]) {
        for (index, &arg) in args.iter().enumerate() {
            let index = u8::try_from(index).expect("a few arguments");
            assert!(index < ARG_COUNT, "at most four arguments");
            self.read(ARGS + 2 * index, arg);
        }
        let target = self.starts[function.index()];
        self.asm.jsr(target);
        if let Some(&result) = results.first() {
            self.write_slot(result, R0);
        }
    }

    /// `T0 = T0 op T1`, low byte then high, carrying between them -- or
    /// only the low byte, when the class is narrow enough that the machine's
    /// own wrapping is the answer.
    fn arithmetic(&mut self, op: Binary, narrow: bool) {
        self.asm.lda_zp(T0);
        match op {
            Binary::Add => {
                self.asm.clc();
                self.asm.adc_zp(T1);
            }
            Binary::Sub => {
                self.asm.sec();
                self.asm.sbc_zp(T1);
            }
        }
        self.asm.sta_zp(T0);
        if narrow {
            self.asm.lda_imm(0);
        } else {
            self.asm.lda_zp(T0 + 1);
            match op {
                Binary::Add => self.asm.adc_zp(T1 + 1),
                Binary::Sub => self.asm.sbc_zp(T1 + 1),
            }
        }
        self.asm.sta_zp(T0 + 1);
    }

    /// `T0 = T0 relation T1`, as one or zero.
    fn comparison(&mut self, relation: Relation) {
        let yes = self.asm.label();
        let done = self.asm.label();

        match relation {
            Relation::Equal => {
                let no = self.asm.label();
                self.asm.lda_zp(T0);
                self.asm.cmp_zp(T1);
                self.asm.branch(Cc::NotEqual, no);
                self.asm.lda_zp(T0 + 1);
                self.asm.cmp_zp(T1 + 1);
                self.asm.branch(Cc::Equal, yes);
                self.asm.bind(no);
            }
            // Unsigned, high byte first; the low byte only decides a tie.
            Relation::Less => {
                let no = self.asm.label();
                self.asm.lda_zp(T0 + 1);
                self.asm.cmp_zp(T1 + 1);
                self.asm.branch(Cc::NoCarry, yes);
                self.asm.branch(Cc::NotEqual, no);
                self.asm.lda_zp(T0);
                self.asm.cmp_zp(T1);
                self.asm.branch(Cc::NoCarry, yes);
                self.asm.bind(no);
            }
        }

        self.set_pair(T0, 0);
        self.asm.jmp(done);
        self.asm.bind(yes);
        self.set_pair(T0, 1);
        self.asm.bind(done);
    }

    fn pair(&mut self, left: ValueId, right: ValueId) {
        self.read(T0, left);
        self.read(T1, right);
    }
}

impl Lowering<'_> {
    fn terminator(&mut self, terminator: Terminator) {
        let program = self.program;
        let values = program.values(terminator.values);
        match terminator.exit {
            Exit::Yield => {
                let (results, end) = self.ifs.last().expect("a yield inside an if").clone();
                self.transfer(values, &results);
                self.asm.jmp(end);
            }
            Exit::Continue => {
                let frame = self.loops.last().expect("a continue inside a loop");
                let (params, head) = (frame.0.clone(), frame.1);
                self.transfer(values, &params);
                self.asm.jmp(head);
            }
            Exit::Break => {
                let frame = self.loops.last().expect("a break inside a loop");
                let (results, end) = (frame.2.clone(), frame.3);
                self.transfer(values, &results);
                self.asm.jmp(end);
            }
            Exit::Return => {
                if let Some(&value) = values.first() {
                    self.read(R0, value);
                }
                if self.frame > 0 {
                    let frame = self.frame;
                    self.adjust_frame(frame, false);
                }
                self.asm.rts();
            }
            // Nothing follows, so nothing has to be emitted to avoid it.
            Exit::Unreachable => self.asm.rts(),
        }
    }

    /// Through the scratch pairs, so a swap cannot overwrite its own input.
    /// Only two values move at a time, which is all any region here carries.
    fn transfer(&mut self, sources: &[ValueId], destinations: &[ValueId]) {
        assert!(sources.len() <= 2, "at most two values cross a region edge");
        for (index, &source) in sources.iter().enumerate() {
            self.read(if index == 0 { T0 } else { T1 }, source);
        }
        for (index, &destination) in destinations.iter().enumerate() {
            self.write_slot(destination, if index == 0 { T0 } else { T1 });
        }
    }

    fn conditional(
        &mut self,
        condition: ValueId,
        then_region: RegionId,
        else_region: RegionId,
        results: &[ValueId],
    ) {
        let otherwise = self.asm.label();
        let end = self.asm.label();

        self.read(T0, condition);
        self.asm.lda_zp(T0);
        self.asm.ora_zp(T0 + 1);
        self.asm.branch(Cc::Equal, otherwise);

        self.ifs.push((results.to_vec(), end));
        self.region(then_region);
        self.asm.bind(otherwise);
        self.region(else_region);
        self.ifs.pop();
        self.asm.bind(end);
    }

    fn repeat(&mut self, initial: &[ValueId], body: RegionId, results: &[ValueId]) {
        let head = self.asm.label();
        let end = self.asm.label();

        let program = self.program;
        let params = program.values(program.region(body).params);
        self.transfer(initial, params);
        self.asm.bind(head);
        self.loops
            .push((params.to_vec(), head, results.to_vec(), end));
        self.region(body);
        self.loops.pop();
        self.asm.bind(end);
    }

    fn load_at(&mut self, width: Width, address: ValueId, offset: Offset, result: ValueId) {
        self.read(PTR, address);
        let at = displacement(offset);
        self.asm.ldy_imm(at);
        self.asm.lda_ind_y(PTR);
        self.asm.sta_zp(T0);
        match width {
            Width::Byte => self.asm.lda_imm(0),
            Width::Word => {
                self.asm.iny();
                self.asm.lda_ind_y(PTR);
            }
        }
        self.asm.sta_zp(T0 + 1);
        self.write_slot(result, T0);
    }

    fn store_at(&mut self, width: Width, address: ValueId, offset: Offset, value: ValueId) {
        self.read(T0, value);
        self.read(PTR, address);
        let at = displacement(offset);
        self.asm.ldy_imm(at);
        self.asm.lda_zp(T0);
        self.asm.sta_ind_y(PTR);
        if matches!(width, Width::Word) {
            self.asm.iny();
            self.asm.lda_zp(T0 + 1);
            self.asm.sta_ind_y(PTR);
        }
    }

    fn platform_call(&mut self, name: &str, args: &[ValueId], results: &[ValueId]) {
        match name {
            // The hook takes the descriptor and the buffer on the C stack,
            // and the count in A and X.
            "write" => {
                self.set_pair(T1, 1);
                self.push(T1);
                self.read(T1, args[0]);
                self.push(T1);
                self.read(T0, args[1]);
                self.asm.ldx_zp(T0 + 1);
                self.asm.lda_zp(T0);
                self.jsr_hook(HOOK_WRITE);
                self.drop_pushed(4);
            }
            "exit" => {
                self.read(T0, args[0]);
                self.asm.lda_zp(T0);
                self.asm.jmp_abs(HOOK_EXIT);
            }
            // There is no kernel to ask, so the region is the next unused
            // memory above the image.
            "grow" => {
                self.read(T0, args[0]);
                self.asm.lda_zp(HEAP);
                self.asm.sta_zp(T1);
                self.asm.lda_zp(HEAP + 1);
                self.asm.sta_zp(T1 + 1);

                self.asm.clc();
                self.asm.lda_zp(HEAP);
                self.asm.adc_zp(T0);
                self.asm.sta_zp(HEAP);
                self.asm.lda_zp(HEAP + 1);
                self.asm.adc_zp(T0 + 1);
                self.asm.sta_zp(HEAP + 1);

                self.write_slot(results[0], T1);
            }
            other => panic!("this target provides no `{other}`"),
        }
    }

    /// The C stack the hooks read, which grows down two bytes at a time.
    fn push(&mut self, pair: u8) {
        self.asm.sec();
        self.asm.lda_zp(CSP);
        self.asm.sbc_imm(2);
        self.asm.sta_zp(CSP);
        self.asm.lda_zp(CSP + 1);
        self.asm.sbc_imm(0);
        self.asm.sta_zp(CSP + 1);

        self.asm.ldy_imm(0);
        self.asm.lda_zp(pair);
        self.asm.sta_ind_y(CSP);
        self.asm.iny();
        self.asm.lda_zp(pair + 1);
        self.asm.sta_ind_y(CSP);
    }

    fn drop_pushed(&mut self, bytes: u8) {
        self.asm.clc();
        self.asm.lda_zp(CSP);
        self.asm.adc_imm(bytes);
        self.asm.sta_zp(CSP);
        self.asm.lda_zp(CSP + 1);
        self.asm.adc_imm(0);
        self.asm.sta_zp(CSP + 1);
    }

    fn jsr_hook(&mut self, at: u16) {
        let back = self.asm.label();
        self.asm.jsr_abs(at, back);
        self.asm.bind(back);
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
fn displacement(offset: Offset) -> u8 {
    let bytes = match offset {
        Offset::Bytes(bytes) => bytes,
        Offset::Words(words) => words * WORD,
    };
    u8::try_from(bytes).expect("an offset a single index register can reach")
}
