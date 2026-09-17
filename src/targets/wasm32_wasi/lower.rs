//! `if` and `loop` are what wasm already has, so nothing is reconstructed.
//! A word is an `i32`; region parameters and results live in locals.
//!
//! - A transfer pushes every source before setting any destination, leaving
//!   the parallel move to wasm's operand stack.
//! - Returning from `_start` already exits zero, so a program ending in
//!   `exit 0` calls nothing and need not import `proc_exit`, which is most
//!   of an import section.

use super::{CALL, Code, DROP, END, I32_CONST, Segment};
use crate::ir::{Binary, DataId, Instruction, Op, Program, Region, Relation, Terminator, ValueId};
use crate::targets::bytes::{Bytes, len32};

const STDOUT: i32 = 1;

/// Linear memory: the cell `fd_write` reports its byte count into and which
/// nothing reads, then the vectors, four-byte aligned as WASI requires, then
/// the data.
const NWRITTEN: i32 = 0;
const IOVECS: u32 = 8;
const IOVEC_LEN: u32 = 8;

const UNREACHABLE: u8 = 0x00;
const BLOCK: u8 = 0x02;
const LOOP: u8 = 0x03;
const IF: u8 = 0x04;
const ELSE: u8 = 0x05;
const BR: u8 = 0x0C;
const RETURN: u8 = 0x0F;
const LOCAL_GET: u8 = 0x20;
const LOCAL_SET: u8 = 0x21;
const I32_STORE: u8 = 0x36;
const I32_ADD: u8 = 0x6A;
const I32_SUB: u8 = 0x6B;
const I32_EQ: u8 = 0x46;
const I32_LT_U: u8 = 0x49;
const EMPTY: u8 = 0x40;

#[derive(Clone, Copy)]
enum Source {
    Const(u64),
    Addr(DataId),
    Local(u32),
}

pub(super) fn lower(program: &Program) -> Code {
    let mut plan = Plan {
        source: vec![Source::Local(0); program.values()],
        locals: 0,
        writes: 0,
    };
    plan.walk(program, program.body());

    // The data sits after the vectors that will point into it.
    let data_start = IOVECS + IOVEC_LEN * len32(plan.writes);
    let mut blob = Vec::new();
    let mut addrs = Vec::with_capacity(program.data().len());
    for bytes in program.data() {
        addrs.push(data_start + len32(blob.len()));
        blob.extend_from_slice(bytes);
    }

    let writes = plan.writes > 0;
    let mut state = Lowering {
        program,
        iovecs: vec![None; plan.writes],
        plan,
        addrs,
        body: Bytes::default(),
        written: 0,
        depth: 0,
        ifs: Vec::new(),
        loops: Vec::new(),
        imports: Vec::new(),
    };

    // Which imports exist decides their indices, so it has to be settled
    // before anything calls one.
    if writes {
        state.imports.push("fd_write");
    }
    if state.needs_proc_exit(program.body(), true) {
        state.imports.push("proc_exit");
    }
    state.region(program.body());

    Code {
        data: state.segments(data_start, blob),
        locals: state.plan.locals,
        memory: writes,
        imports: state.imports,
        body: state.body.finish(),
    }
}

struct Plan {
    source: Vec<Source>,
    locals: u32,
    writes: usize,
}

impl Plan {
    fn walk(&mut self, program: &Program, region: &Region) {
        for &param in &region.params {
            self.give_local(param);
        }
        for inst in &region.instructions {
            match &inst.op {
                Op::Constant { value, .. } => {
                    self.source[inst.results[0].0] = Source::Const(*value);
                }
                Op::AddressOf(data) => self.source[inst.results[0].0] = Source::Addr(*data),
                _ => {
                    for &result in &inst.results {
                        self.give_local(result);
                    }
                }
            }
            match &inst.op {
                Op::PlatformCall { platform, .. }
                    if program.platforms()[platform.0].name == "write" =>
                {
                    self.writes += 1;
                }
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.walk(program, then_region);
                    self.walk(program, else_region);
                }
                Op::Loop { body, .. } => self.walk(program, body),
                _ => {}
            }
        }
    }

    fn give_local(&mut self, value: ValueId) {
        self.source[value.0] = Source::Local(self.locals);
        self.locals += 1;
    }
}

struct Lowering<'a> {
    program: &'a Program,
    plan: Plan,
    addrs: Vec<u32>,
    body: Bytes,
    /// The constant contents of each call's vector, where they were known.
    iovecs: Vec<Option<(u32, u32)>>,
    written: usize,
    /// How many labels enclose the instruction being emitted.
    depth: usize,
    ifs: Vec<Vec<ValueId>>,
    loops: Vec<(Vec<ValueId>, usize, Vec<ValueId>, usize)>,
    imports: Vec<&'static str>,
}

impl Lowering<'_> {
    /// Falling off the end of `_start` already means success, so an `exit 0`
    /// in last position costs nothing and needs no import.
    fn needs_proc_exit(&self, region: &Region, outermost: bool) -> bool {
        let last = region.instructions.len().saturating_sub(1);
        region
            .instructions
            .iter()
            .enumerate()
            .any(|(index, inst)| match &inst.op {
                Op::PlatformCall { platform, args } => {
                    self.program.platforms()[platform.0].name == "exit"
                        && !(outermost && index == last && self.constant(args[0]) == Some(0))
                }
                Op::If {
                    then_region,
                    else_region,
                    ..
                } => {
                    self.needs_proc_exit(then_region, false)
                        || self.needs_proc_exit(else_region, false)
                }
                Op::Loop { body, .. } => self.needs_proc_exit(body, false),
                _ => false,
            })
    }

    /// The vectors and the data, as regions of memory to initialise.
    fn segments(&self, data_start: u32, blob: Vec<u8>) -> Vec<Segment> {
        let mut vectors = Vec::new();
        for iovec in &self.iovecs {
            // A call whose arguments were not constant fills its own vector.
            let (base, length) = iovec.unwrap_or((0, 0));
            vectors.extend_from_slice(&base.to_le_bytes());
            vectors.extend_from_slice(&length.to_le_bytes());
        }

        let mut data = Vec::new();
        if vectors.iter().any(|&byte| byte != 0) {
            data.push(Segment {
                offset: IOVECS,
                bytes: vectors,
            });
        }
        if !blob.is_empty() {
            data.push(Segment {
                offset: data_start,
                bytes: blob,
            });
        }
        data
    }

    fn region(&mut self, region: &Region) {
        for inst in &region.instructions {
            self.instruction(inst);
        }
        self.terminator(&region.terminator);
    }

    fn instruction(&mut self, inst: &Instruction) {
        match &inst.op {
            Op::Constant { .. } | Op::AddressOf(_) => {}
            Op::Binary { op, left, right } => {
                self.push(*left);
                self.push(*right);
                self.body.byte(match op {
                    Binary::Add => I32_ADD,
                    Binary::Sub => I32_SUB,
                });
                self.set(inst.results[0]);
            }
            Op::Compare {
                relation,
                left,
                right,
            } => {
                self.push(*left);
                self.push(*right);
                self.body.byte(match relation {
                    Relation::Equal => I32_EQ,
                    Relation::Less => I32_LT_U,
                });
                self.set(inst.results[0]);
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

    fn conditional(
        &mut self,
        condition: ValueId,
        then_region: &Region,
        else_region: &Region,
        results: &[ValueId],
    ) {
        self.push(condition);
        self.body.byte(IF);
        self.body.byte(EMPTY);
        self.open();

        self.ifs.push(results.to_vec());
        self.region(then_region);
        self.body.byte(ELSE);
        self.region(else_region);
        self.ifs.pop();

        self.close();
    }

    fn repeat(&mut self, initial: &[ValueId], body: &Region, results: &[ValueId]) {
        self.transfer(initial, &body.params);
        self.body.byte(BLOCK);
        self.body.byte(EMPTY);
        self.open();
        let exit = self.depth - 1;
        self.body.byte(LOOP);
        self.body.byte(EMPTY);
        self.open();
        let again = self.depth - 1;

        self.loops
            .push((body.params.clone(), again, results.to_vec(), exit));
        self.region(body);
        self.loops.pop();

        self.close();
        self.close();
    }

    fn terminator(&mut self, terminator: &Terminator) {
        match terminator {
            // The `end` that closes the if is the branch.
            Terminator::Yield(values) => {
                let results = self.ifs.last().expect("a yield inside an if").clone();
                self.transfer(values, &results);
            }
            Terminator::Continue(values) => {
                let (params, again, ..) = self.loops.last().expect("inside a loop").clone();
                self.transfer(values, &params);
                self.branch(again);
            }
            Terminator::Break(values) => {
                let (_, _, results, exit) = self.loops.last().expect("inside a loop").clone();
                self.transfer(values, &results);
                self.branch(exit);
            }
            Terminator::Return(_) => self.body.byte(RETURN),
            // Falling off the end of `_start` is how a WASI command succeeds.
            Terminator::Unreachable => {
                if self.depth > 0 {
                    self.body.byte(UNREACHABLE);
                }
            }
        }
    }

    fn platform_call(&mut self, platform: crate::ir::PlatformId, args: &[ValueId]) {
        match self.program.platforms()[platform.0].name.as_str() {
            "write" => {
                let index = self.written;
                self.written += 1;
                let iovec = IOVECS + IOVEC_LEN * len32(index);

                // Both known, so the vector ships as initialised data; if
                // not, the call fills it in for itself.
                if let (Some(base), Some(length)) = (self.constant(args[0]), self.constant(args[1]))
                {
                    self.iovecs[index] = Some((base, length));
                } else {
                    self.store(iovec, args[0]);
                    self.store(iovec + 4, args[1]);
                }

                self.constant_i32(STDOUT);
                self.constant_i32(iovec.cast_signed());
                self.constant_i32(1);
                self.constant_i32(NWRITTEN);
                let fd_write = self.import("fd_write").expect("a write imports fd_write");
                self.call(fd_write);
                self.body.byte(DROP); // The errno, which there is nobody to tell.
            }
            "exit" => {
                if let Some(index) = self.import("proc_exit") {
                    self.push(args[0]);
                    self.call(index);
                }
            }
            other => panic!("this target provides no `{other}`"),
        }
    }

    /// Hand `sources` over as `destinations`. Every source is pushed before
    /// any destination is set, so wasm's operand stack does the swapping.
    fn transfer(&mut self, sources: &[ValueId], destinations: &[ValueId]) {
        for &source in sources {
            self.push(source);
        }
        for &destination in destinations.iter().rev() {
            self.set(destination);
        }
    }

    fn push(&mut self, value: ValueId) {
        match self.plan.source[value.0] {
            Source::Const(value) => {
                let narrow = u32::try_from(value).expect("a word this target can hold");
                self.constant_i32(narrow.cast_signed());
            }
            Source::Addr(data) => {
                let address = self.addrs[data.0];
                self.constant_i32(address.cast_signed());
            }
            Source::Local(index) => {
                self.body.byte(LOCAL_GET);
                self.body.uleb(index);
            }
        }
    }

    fn set(&mut self, value: ValueId) {
        let Source::Local(index) = self.plan.source[value.0] else {
            panic!("a computed value needs a local");
        };
        self.body.byte(LOCAL_SET);
        self.body.uleb(index);
    }

    fn store(&mut self, address: u32, value: ValueId) {
        self.constant_i32(address.cast_signed());
        self.push(value);
        self.body.byte(I32_STORE);
        self.body.uleb(2); // Four-byte alignment, as a power of two.
        self.body.uleb(0);
    }

    fn constant(&self, value: ValueId) -> Option<u32> {
        match self.plan.source[value.0] {
            Source::Const(value) => {
                Some(u32::try_from(value).expect("a word this target can hold"))
            }
            Source::Addr(data) => Some(self.addrs[data.0]),
            Source::Local(_) => None,
        }
    }

    fn constant_i32(&mut self, value: i32) {
        self.body.byte(I32_CONST);
        self.body.sleb(value);
    }

    fn import(&self, name: &str) -> Option<u32> {
        let index = self.imports.iter().position(|&each| each == name)?;
        Some(len32(index))
    }

    fn call(&mut self, function: u32) {
        self.body.byte(CALL);
        self.body.uleb(function);
    }

    /// `br` counts labels outwards from the innermost, which is why the
    /// depth a label was opened at is what gets remembered.
    fn branch(&mut self, label: usize) {
        self.body.byte(BR);
        self.body.uleb(len32(self.depth - 1 - label));
    }

    const fn open(&mut self) {
        self.depth += 1;
    }

    fn close(&mut self) {
        self.depth -= 1;
        self.body.byte(END);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Class;

    #[test]
    fn hello_world_needs_no_exit_call() {
        let code = lower(&crate::hello_world());
        assert_eq!(code.imports, ["fd_write"], "no proc_exit");
        assert_eq!(code.locals, 0, "constants need no locals");

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
        assert_eq!(code.data[0].offset, IOVECS);
        // Base 16 is the first byte past the vector itself, length 14.
        assert_eq!(code.data[0].bytes, [16, 0, 0, 0, 14, 0, 0, 0]);
        assert_eq!(code.data[1].bytes, b"Hello, World!\n");
    }

    #[test]
    fn a_nonzero_status_has_to_be_called_in() {
        let mut program = Program::new();
        let exit = program.platform("exit", vec![Class::Word], Vec::new());
        program.build(|b| {
            let status = b.constant(Class::Word, 3);
            b.call(exit, vec![status]);
            Terminator::Unreachable
        });

        let code = lower(&program);
        assert_eq!(code.imports, ["proc_exit"]);
        // The only import, so `proc_exit` is function zero.
        assert_eq!(code.body, [0x41, 0x03, 0x10, 0x00]);
    }

    /// Structured control flow goes out as it came in: a `loop` inside a
    /// `block`, with `br 0` going round and `br 1` leaving.
    #[test]
    fn a_loop_is_a_loop() {
        let mut program = Program::new();
        program.build(|b| {
            let start = b.constant(Class::Word, 1);
            b.loop_(vec![start], Vec::new(), |b, params| {
                let zero = b.constant(Class::Word, 0);
                let done = b.compare(Relation::Equal, params[0], zero);
                b.if_(
                    done,
                    Vec::new(),
                    |_| Terminator::Break(Vec::new()),
                    |b| {
                        let one = b.constant(Class::Word, 1);
                        let next = b.binary(Binary::Sub, params[0], one);
                        Terminator::Continue(vec![next])
                    },
                );
                Terminator::Unreachable
            });
            Terminator::Unreachable
        });

        let body = lower(&program).body;
        assert!(body.starts_with(&[
            0x41, 0x01, // i32.const 1
            0x21, 0x00, // local.set 0   (the loop parameter)
            0x02, 0x40, // block
            0x03, 0x40, // loop
        ]));
        // Both branches sit inside the `if`, so label 0 is the if itself:
        // `br 1` goes round the loop and `br 2` leaves the block.
        assert!(body.windows(2).any(|w| w == [0x0C, 0x01]), "continue");
        assert!(body.windows(2).any(|w| w == [0x0C, 0x02]), "break");
    }
}
