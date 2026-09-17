//! Lowering the IR to 6502 code.
//!
//! Every value that is not a constant or an address lives in a two-byte
//! frame slot, reached as `(FP),Y`, so a frame holds at most 128 of them.
//! Two-operand forms read their operands into the scratch pairs and work
//! there, which is unhurried and obviously correct.

use super::asm::{Assembler, Cc};
use super::{
    ARG_COUNT, ARGS, CSP, CSTACK_TOP, Code, FP, FRAMES_TOP, HEAP, HOME_COUNT, HOMES, HOOK_EXIT,
    HOOK_WRITE, LOAD, PTR, R0, T0, T1,
};
use crate::ir::{
    Binary, Class, DataId, Exit, FunctionId, Offset, Op, Origin, Program, RegionId, Relation,
    Terminator, ValueId, Width,
};

/// A word here is the width of an address.
const WORD: i32 = 2;
const WORD_BITS: u16 = 16;

/// The next slot in a frame that holds at most 128 values.
const fn step(frame: u8) -> u8 {
    frame.checked_add(2).expect("a frame within 128 values")
}

/// Where a function keeps what it holds.
#[derive(Clone, Default)]
struct Plan {
    frame: u8,
    homes: Vec<u8>,
}

/// Where each value is first and last touched in emission order, and where
/// the calls are. A value whose span holds a call is live across it.
#[derive(Default)]
struct Spans {
    first: Vec<usize>,
    last: Vec<usize>,
    calls: Vec<usize>,
}

impl Spans {
    fn touch(&mut self, value: ValueId, at: usize) {
        self.first[value.index()] = self.first[value.index()].min(at);
        self.last[value.index()] = self.last[value.index()].max(at);
    }

    /// Whether a call happens while the value still matters. A call's own
    /// argument dies at it and its result is born there, so neither counts.
    fn crosses_call(&self, value: ValueId) -> bool {
        let (first, last) = (self.first[value.index()], self.last[value.index()]);
        first <= last && self.calls.iter().any(|at| first < *at && *at < last)
    }

    /// Everything touched inside a loop stays live for all of it, since the
    /// body runs again.
    fn widen(&mut self, from: usize, to: usize) {
        for value in 0..self.first.len() {
            let (first, last) = (self.first[value], self.last[value]);
            if first <= last && first <= to && last >= from {
                self.first[value] = first.min(from);
                self.last[value] = last.max(to);
            }
        }
    }
}

/// Whether the program calls the named platform routine anywhere.
fn uses_platform(program: &Program, name: &str) -> bool {
    fn within(program: &Program, region: RegionId, name: &str) -> bool {
        program.ops(region).iter().any(|op| match *op {
            Op::PlatformCall { platform, .. } => {
                program.name(program.platforms()[platform.index()].name) == name
            }
            Op::If {
                then_region,
                else_region,
                ..
            } => within(program, then_region, name) || within(program, else_region, name),
            Op::Loop { body, .. } => within(program, body, name),
            _ => false,
        })
    }
    program
        .functions()
        .iter()
        .any(|function| within(program, function.body, name))
}

#[derive(Clone, Copy)]
enum Source {
    Const(u64),
    Addr(DataId),
    /// A zero-page pair. Only a value no call outlives gets one, so a
    /// callee taking the same pair can never be noticed.
    Home(u8),
    Slot(u8),
}

/// Where an instruction can read a value without copying it anywhere first.
#[derive(Clone, Copy)]
enum Operand {
    Imm(u16),
    Zp(u8),
}

/// One byte of an operand, as the instruction reading it takes it.
#[derive(Clone, Copy)]
enum Byte {
    Imm(u8),
    Zp(u8),
}

impl Operand {
    /// The byte at `index`, low first.
    const fn byte(self, index: u8) -> Byte {
        match self {
            Self::Imm(value) => {
                let [low, high] = value.to_le_bytes();
                Byte::Imm(if index == 0 { low } else { high })
            }
            Self::Zp(at) => Byte::Zp(at + index),
        }
    }
}

/// Whether `value` is read anywhere in `region`, nested regions included.
fn region_reads(program: &Program, region: RegionId, value: ValueId) -> bool {
    program
        .values(program.region(region).terminator.values)
        .contains(&value)
        || program
            .ops(region)
            .iter()
            .any(|op| op_reads(program, op, value))
}

fn op_reads(program: &Program, op: &Op, value: ValueId) -> bool {
    let mut found = false;
    for_each_operand(op, program, |read| found |= read == value);
    found
        || match *op {
            Op::If {
                then_region,
                else_region,
                ..
            } => {
                region_reads(program, then_region, value)
                    || region_reads(program, else_region, value)
            }
            Op::Loop { body, .. } => region_reads(program, body, value),
            _ => false,
        }
}

/// How many `continue`s a loop body has. A nested loop's own belongs to it,
/// so the walk stops at one.
fn continues(program: &Program, region: RegionId) -> usize {
    let own = usize::from(program.region(region).terminator.exit == Exit::Continue);
    own + program
        .ops(region)
        .iter()
        .map(|op| match *op {
            Op::If {
                then_region,
                else_region,
                ..
            } => continues(program, then_region) + continues(program, else_region),
            _ => 0,
        })
        .sum::<usize>()
}

/// The chain of regions from a loop body down to its `continue`, each with
/// the index of the instruction descended into. The innermost index is past
/// the last instruction, since the `continue` is that region's terminator.
fn path_to_continue(
    program: &Program,
    region: RegionId,
    path: &mut Vec<(RegionId, usize)>,
) -> bool {
    let ops = program.ops(region);
    if program.region(region).terminator.exit == Exit::Continue {
        path.push((region, ops.len()));
        return true;
    }
    for (index, op) in ops.iter().enumerate() {
        let Op::If {
            then_region,
            else_region,
            ..
        } = *op
        else {
            continue;
        };
        for child in [then_region, else_region] {
            path.push((region, index));
            if path_to_continue(program, child, path) {
                return true;
            }
            path.pop();
        }
    }
    false
}

/// Every value an operation reads, nested regions excluded.
fn for_each_operand(op: &Op, program: &Program, mut each: impl FnMut(ValueId)) {
    match op {
        Op::Constant { .. } | Op::AddressOf(_) => {}
        Op::Binary { left, right, .. } | Op::Compare { left, right, .. } => {
            each(*left);
            each(*right);
        }
        Op::Load { address, .. } => each(*address),
        Op::Store { address, value, .. } => {
            each(*address);
            each(*value);
        }
        Op::Convert { value, .. } => each(*value),
        Op::PlatformCall { args, .. } | Op::Call { args, .. } => {
            program.values(*args).iter().copied().for_each(each);
        }
        Op::If { condition, .. } => each(*condition),
        Op::Loop { initial, .. } => program.values(*initial).iter().copied().for_each(each),
    }
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

    let mut state = Lowering::new(program, Assembler::new(origin, far), data);
    state.starts = (0..program.functions().len())
        .map(|_| state.asm.label())
        .collect();
    state.plans = (0..program.functions().len())
        .map(|id| state.plan(program.functions()[id].body))
        .collect();
    state.reset(heap);
    for id in 0..program.functions().len() {
        state.function(FunctionId::at(id));
    }
    state.emit_pool();

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
    /// The zero-page pairs this function keeps values in.
    homes: Vec<u8>,
    /// Every function's plan, worked out before any code is emitted.
    plans: Vec<Plan>,
    /// Argument blocks the target lays down itself, one per distinct pair of
    /// constant arguments, emitted after the code.
    pool: Vec<(super::asm::Label, [u16; 2])>,
    /// Comparisons that answer in the flags, by value index, and the one
    /// waiting for the `if` that reads it.
    fused: std::collections::HashSet<usize>,
    pending: Option<(ValueId, Relation, ValueId, ValueId)>,
    starts: Vec<super::asm::Label>,
    loops: Vec<(
        Vec<ValueId>,
        super::asm::Label,
        Vec<ValueId>,
        super::asm::Label,
    )>,
    ifs: Vec<(Vec<ValueId>, super::asm::Label)>,
}

impl<'a> Lowering<'a> {
    fn new(program: &'a Program, asm: Assembler, data: u16) -> Self {
        Self {
            program,
            asm,
            data,
            source: vec![Source::Slot(0); program.values_count()],
            frame: 0,
            homes: Vec::new(),
            plans: Vec::new(),
            pool: Vec::new(),
            fused: std::collections::HashSet::new(),
            pending: None,
            starts: Vec::new(),
            loops: Vec::new(),
            ifs: Vec::new(),
        }
    }
}

impl Lowering<'_> {
    /// The hardware stack, the two software stacks, the heap past the end
    /// of the image, then the entry; its result is the program's status.
    fn reset(&mut self, heap: u16) {
        self.asm.ldx_imm(0xFF);
        self.asm.txs();
        if self.plans.iter().any(|plan| plan.frame > 0) {
            self.set_pair(FP, FRAMES_TOP);
        }
        if uses_platform(self.program, "grow") {
            self.set_pair(HEAP, heap);
        }

        let entry = self.starts[self.program.entry().index()];
        self.asm.jsr(entry);
        self.asm.lda_zp(R0);
        self.asm.jmp_abs(HOOK_EXIT);
    }

    fn set_pair(&mut self, pair: u8, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.asm.lda_imm(low);
        self.asm.sta_zp(pair);
        if high != low {
            self.asm.lda_imm(high);
        }
        self.asm.sta_zp(pair + 1);
    }

    fn function(&mut self, id: FunctionId) {
        let function = &self.program.functions()[id.index()];
        let start = self.starts[id.index()];
        self.asm.bind(start);

        let plan = self.plans[id.index()].clone();
        self.frame = plan.frame;
        self.homes = plan.homes;
        let frame = self.frame;
        if frame > 0 {
            self.adjust_frame(frame, true);
        }

        // Arguments arrive in fixed pairs, so a callee takes its own copies
        // before anything it calls can overwrite them.
        let params = self.program.values(function.params);
        for (index, &param) in params.to_vec().iter().enumerate() {
            let pair = ARGS + 2 * u8::try_from(index).expect("a few arguments");
            self.commit(param, pair);
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

    /// Give every value the function holds a place. The most used get a
    /// zero-page pair, where an instruction can reach them in three cycles
    /// rather than the twenty a frame slot costs; the rest get slots. Use
    /// is counted per loop depth, so an inner loop outbids straight line
    /// code for the pairs.
    fn plan(&mut self, body: RegionId) -> Plan {
        let mut weight = vec![0u64; self.program.values_count()];
        let mut uses = vec![0u32; self.program.values_count()];
        let mut held = Vec::new();
        self.survey(body, 0, &mut weight, &mut uses, &mut held);
        // Value numbers are unique across the program, so what one function
        // marks cannot be mistaken for another's.
        self.mark(body, &uses);
        let shared = self.share(body);

        // A fused comparison answers in the flags and a shared value lives
        // in the parameter's place, so neither needs one of its own. The
        // weight a shared value carried belongs to the parameter it joins.
        for &(value, param) in &shared {
            weight[param.index()] += weight[value.index()];
        }
        held.retain(|value| {
            !self.fused.contains(&value.index())
                && !shared.iter().any(|(shared, _)| shared == value)
        });
        held.sort_by_key(|value| (core::cmp::Reverse(weight[value.index()]), value.index()));

        let spans = self.spans(body);
        let plan = self.place(&held, &spans);
        for (value, param) in shared {
            self.source[value.index()] = self.source[param.index()];
        }
        plan
    }

    /// The most used values get a zero-page pair and the rest frame slots,
    /// with room after them to save the pairs, for a function that has
    /// someone to give them back to.
    fn place(&mut self, held: &[ValueId], spans: &Spans) -> Plan {
        let mut plan = Plan::default();
        for &value in held {
            // A callee takes the pairs from the same place, so a value a
            // call outlives cannot hold one; the frame is out of a callee's
            // reach, and that is where such a value goes.
            let exposed = spans.crosses_call(value);
            match u8::try_from(plan.homes.len()) {
                Ok(taken) if taken < HOME_COUNT && !exposed => {
                    let pair = HOMES + 2 * taken;
                    plan.homes.push(pair);
                    self.source[value.index()] = Source::Home(pair);
                }
                _ => {
                    self.source[value.index()] = Source::Slot(plan.frame);
                    plan.frame = step(plan.frame);
                }
            }
        }
        plan
    }

    /// What each value is, and how much the function leans on it.
    fn survey(
        &mut self,
        region: RegionId,
        depth: u32,
        weight: &mut [u64],
        uses: &mut [u32],
        held: &mut Vec<ValueId>,
    ) {
        let program = self.program;
        let each = 1u64 << (2 * depth.min(8));
        for &param in program.values(program.region(region).params) {
            held.push(param);
            weight[param.index()] += each;
        }

        for (op, results) in program.walk(region) {
            match *op {
                Op::Constant { value, .. } => {
                    self.source[results[0].index()] = Source::Const(value);
                    continue;
                }
                Op::AddressOf(data) => {
                    self.source[results[0].index()] = Source::Addr(data);
                    continue;
                }
                _ => {}
            }
            for &result in results {
                held.push(result);
                weight[result.index()] += each;
            }
            for_each_operand(op, program, |value| {
                weight[value.index()] += each;
                uses[value.index()] += 1;
            });
        }
        for &value in program.values(program.region(region).terminator.values) {
            weight[value.index()] += each;
            uses[value.index()] += 1;
        }

        self.survey_within(region, depth, weight, uses, held);
    }

    /// The nested regions after the run they sit in, so each stays one scan
    /// rather than a walk interleaved with recursion. A loop's body counts
    /// for more, since everything in it happens more than once.
    fn survey_within(
        &mut self,
        region: RegionId,
        depth: u32,
        weight: &mut [u64],
        uses: &mut [u32],
        held: &mut Vec<ValueId>,
    ) {
        for op in self.program.ops(region) {
            match *op {
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.survey(then_region, depth, weight, uses, held);
                    self.survey(else_region, depth, weight, uses, held);
                }
                Op::Loop { body, .. } => self.survey(body, depth + 1, weight, uses, held),
                _ => {}
            }
        }
    }

    /// Spans over the whole function, in the order it is emitted.
    fn spans(&self, body: RegionId) -> Spans {
        let count = self.program.values_count();
        let mut spans = Spans {
            first: vec![usize::MAX; count],
            last: vec![0; count],
            calls: Vec::new(),
        };
        let mut at = 0;
        self.span_region(body, &[], &mut at, &mut spans);
        spans
    }

    fn span_region(&self, region: RegionId, params: &[ValueId], at: &mut usize, spans: &mut Spans) {
        let program = self.program;
        for &param in program.values(program.region(region).params) {
            spans.touch(param, *at);
        }
        let steps: Vec<(&Op, &[ValueId])> = program.walk(region).collect();
        for index in self.order(region, params) {
            let (op, results) = steps[index];
            *at += 1;
            for &result in results {
                spans.touch(result, *at);
            }
            for_each_operand(op, program, |value| spans.touch(value, *at));
            match *op {
                Op::Call { .. } => spans.calls.push(*at),
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.span_region(then_region, params, at, spans);
                    self.span_region(else_region, params, at, spans);
                }
                Op::Loop { body, .. } => {
                    let from = *at;
                    let inner = program.values(program.region(body).params).to_vec();
                    self.span_region(body, &inner, at, spans);
                    spans.widen(from, *at);
                }
                _ => {}
            }
        }
        *at += 1;
        for &value in program.values(program.region(region).terminator.values) {
            spans.touch(value, *at);
        }
    }

    /// The instructions a `continue` carries the results of, which are read
    /// nowhere else in the region and so can be computed last.
    fn deferred(&self, region: RegionId) -> Vec<usize> {
        let program = self.program;
        let terminator = program.region(region).terminator;
        if terminator.exit != Exit::Continue {
            return Vec::new();
        }
        let carried = program.values(terminator.values);
        let ops = program.ops(region);
        program
            .walk(region)
            .enumerate()
            .filter_map(|(index, (op, results))| {
                let (Op::Binary { .. }, [value]) = (op, results) else {
                    return None;
                };
                let read_elsewhere = ops.iter().any(|op| op_reads(program, op, *value));
                (carried.contains(value) && !read_elsewhere).then_some(index)
            })
            .collect()
    }

    /// The order a region's instructions are emitted in. Among the deferred
    /// definitions, one that would overwrite a loop parameter goes after
    /// every other that still has to read it, which is what lets the
    /// parameter and its next value share a place.
    fn order(&self, region: RegionId, params: &[ValueId]) -> Vec<usize> {
        let program = self.program;
        let mut deferred = self.deferred(region);
        let ops = program.ops(region);
        let results: Vec<&[ValueId]> = program.walk(region).map(|(_, results)| results).collect();
        let carried = program.values(program.region(region).terminator.values);

        // Which parameter the instruction at `index` would overwrite.
        let overwrites = |index: usize| -> Option<ValueId> {
            let [value] = results[index] else {
                return None;
            };
            let slot = carried.iter().position(|carried| carried == value)?;
            params.get(slot).copied()
        };
        for _ in 0..deferred.len() {
            let clash = (0..deferred.len()).find_map(|a| {
                let param = overwrites(deferred[a])?;
                let b = (a + 1..deferred.len())
                    .find(|&b| op_reads(program, &ops[deferred[b]], param))?;
                Some((a, b))
            });
            let Some((a, b)) = clash else { break };
            let moved = deferred.remove(a);
            deferred.insert(b, moved);
        }

        let mut order: Vec<usize> = (0..ops.len()).filter(|i| !deferred.contains(i)).collect();
        order.extend(deferred);
        order
    }

    /// Loop parameters whose `continue` value can simply overwrite them, so
    /// the back edge copies nothing. A pair is safe when the loop has one
    /// `continue`, the value is computed by arithmetic -- which reads both
    /// low bytes before writing the low result, and so tolerates sharing a
    /// place with its own operands -- and nothing reads the parameter
    /// between that computation and the `continue`.
    fn share(&self, region: RegionId) -> Vec<(ValueId, ValueId)> {
        let program = self.program;
        let mut out: Vec<(ValueId, ValueId)> = Vec::new();
        let mut loops = vec![region];
        while let Some(region) = loops.pop() {
            for op in program.ops(region) {
                match *op {
                    Op::If {
                        then_region,
                        else_region,
                        ..
                    } => {
                        loops.push(then_region);
                        loops.push(else_region);
                    }
                    Op::Loop { body, .. } => {
                        loops.push(body);
                        self.share_loop(body, &mut out);
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn share_loop(&self, body: RegionId, out: &mut Vec<(ValueId, ValueId)>) {
        let program = self.program;
        if continues(program, body) != 1 {
            return;
        }
        let mut path = Vec::new();
        if !path_to_continue(program, body, &mut path) {
            return;
        }
        let (innermost, _) = *path.last().expect("a path to the continue");
        let carried = program
            .values(program.region(innermost).terminator.values)
            .to_vec();
        let params = program.values(program.region(body).params).to_vec();

        for (slot, &value) in carried.iter().enumerate() {
            let Some(&param) = params.get(slot) else {
                continue;
            };
            if value == param
                || params.contains(&value)
                || matches!(
                    self.source[value.index()],
                    Source::Const(_) | Source::Addr(_)
                )
                || out.iter().any(|(v, p)| *v == value || *p == param)
            {
                continue;
            }
            if self.overwrites_safely(&path, &params, value, param)
                && !carried
                    .iter()
                    .enumerate()
                    .any(|(other, &v)| other != slot && v == param)
            {
                out.push((value, param));
            }
        }
    }

    /// Whether computing `value` may write straight into `param`'s place:
    /// it is arithmetic, it runs on the way to the `continue`, and nothing
    /// between it and the `continue` still reads the parameter.
    fn overwrites_safely(
        &self,
        path: &[(RegionId, usize)],
        params: &[ValueId],
        value: ValueId,
        param: ValueId,
    ) -> bool {
        let program = self.program;
        // Where the value is computed, and how far down the path.
        let defined = path.iter().enumerate().find_map(|(level, &(region, _))| {
            let index = program
                .walk(region)
                .position(|(_, results)| results == [value])?;
            Some((level, index))
        });
        let Some((level, index)) = defined else {
            return false;
        };
        let (region, descended) = path[level];
        let ops = program.ops(region);
        if !matches!(ops[index], Op::Binary { .. }) {
            return false;
        }
        // Computed past the instruction the path descends into, so it never
        // runs on the way to this `continue`.
        if level + 1 < path.len() && index > descended {
            return false;
        }

        // Nothing between the computation and the `continue` may read the
        // parameter: the rest of this region in the order it is emitted, and
        // every deeper level in full.
        let order = self.order(region, params);
        let position = order.iter().position(|&at| at == index).expect("emitted");
        let upto = (descended + 1).min(ops.len());
        let after = order[position + 1..]
            .iter()
            .any(|&at| at < upto && op_reads(program, &ops[at], param));
        let deeper = path[level + 1..].iter().any(|&(region, _)| {
            program
                .ops(region)
                .iter()
                .any(|op| op_reads(program, op, param))
        });
        !after && !deeper
    }

    /// Comparisons the `if` that follows is the only reader of. Such a
    /// comparison answers in the flags, so it builds neither the one-or-zero
    /// word nor the place to keep it in.
    fn mark(&mut self, region: RegionId, uses: &[u32]) {
        let program = self.program;
        let ops: Vec<&Op> = program.ops(region).iter().collect();
        for (index, (op, results)) in program.walk(region).enumerate() {
            let (Op::Compare { .. }, [result]) = (op, results) else {
                continue;
            };
            if uses[result.index()] != 1 {
                continue;
            }
            // A constant emits nothing, so one between the two leaves the
            // flags alone; anything else does not.
            let next = ops[index + 1..]
                .iter()
                .find(|op| !matches!(op, Op::Constant { .. } | Op::AddressOf(_)));
            if let Some(Op::If { condition, .. }) = next
                && condition == result
            {
                self.fused.insert(result.index());
            }
        }
        for op in ops {
            match *op {
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.mark(then_region, uses);
                    self.mark(else_region, uses);
                }
                Op::Loop { body, .. } => self.mark(body, uses),
                _ => {}
            }
        }
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
            Source::Home(home) if home == pair => {}
            Source::Home(home) => {
                self.asm.lda_zp(home);
                self.asm.sta_zp(pair);
                self.asm.lda_zp(home + 1);
                self.asm.sta_zp(pair + 1);
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

    /// `value` where an instruction can read it, copied into `scratch` only
    /// when it lives in the frame.
    fn operand(&mut self, value: ValueId, scratch: u8) -> Operand {
        match self.source[value.index()] {
            Source::Const(word) => {
                Operand::Imm(u16::try_from(word).expect("a word this target can hold"))
            }
            Source::Addr(id) => Operand::Imm(self.address(id)),
            Source::Home(home) => Operand::Zp(home),
            Source::Slot(_) => {
                self.read(scratch, value);
                Operand::Zp(scratch)
            }
        }
    }

    /// Where a result is computed: its own pair, or the scratch it will be
    /// written out of.
    fn dest(&self, value: ValueId) -> u8 {
        match self.source[value.index()] {
            Source::Home(home) => home,
            _ => T0,
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

    /// A pair holding `value`'s result reaches wherever the value lives.
    fn commit(&mut self, value: ValueId, pair: u8) {
        match self.source[value.index()] {
            Source::Home(home) if home == pair => {}
            Source::Home(home) => {
                self.asm.lda_zp(pair);
                self.asm.sta_zp(home);
                self.asm.lda_zp(pair + 1);
                self.asm.sta_zp(home + 1);
            }
            Source::Slot(offset) => {
                self.asm.ldy_imm(offset);
                self.asm.lda_zp(pair);
                self.asm.sta_ind_y(FP);
                self.asm.iny();
                self.asm.lda_zp(pair + 1);
                self.asm.sta_ind_y(FP);
            }
            Source::Const(_) | Source::Addr(_) => panic!("a computed value needs a place"),
        }
    }

    /// `write(1, buffer, count)`: the first two through `CSP`, the count in
    /// A and X.
    fn write(&mut self, args: &[ValueId]) {
        let buffer = self.operand(args[0], T1);
        let count = self.operand(args[1], T0);
        if let Some(at) = self.constant(args[0]) {
            let block = self.intern([at, 1]);
            self.asm.lda_imm_half(block, false);
            self.asm.sta_zp(CSP);
            self.asm.lda_imm_half(block, true);
            self.asm.sta_zp(CSP + 1);
        } else {
            self.build_arguments(buffer);
        }
        match count.byte(1) {
            Byte::Imm(high) => self.asm.ldx_imm(high),
            Byte::Zp(at) => self.asm.ldx_zp(at),
        }
        self.load(count.byte(0));
        self.jsr_hook(HOOK_WRITE);
    }

    /// A computed buffer, written into the block below the C stack top.
    fn build_arguments(&mut self, buffer: Operand) {
        self.set_pair(CSP, CSTACK_TOP - 4);
        self.asm.ldy_imm(0);
        self.load(buffer.byte(0));
        self.asm.sta_ind_y(CSP);
        self.asm.iny();
        self.load(buffer.byte(1));
        self.asm.sta_ind_y(CSP);
        self.asm.iny();
        self.asm.lda_imm(1);
        self.asm.sta_ind_y(CSP);
        self.asm.iny();
        self.asm.lda_imm(0);
        self.asm.sta_ind_y(CSP);
    }

    /// The label of a block holding `words`, added to the pool if it is new.
    fn intern(&mut self, words: [u16; 2]) -> super::asm::Label {
        if let Some((label, _)) = self.pool.iter().find(|(_, held)| *held == words) {
            return *label;
        }
        let label = self.asm.label();
        self.pool.push((label, words));
        label
    }

    /// The pool, after the code it belongs to.
    fn emit_pool(&mut self) {
        for (label, words) in std::mem::take(&mut self.pool) {
            self.asm.bind(label);
            for word in words {
                self.asm.word(word);
            }
        }
    }

    fn region(&mut self, region: RegionId) {
        // The program outlives this, so the walk borrows it rather than
        // `self`, and the loop stays one pass over a contiguous run.
        let program = self.program;
        let steps: Vec<(&Op, &[ValueId])> = program.walk(region).collect();
        let params = self
            .loops
            .last()
            .map(|frame| frame.0.clone())
            .unwrap_or_default();
        for index in self.order(region, &params) {
            let (op, results) = steps[index];
            self.instruction(op, results);
        }
        self.terminator(program.region(region).terminator);
    }

    /// Store each source into its destination as one simultaneous
    /// assignment.
    fn transfer(&mut self, sources: &[ValueId], destinations: &[ValueId]) {
        assert!(sources.len() <= 2, "at most two values cross a region edge");
        // A value that already lives where it is going does not move, which
        // is what makes a coalesced loop parameter free.
        let moving: Vec<bool> = sources
            .iter()
            .zip(destinations)
            .map(|(&source, &destination)| !self.same_place(source, destination))
            .collect();
        // Writing one destination can only disturb another pair's source
        // when the two share a place; short of that the moves are
        // independent and need no scratch between them.
        let tangled = destinations
            .iter()
            .enumerate()
            .any(|(index, &destination)| {
                sources
                    .iter()
                    .enumerate()
                    .any(|(other, &source)| other != index && self.same_place(destination, source))
            });
        if tangled {
            self.transfer_through_scratch(sources, destinations, &moving);
            return;
        }
        for (index, &source) in sources.iter().enumerate() {
            if !moving[index] {
                continue;
            }
            match self.operand(source, T0) {
                Operand::Zp(at) => self.commit(destinations[index], at),
                Operand::Imm(value) => {
                    let scratch = if index == 0 { T0 } else { T1 };
                    self.set_pair(scratch, value);
                    self.commit(destinations[index], scratch);
                }
            }
        }
    }

    /// Every source read out before any destination is written, so that one
    /// move cannot destroy another's input.
    fn transfer_through_scratch(
        &mut self,
        sources: &[ValueId],
        destinations: &[ValueId],
        moving: &[bool],
    ) {
        for (index, &source) in sources.iter().enumerate() {
            if moving[index] {
                self.read(if index == 0 { T0 } else { T1 }, source);
            }
        }
        for (index, &destination) in destinations.iter().enumerate() {
            if moving[index] {
                self.commit(destination, if index == 0 { T0 } else { T1 });
            }
        }
    }

    /// Whether two values are held in the very same place.
    fn same_place(&self, left: ValueId, right: ValueId) -> bool {
        match (self.source[left.index()], self.source[right.index()]) {
            (Source::Home(a), Source::Home(b)) | (Source::Slot(a), Source::Slot(b)) => a == b,
            _ => false,
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

        // The operands may load, and the flags have to outlive that, so both
        // are in hand before the comparison itself.
        if let Some((_, relation, left, right)) =
            self.pending.take_if(|(value, ..)| *value == condition)
        {
            let left = self.operand(left, T0);
            let right = self.operand(right, T1);
            self.holds_not(relation, left, right, otherwise);
        } else {
            self.read(T0, condition);
            self.asm.lda_zp(T0);
            self.asm.ora_zp(T0 + 1);
            self.asm.branch(Cc::Equal, otherwise);
        }

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
        self.commit(result, T0);
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
            "write" => self.write(args),
            "exit" => {
                let status = self.operand(args[0], T0);
                self.load(status.byte(0));
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

                self.commit(results[0], T1);
            }
            other => panic!("this target provides no `{other}`"),
        }
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
            self.commit(result, R0);
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
        self.commit(result, T0);
    }

    fn arithmetic(&mut self, op: Binary, left: Operand, right: Operand, dest: u8, narrow: bool) {
        // A step of one over a pair the result already occupies is an
        // increment, which carries into the high byte only on a wrap.
        if let (Operand::Zp(at), Operand::Imm(1), false) = (left, right, narrow)
            && at == dest
        {
            let done = self.asm.label();
            match op {
                Binary::Add => {
                    self.asm.inc_zp(dest);
                    self.asm.branch(Cc::NotEqual, done);
                    self.asm.inc_zp(dest + 1);
                }
                Binary::Sub => {
                    self.asm.lda_zp(dest);
                    self.asm.branch(Cc::NotEqual, done);
                    self.asm.dec_zp(dest + 1);
                }
            }
            self.asm.bind(done);
            if matches!(op, Binary::Sub) {
                self.asm.dec_zp(dest);
            }
            return;
        }
        // Both low bytes are read before the low result is written, and both
        // high bytes before the high one, so the destination may be one of
        // the operands.
        self.load(left.byte(0));
        match op {
            Binary::Add => self.asm.clc(),
            Binary::Sub => self.asm.sec(),
        }
        self.accumulate(op, right.byte(0));
        self.asm.sta_zp(dest);
        if narrow {
            self.asm.lda_imm(0);
        } else {
            self.load(left.byte(1));
            self.accumulate(op, right.byte(1));
        }
        self.asm.sta_zp(dest + 1);
    }

    fn load(&mut self, byte: Byte) {
        match byte {
            Byte::Imm(value) => self.asm.lda_imm(value),
            Byte::Zp(at) => self.asm.lda_zp(at),
        }
    }

    fn accumulate(&mut self, op: Binary, byte: Byte) {
        match (op, byte) {
            (Binary::Add, Byte::Imm(value)) => self.asm.adc_imm(value),
            (Binary::Add, Byte::Zp(at)) => self.asm.adc_zp(at),
            (Binary::Sub, Byte::Imm(value)) => self.asm.sbc_imm(value),
            (Binary::Sub, Byte::Zp(at)) => self.asm.sbc_zp(at),
        }
    }

    fn compare_byte(&mut self, left: Byte, right: Byte) {
        self.load(left);
        match right {
            Byte::Imm(value) => self.asm.cmp_imm(value),
            Byte::Zp(at) => self.asm.cmp_zp(at),
        }
    }

    /// Branch to `yes` when the relation holds, and fall through when it
    /// does not.
    fn holds(&mut self, relation: Relation, left: Operand, right: Operand, yes: super::asm::Label) {
        let no = self.asm.label();
        match relation {
            Relation::Equal => {
                self.compare_byte(left.byte(0), right.byte(0));
                self.asm.branch(Cc::NotEqual, no);
                self.compare_byte(left.byte(1), right.byte(1));
                self.asm.branch(Cc::Equal, yes);
            }
            // Unsigned, high byte first; the low byte only decides a tie.
            Relation::Less => {
                self.compare_byte(left.byte(1), right.byte(1));
                self.asm.branch(Cc::NoCarry, yes);
                self.asm.branch(Cc::NotEqual, no);
                self.compare_byte(left.byte(0), right.byte(0));
                self.asm.branch(Cc::NoCarry, yes);
            }
        }
        self.asm.bind(no);
    }

    /// Branch to `no` when the relation does not hold, and fall through when
    /// it does.
    fn holds_not(
        &mut self,
        relation: Relation,
        left: Operand,
        right: Operand,
        no: super::asm::Label,
    ) {
        match relation {
            Relation::Equal => {
                self.compare_byte(left.byte(0), right.byte(0));
                self.asm.branch(Cc::NotEqual, no);
                self.compare_byte(left.byte(1), right.byte(1));
                self.asm.branch(Cc::NotEqual, no);
            }
            Relation::Less => {
                let yes = self.asm.label();
                self.compare_byte(left.byte(1), right.byte(1));
                self.asm.branch(Cc::NoCarry, yes);
                self.asm.branch(Cc::NotEqual, no);
                self.compare_byte(left.byte(0), right.byte(0));
                self.asm.branch(Cc::Carry, no);
                self.asm.bind(yes);
            }
        }
    }

    /// `dest = left relation right`, as one or zero.
    fn comparison(&mut self, relation: Relation, left: Operand, right: Operand, dest: u8) {
        let yes = self.asm.label();
        let done = self.asm.label();
        self.holds(relation, left, right, yes);
        self.set_pair(dest, 0);
        self.asm.jmp(done);
        self.asm.bind(yes);
        self.set_pair(dest, 1);
        self.asm.bind(done);
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
                let left = self.operand(*left, T0);
                let right = self.operand(*right, T1);
                let narrow = bits(self.program.class(results[0])) <= 8;
                let dest = self.dest(results[0]);
                self.arithmetic(*op, left, right, dest, narrow);
                self.commit(results[0], dest);
            }
            Op::Compare {
                relation,
                left,
                right,
            } => {
                // When the next `if` is the only reader, the flags carry the
                // answer and the one-or-zero word is never built.
                if self.fused.contains(&results[0].index()) {
                    self.pending = Some((results[0], *relation, *left, *right));
                    return;
                }
                let left = self.operand(*left, T0);
                let right = self.operand(*right, T1);
                let dest = self.dest(results[0]);
                self.comparison(*relation, left, right, dest);
                self.commit(results[0], dest);
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
                let name = program.name(program.platforms()[platform.index()].name);
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

    /// `value` when it is known at assembly time, without emitting anything.
    fn constant(&self, value: ValueId) -> Option<u16> {
        match self.source[value.index()] {
            Source::Const(word) => u16::try_from(word).ok(),
            Source::Addr(id) => Some(self.address(id)),
            Source::Home(_) | Source::Slot(_) => None,
        }
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
