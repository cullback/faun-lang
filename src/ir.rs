//! The machine IR. Names no register, syscall, instruction set, or word
//! width.
//!
//! It decides:
//!
//! - Representation: the form each value takes, and the width it needs
//! - Ownership: where a value is retained and released, and when storage is
//!   unique enough to be reused in place
//! - Layout: what is data, what is global, and what an offset means
//! - Function signatures: arity, and the class of each parameter and result
//!
//! It does not decide:
//!
//! - Which values live in registers and which in memory
//! - Calling conventions, stacks, and frames
//! - Instruction selection and encoding
//! - The target's word size, and what its platform provides
//!
//! Instructions live in one flat array, and everything of variable length --
//! operand lists, the regions a construct holds -- is a range into a pool
//! beside it. Nothing about an instruction is boxed or owned, so replacing
//! one is a store and a pass over them is a linear scan.
//!
//! Control flow is nested and never flattened to blocks and jumps:
//!
//! - Structure lowers to jumps in about fifteen lines. The reverse needs a
//!   Relooper, which emscripten and LLVM's wasm backend each carry.
//! - Regions take parameters, so a loop stays the tail call it was written
//!   as and no target reconstructs one.
//! - A region's instructions are contiguous, so walking one stays sequential
//!   where a graph of blocks would scatter.
//!
//! A constant is 64 bits of pattern, not a number:
//!
//! - `-1` and `u64::MAX` are one value. [`Builder::constant_signed`] is a
//!   spelling, not a second representation.
//! - Signedness picks `div` against `idiv` and `jb` against `jl`, so it
//!   belongs to operations. LLVM dropped signed and unsigned integer types;
//!   wasm never had them.

use std::ops::Range;

use crate::index::index;

index!(ValueId, DataId, FunctionId, PlatformId, RegionId, OpId);

/// A run of values in the program's operand pool. Argument lists, region
/// parameters and the values a terminator carries are all one of these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operands {
    start: u32,
    len: u32,
}

impl Operands {
    /// # Panics
    ///
    /// Never, on any machine whose pointers reach 32 bits.
    #[must_use]
    pub fn len(self) -> usize {
        usize::try_from(self.len).expect("an index that fits a pointer")
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    fn range(self) -> Range<usize> {
        let start = usize::try_from(self.start).expect("an index that fits a pointer");
        start..start + self.len()
    }
}

/// Which of a program's two regions a datum sits in.
///
/// They are kept apart so that a target may map what is only read
/// differently from what is written, and so that a program writing nothing
/// needs no writable region at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Data,
    Globals,
}

/// Where a datum sits, and how long it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub origin: Origin,
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// As wide as an address, whatever the target's is.
    Word,
    /// Arithmetic in Z/2^bits: wrapping is the meaning, not an overflow a
    /// target may or may not have. One wider than `bits` narrows after each
    /// operation, and one narrower synthesizes.
    Fixed {
        bits: u16,
    },
    Address,
}

/// What a program cannot compute for itself. A target provides what it has
/// and rejects the rest by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub name: String,
    pub params: Vec<Class>,
    pub returns: Vec<Class>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Equal,
    /// Unsigned, like every comparison on a word.
    Less,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Byte,
    Word,
}

/// A distance from an address, in units the target scales. `Words` is how a
/// machine with a word narrower than 64 bits keeps its own stride.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offset {
    Bytes(i32),
    Words(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Constant {
        class: Class,
        value: u64,
    },
    AddressOf(DataId),
    Binary {
        op: Binary,
        left: ValueId,
        right: ValueId,
    },
    Compare {
        relation: Relation,
        left: ValueId,
        right: ValueId,
    },
    Load {
        width: Width,
        address: ValueId,
        offset: Offset,
    },
    Store {
        width: Width,
        address: ValueId,
        offset: Offset,
        value: ValueId,
    },
    /// `value` read as `class`, which is the only place a width changes.
    /// Everything else works at the width its operands already have, so a
    /// target need not normalise after each operation.
    Convert {
        class: Class,
        value: ValueId,
    },
    PlatformCall {
        platform: PlatformId,
        args: Operands,
    },
    Call {
        function: FunctionId,
        args: Operands,
    },
    If {
        condition: ValueId,
        then_region: RegionId,
        else_region: RegionId,
    },
    /// Runs `body` with `initial`, then with whatever each `Continue`
    /// carries, until a `Break` leaves.
    Loop {
        initial: Operands,
        body: RegionId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Leave the enclosing `if`, giving it its results.
    Yield,
    Continue,
    Break,
    Return,
    Unreachable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminator {
    pub exit: Exit,
    pub values: Operands,
}

impl Terminator {
    pub const UNREACHABLE: Self = Self {
        exit: Exit::Unreachable,
        values: Operands { start: 0, len: 0 },
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub params: Operands,
    /// The instructions, contiguous, so that walking one is a scan.
    pub ops: Operands,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub params: Operands,
    pub returns: Vec<Class>,
    pub body: RegionId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    platform: Vec<Platform>,
    /// Every datum end to end. A target adds its own base and nothing else,
    /// so two of them cannot disagree about the layout between.
    data: Vec<u8>,
    /// The same, for what a program may write to.
    globals: Vec<u8>,
    spans: Vec<Span>,
    functions: Vec<Function>,
    classes: Vec<Class>,
    ops: Vec<Op>,
    /// What each instruction defines, beside it rather than in it: a pass
    /// that does not care never reads this array.
    results: Vec<Operands>,
    operands: Vec<ValueId>,
    regions: Vec<Region>,
}

impl Program {
    /// A program and its entry function, which takes nothing and returns one
    /// word, the exit status. Minting it here is what makes a program without
    /// an entry unrepresentable.
    #[must_use]
    pub fn new(entry: &str) -> (Self, FunctionId) {
        let mut program = Self {
            platform: Vec::new(),
            data: Vec::new(),
            globals: Vec::new(),
            spans: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            ops: Vec::new(),
            results: Vec::new(),
            operands: Vec::new(),
            regions: Vec::new(),
        };
        let entry = program.declare(entry, &[], vec![Class::Word]);
        (program, entry)
    }

    /// # Panics
    ///
    /// If the program's data outgrows the 4 GiB a target can address.
    pub fn intern(&mut self, bytes: &[u8]) -> DataId {
        Self::place(&mut self.data, &mut self.spans, Origin::Data, bytes)
    }

    /// The same, for a datum the program writes to. Its initial contents are
    /// in the image, so a counter starting at zero is eight zero bytes.
    ///
    /// # Panics
    ///
    /// If the program's data outgrows the 4 GiB a target can address.
    pub fn global(&mut self, bytes: &[u8]) -> DataId {
        Self::place(&mut self.globals, &mut self.spans, Origin::Globals, bytes)
    }

    fn place(blob: &mut Vec<u8>, spans: &mut Vec<Span>, origin: Origin, bytes: &[u8]) -> DataId {
        let start = u32::try_from(blob.len()).expect("data within 4 GiB");
        let len = u32::try_from(bytes.len()).expect("a datum within 4 GiB");
        blob.extend_from_slice(bytes);
        spans.push(Span { origin, start, len });
        DataId::at(spans.len() - 1)
    }

    pub fn platform(&mut self, name: &str, params: Vec<Class>, returns: Vec<Class>) -> PlatformId {
        self.platform.push(Platform {
            name: name.to_owned(),
            params,
            returns,
        });
        PlatformId::at(self.platform.len() - 1)
    }

    /// Separate from defining, so a body may call a function declared after
    /// it, or itself.
    pub fn declare(&mut self, name: &str, params: &[Class], returns: Vec<Class>) -> FunctionId {
        let params = self.mint(params);
        self.regions.push(Region {
            params: Operands::default(),
            ops: Operands::default(),
            terminator: Terminator::UNREACHABLE,
        });
        self.functions.push(Function {
            name: name.to_owned(),
            params,
            returns,
            body: RegionId::at(self.regions.len() - 1),
        });
        FunctionId::at(self.functions.len() - 1)
    }

    pub fn define(
        &mut self,
        function: FunctionId,
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) {
        let entry = self.functions[function.index()].params;
        let body = self.functions[function.index()].body;
        let params: Vec<_> = self.values(entry).to_vec();

        let mut builder = Builder {
            program: self,
            params: entry,
            ops: Vec::new(),
            results: Vec::new(),
        };
        let terminator = build(&mut builder, &params);
        let region = builder.finish(terminator);
        self.regions[body.index()] = region;
    }

    /// Mint one value per class, and give back the run they occupy.
    fn mint(&mut self, classes: &[Class]) -> Operands {
        let start = u32::try_from(self.operands.len()).expect("a program within 4G operands");
        for &class in classes {
            self.classes.push(class);
            self.operands.push(ValueId::at(self.classes.len() - 1));
        }
        Operands {
            start,
            len: u32::try_from(classes.len()).expect("a sane arity"),
        }
    }

    /// Keep a list of existing values, giving back the run it occupies.
    fn hold(&mut self, values: &[ValueId]) -> Operands {
        let start = u32::try_from(self.operands.len()).expect("a program within 4G operands");
        self.operands.extend_from_slice(values);
        Operands {
            start,
            len: u32::try_from(values.len()).expect("a sane arity"),
        }
    }

    #[must_use]
    pub fn values(&self, operands: Operands) -> &[ValueId] {
        &self.operands[operands.range()]
    }

    #[must_use]
    pub fn ops(&self, region: RegionId) -> &[Op] {
        let ops = self.regions[region.index()].ops;
        &self.ops[ops.range()]
    }

    /// The instructions of a region, each with what it defines.
    pub fn walk(&self, region: RegionId) -> impl Iterator<Item = (&Op, &[ValueId])> {
        let ops = self.regions[region.index()].ops;
        self.ops[ops.range()]
            .iter()
            .zip(&self.results[ops.range()])
            .map(|(op, results)| (op, &self.operands[results.range()]))
    }

    #[must_use]
    pub fn region(&self, region: RegionId) -> Region {
        self.regions[region.index()]
    }

    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        &self.platform
    }

    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// Always the first, since [`Program::new`] declares it.
    #[must_use]
    pub const fn entry(&self) -> FunctionId {
        FunctionId(0)
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[must_use]
    pub fn globals(&self) -> &[u8] {
        &self.globals
    }

    #[must_use]
    pub fn datum(&self, data: DataId) -> Span {
        self.spans[data.index()]
    }

    #[must_use]
    pub fn class(&self, value: ValueId) -> Class {
        self.classes[value.index()]
    }

    #[must_use]
    pub const fn values_count(&self) -> usize {
        self.classes.len()
    }
}

/// A value taken modulo its class, which for [`Class::Fixed`] is the only
/// reading there is.
#[must_use]
pub const fn wrap(class: Class, value: u64) -> u64 {
    match class {
        Class::Fixed { bits } if bits < 64 => value & ((1 << bits) - 1),
        _ => value,
    }
}

/// Accumulates one region.
///
/// Its instructions are appended to the program only when the region is
/// finished, which is what keeps a region's run of them contiguous while
/// nested regions are being built inside it.
#[derive(Debug)]
pub struct Builder<'a> {
    program: &'a mut Program,
    params: Operands,
    ops: Vec<Op>,
    results: Vec<Operands>,
}

impl Builder<'_> {
    pub fn constant(&mut self, class: Class, value: u64) -> ValueId {
        let value = wrap(class, value);
        self.push(Op::Constant { class, value }, &[class])[0]
    }

    /// The same word, written the way a negative number reads.
    pub fn constant_signed(&mut self, class: Class, value: i64) -> ValueId {
        self.constant(class, value.cast_unsigned())
    }

    pub fn address_of(&mut self, data: DataId) -> ValueId {
        self.push(Op::AddressOf(data), &[Class::Address])[0]
    }

    pub fn data_len(&mut self, data: DataId) -> ValueId {
        let len = self.program.datum(data).len;
        self.constant(Class::Word, u64::from(len))
    }

    pub fn binary(&mut self, op: Binary, left: ValueId, right: ValueId) -> ValueId {
        let class = self.program.class(left);
        self.push(Op::Binary { op, left, right }, &[class])[0]
    }

    pub fn compare(&mut self, relation: Relation, left: ValueId, right: ValueId) -> ValueId {
        let op = Op::Compare {
            relation,
            left,
            right,
        };
        self.push(op, &[Class::Word])[0]
    }

    pub fn load(&mut self, width: Width, address: ValueId, offset: Offset) -> ValueId {
        let op = Op::Load {
            width,
            address,
            offset,
        };
        self.push(op, &[Class::Word])[0]
    }

    pub fn store(&mut self, width: Width, address: ValueId, offset: Offset, value: ValueId) {
        let op = Op::Store {
            width,
            address,
            offset,
            value,
        };
        self.push(op, &[]);
    }

    pub fn convert(&mut self, class: Class, value: ValueId) -> ValueId {
        self.push(Op::Convert { class, value }, &[class])[0]
    }

    pub fn platform_call(&mut self, platform: PlatformId, args: &[ValueId]) -> Vec<ValueId> {
        let returns = self.program.platform[platform.index()].returns.clone();
        let args = self.program.hold(args);
        self.push(Op::PlatformCall { platform, args }, &returns)
    }

    pub fn call(&mut self, function: FunctionId, args: &[ValueId]) -> Vec<ValueId> {
        let returns = self.program.functions[function.index()].returns.clone();
        let args = self.program.hold(args);
        self.push(Op::Call { function, args }, &returns)
    }

    pub fn if_(
        &mut self,
        condition: ValueId,
        results: &[Class],
        then: impl FnOnce(&mut Builder) -> Terminator,
        otherwise: impl FnOnce(&mut Builder) -> Terminator,
    ) -> Vec<ValueId> {
        let then_region = self.region(&[], |builder, _| then(builder));
        let else_region = self.region(&[], |builder, _| otherwise(builder));
        let op = Op::If {
            condition,
            then_region,
            else_region,
        };
        self.push(op, results)
    }

    /// The body runs with `initial`, then with whatever each `Continue`
    /// carries. `results` are the classes a `Break` leaves with.
    pub fn loop_(
        &mut self,
        initial: &[ValueId],
        results: &[Class],
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) -> Vec<ValueId> {
        let classes: Vec<_> = initial
            .iter()
            .map(|&value| self.program.class(value))
            .collect();
        let body = self.region(&classes, build);
        let initial = self.program.hold(initial);
        self.push(Op::Loop { initial, body }, results)
    }

    // The exits, which have to intern the values they carry.

    pub fn ret(&mut self, values: &[ValueId]) -> Terminator {
        self.exit(Exit::Return, values)
    }

    pub fn yield_(&mut self, values: &[ValueId]) -> Terminator {
        self.exit(Exit::Yield, values)
    }

    pub fn continue_(&mut self, values: &[ValueId]) -> Terminator {
        self.exit(Exit::Continue, values)
    }

    pub fn break_(&mut self, values: &[ValueId]) -> Terminator {
        self.exit(Exit::Break, values)
    }

    fn exit(&mut self, exit: Exit, values: &[ValueId]) -> Terminator {
        Terminator {
            exit,
            values: self.program.hold(values),
        }
    }

    fn region(
        &mut self,
        params: &[Class],
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) -> RegionId {
        let entry = self.program.mint(params);
        let params: Vec<_> = self.program.values(entry).to_vec();

        let mut builder = Builder {
            program: self.program,
            params: entry,
            ops: Vec::new(),
            results: Vec::new(),
        };
        let terminator = build(&mut builder, &params);
        let region = builder.finish(terminator);

        self.program.regions.push(region);
        RegionId::at(self.program.regions.len() - 1)
    }

    fn push(&mut self, op: Op, classes: &[Class]) -> Vec<ValueId> {
        let results = self.program.mint(classes);
        let values = self.program.values(results).to_vec();
        self.ops.push(op);
        self.results.push(results);
        values
    }

    /// Append this region's instructions to the program, where they become
    /// one contiguous run.
    fn finish(self, terminator: Terminator) -> Region {
        let start = u32::try_from(self.program.ops.len()).expect("a program within 4G ops");
        let len = u32::try_from(self.ops.len()).expect("a region within 4G ops");
        self.program.ops.extend_from_slice(&self.ops);
        self.program.results.extend_from_slice(&self.results);

        Region {
            params: self.params,
            ops: Operands { start, len },
            terminator,
        }
    }
}

/// What a program has to be true of before a target sees it. These are the
/// mistakes a builder can make that a target would otherwise miscompile in
/// silence rather than reject.
///
/// # Errors
///
/// Names the first disagreement found.
pub fn validate(program: &Program) -> Result<(), String> {
    for function in program.functions() {
        check(program, &function.name, function.body, &function.returns)?;
    }
    Ok(())
}

fn check(program: &Program, name: &str, region: RegionId, returns: &[Class]) -> Result<(), String> {
    for op in program.ops(region) {
        match *op {
            Op::Binary { left, right, .. } | Op::Compare { left, right, .. } => {
                let (left, right) = (program.class(left), program.class(right));
                if left != right {
                    return Err(format!("{name}: {left:?} and {right:?} in one operation"));
                }
            }
            Op::If {
                then_region,
                else_region,
                ..
            } => {
                check(program, name, then_region, returns)?;
                check(program, name, else_region, returns)?;
            }
            Op::Loop { body, .. } => check(program, name, body, returns)?,
            _ => {}
        }
    }

    let terminator = program.region(region).terminator;
    if terminator.exit == Exit::Return {
        let found: Vec<_> = program
            .values(terminator.values)
            .iter()
            .map(|&value| program.class(value))
            .collect();
        if found != returns {
            return Err(format!(
                "{name} returns {returns:?} but leaves with {found:?}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mistake `Convert` exists to make visible: an entry that says it
    /// leaves with a word, leaving with eight bits instead.
    #[test]
    fn a_width_that_changes_without_saying_so_is_rejected() {
        let (mut program, main) = Program::new("main");
        program.define(main, |b, _| {
            let byte = b.constant(Class::Fixed { bits: 8 }, 250);
            b.ret(&[byte])
        });

        let error = validate(&program).unwrap_err();
        assert!(error.contains("main returns"), "{error}");

        let (mut program, main) = Program::new("main");
        program.define(main, |b, _| {
            let byte = b.constant(Class::Fixed { bits: 8 }, 250);
            let word = b.convert(Class::Word, byte);
            b.ret(&[word])
        });
        assert!(validate(&program).is_ok());
    }

    #[test]
    fn operands_of_one_operation_must_agree() {
        let (mut program, main) = Program::new("main");
        program.define(main, |b, _| {
            let byte = b.constant(Class::Fixed { bits: 8 }, 1);
            let word = b.constant(Class::Word, 1);
            let sum = b.binary(Binary::Add, byte, word);
            let sum = b.convert(Class::Word, sum);
            b.ret(&[sum])
        });

        let error = validate(&program).unwrap_err();
        assert!(error.contains("in one operation"), "{error}");
    }

    #[test]
    fn an_instruction_stays_small() {
        assert!(size_of::<Op>() <= 24, "{} bytes", size_of::<Op>());
    }
}
