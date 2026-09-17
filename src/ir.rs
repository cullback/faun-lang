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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FunctionId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// As wide as an address, whatever the target's is.
    Word,
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
pub enum Width {
    Byte,
    Word,
}

/// A distance from an address, in units the target scales. `Words` is how a
/// machine with a word narrower than 64 bits keeps its own stride.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offset {
    Bytes(i64),
    Words(i64),
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
    PlatformCall {
        platform: PlatformId,
        args: Vec<ValueId>,
    },
    Call {
        function: FunctionId,
        args: Vec<ValueId>,
    },
    If {
        condition: ValueId,
        then_region: Region,
        else_region: Region,
    },
    /// Runs `body` with `initial`, then with whatever each `Continue`
    /// carries, until a `Break` leaves.
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
    Continue(Vec<ValueId>),
    Break(Vec<ValueId>),
    Return(Vec<ValueId>),
    Unreachable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub params: Vec<ValueId>,
    pub returns: Vec<Class>,
    pub body: Region,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    platform: Vec<Platform>,
    /// Every datum end to end. A target adds its own base and nothing else,
    /// so two of them cannot disagree about the layout between.
    data: Vec<u8>,
    /// Each datum's start and length within `data`.
    spans: Vec<(u32, u32)>,
    functions: Vec<Function>,
    classes: Vec<Class>,
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
    /// A program and its entry function, which takes nothing and returns one
    /// word, the exit status. Minting it here is what makes a program without
    /// an entry unrepresentable.
    #[must_use]
    pub fn new(entry: &str) -> (Self, FunctionId) {
        let mut program = Self {
            platform: Vec::new(),
            data: Vec::new(),
            spans: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
        };
        let entry = program.declare(entry, Vec::new(), vec![Class::Word]);
        (program, entry)
    }

    /// # Panics
    ///
    /// If the program's data outgrows the 4 GiB a target can address.
    pub fn intern(&mut self, bytes: &[u8]) -> DataId {
        let start = u32::try_from(self.data.len()).expect("data within 4 GiB");
        let length = u32::try_from(bytes.len()).expect("a datum within 4 GiB");
        self.data.extend_from_slice(bytes);
        self.spans.push((start, length));
        DataId(self.spans.len() - 1)
    }

    pub fn platform(&mut self, name: &str, params: Vec<Class>, returns: Vec<Class>) -> PlatformId {
        self.platform.push(Platform {
            name: name.to_owned(),
            params,
            returns,
        });
        PlatformId(self.platform.len() - 1)
    }

    /// Separate from defining, so a body may call a function declared after
    /// it, or itself.
    pub fn declare(&mut self, name: &str, params: Vec<Class>, returns: Vec<Class>) -> FunctionId {
        let params = params.into_iter().map(|class| self.fresh(class)).collect();
        self.functions.push(Function {
            name: name.to_owned(),
            params,
            returns,
            body: Region::default(),
        });
        FunctionId(self.functions.len() - 1)
    }

    pub fn define(
        &mut self,
        function: FunctionId,
        build: impl FnOnce(&mut Builder, &[ValueId]) -> Terminator,
    ) {
        let params = self.functions[function.0].params.clone();
        let body = {
            let mut builder = Builder {
                program: self,
                params: params.clone(),
                instructions: Vec::new(),
            };
            let terminator = build(&mut builder, &params);
            builder.finish(terminator)
        };
        self.functions[function.0].body = body;
    }

    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        &self.platform
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[must_use]
    pub fn datum(&self, data: DataId) -> (u32, u32) {
        self.spans[data.0]
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

    pub fn data_len(&mut self, data: DataId) -> ValueId {
        let (_, length) = self.program.datum(data);
        self.constant(Class::Word, u64::from(length))
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

    pub fn load(&mut self, width: Width, address: ValueId, offset: Offset) -> ValueId {
        let op = Op::Load {
            width,
            address,
            offset,
        };
        self.push(op, vec![Class::Word])[0]
    }

    pub fn store(&mut self, width: Width, address: ValueId, offset: Offset, value: ValueId) {
        let op = Op::Store {
            width,
            address,
            offset,
            value,
        };
        self.push(op, Vec::new());
    }

    pub fn platform_call(&mut self, platform: PlatformId, args: Vec<ValueId>) -> Vec<ValueId> {
        let returns = self.program.platform[platform.0].returns.clone();
        self.push(Op::PlatformCall { platform, args }, returns)
    }

    pub fn call(&mut self, function: FunctionId, args: Vec<ValueId>) -> Vec<ValueId> {
        let returns = self.program.functions[function.0].returns.clone();
        self.push(Op::Call { function, args }, returns)
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

    /// `results` are the classes a `Break` leaves with.
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
