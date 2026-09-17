//! The machine IR. Names no register, syscall, instruction set, or word
//! width.
//!
//! Control flow is nested and never flattened to blocks and jumps:
//!
//! - Structure lowers to jumps in about fifteen lines. The reverse needs a
//!   Relooper, which emscripten and LLVM's wasm backend each carry.
//! - Regions take parameters, so a loop stays the tail call it was written
//!   as and no target reconstructs one.
//!
//! A constant is 64 bits of pattern, not a number:
//!
//! - `-1` and `u64::MAX` are one value. [`Builder::constant_signed`] is a
//!   spelling, not a second representation.
//! - Signedness picks `div` against `idiv` and `jb` against `jl`, so it
//!   belongs to operations. LLVM dropped signed and unsigned integer types;
//!   wasm never had them.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueId(pub usize);

/// The width a value is held at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A machine word: as wide as an address, whatever the target's is.
    /// Lengths, counts, and tags are words.
    Word,
    /// Exactly `bits` bits, for arithmetic whose width the program fixes.
    Fixed { bits: u16 },
    /// The address of a datum.
    Address,
}

/// What a program cannot compute for itself. A target provides these and
/// nothing else.
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    PlatformCall {
        platform: PlatformId,
        args: Vec<ValueId>,
    },
    If {
        condition: ValueId,
        then_region: Region,
        else_region: Region,
    },
    /// Runs `body` with `initial`, again with whatever each `Continue`
    /// carries, until a `Break` leaves with the results.
    Loop {
        initial: Vec<ValueId>,
        body: Region,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub results: Vec<ValueId>,
    pub op: Op,
}

/// A run of instructions and the way it leaves. Regions take parameters
/// rather than joining values afterwards, so a loop is the tail call it was
/// written as and no target has to reconstruct one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub params: Vec<ValueId>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// Leave the enclosing `if`, giving it its results.
    Yield(Vec<ValueId>),
    /// Go round the enclosing loop again with these.
    Continue(Vec<ValueId>),
    /// Leave the enclosing loop, giving it its results.
    Break(Vec<ValueId>),
    Return(Vec<ValueId>),
    /// Control cannot arrive here.
    Unreachable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    platform: Vec<Platform>,
    data: Vec<Vec<u8>>,
    /// `classes[value]` is the single source of a value's class.
    classes: Vec<Class>,
    body: Region,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: Terminator::Unreachable,
        }
    }
}

impl Program {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, bytes: impl Into<Vec<u8>>) -> DataId {
        self.data.push(bytes.into());
        DataId(self.data.len() - 1)
    }

    pub fn platform(&mut self, name: &str, params: Vec<Class>, returns: Vec<Class>) -> PlatformId {
        self.platform.push(Platform {
            name: name.to_owned(),
            params,
            returns,
        });
        PlatformId(self.platform.len() - 1)
    }

    /// Fill in the program's body. The builder mints values, so it holds the
    /// program until the region is finished.
    pub fn build(&mut self, build: impl FnOnce(&mut Builder) -> Terminator) {
        let mut builder = Builder {
            program: self,
            params: Vec::new(),
            instructions: Vec::new(),
        };
        let terminator = build(&mut builder);
        self.body = builder.finish(terminator);
    }

    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        &self.platform
    }

    #[must_use]
    pub fn data(&self) -> &[Vec<u8>] {
        &self.data
    }

    #[must_use]
    pub const fn body(&self) -> &Region {
        &self.body
    }

    #[must_use]
    pub fn class(&self, value: ValueId) -> Class {
        self.classes[value.0]
    }

    #[must_use]
    pub const fn values(&self) -> usize {
        self.classes.len()
    }

    fn fresh(&mut self, class: Class) -> ValueId {
        self.classes.push(class);
        ValueId(self.classes.len() - 1)
    }
}

/// Accumulates one region's instructions.
#[derive(Debug)]
pub struct Builder<'a> {
    program: &'a mut Program,
    params: Vec<ValueId>,
    instructions: Vec<Instruction>,
}

impl Builder<'_> {
    pub fn constant(&mut self, class: Class, value: u64) -> ValueId {
        self.push(Op::Constant { class, value }, vec![class])[0]
    }

    /// The same word, written the way a negative number reads.
    pub fn constant_signed(&mut self, class: Class, value: i64) -> ValueId {
        self.constant(class, value.cast_unsigned())
    }

    pub fn address_of(&mut self, data: DataId) -> ValueId {
        self.push(Op::AddressOf(data), vec![Class::Address])[0]
    }

    pub fn binary(&mut self, op: Binary, left: ValueId, right: ValueId) -> ValueId {
        let class = self.program.class(left);
        self.push(Op::Binary { op, left, right }, vec![class])[0]
    }

    pub fn compare(&mut self, relation: Relation, left: ValueId, right: ValueId) -> ValueId {
        let op = Op::Compare {
            relation,
            left,
            right,
        };
        self.push(op, vec![Class::Word])[0]
    }

    pub fn call(&mut self, platform: PlatformId, args: Vec<ValueId>) -> Vec<ValueId> {
        let returns = self.program.platform[platform.0].returns.clone();
        self.push(Op::PlatformCall { platform, args }, returns)
    }

    pub fn if_(
        &mut self,
        condition: ValueId,
        results: Vec<Class>,
        then: impl FnOnce(&mut Builder) -> Terminator,
        otherwise: impl FnOnce(&mut Builder) -> Terminator,
    ) -> Vec<ValueId> {
        let then_region = self.region(Vec::new(), |builder, _| then(builder));
        let else_region = self.region(Vec::new(), |builder, _| otherwise(builder));
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
        initial: Vec<ValueId>,
        results: Vec<Class>,
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) -> Vec<ValueId> {
        let classes: Vec<_> = initial.iter().map(|&v| self.program.class(v)).collect();
        let body = self.region(classes, build);
        self.push(Op::Loop { initial, body }, results)
    }

    fn region(
        &mut self,
        params: Vec<Class>,
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) -> Region {
        let params: Vec<_> = params
            .into_iter()
            .map(|class| self.program.fresh(class))
            .collect();
        let mut builder = Builder {
            program: self.program,
            params: params.clone(),
            instructions: Vec::new(),
        };
        let terminator = build(&mut builder, &params);
        builder.finish(terminator)
    }

    fn push(&mut self, op: Op, classes: Vec<Class>) -> Vec<ValueId> {
        let results: Vec<_> = classes
            .into_iter()
            .map(|class| self.program.fresh(class))
            .collect();
        self.instructions.push(Instruction {
            results: results.clone(),
            op,
        });
        results
    }

    fn finish(self, terminator: Terminator) -> Region {
        Region {
            params: self.params,
            instructions: self.instructions,
            terminator,
        }
    }
}
