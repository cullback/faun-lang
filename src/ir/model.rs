//! The data model: operations, regions, declarations, and the pools they
//! live in.

use crate::index::index;

// `Symbol` is a name, held as a run of characters in the program's own
// pool. The IR never reads those characters: names are compared and copied
// as integers, and the text comes back only to render one.
index!(
    ValueId, DataId, FunctionId, PlatformId, RegionId, OpId, Symbol
);

/// A run in one of the program's pools: the values an argument list holds,
/// a region's parameters, the classes a signature names, the characters of
/// a name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub(super) start: u32,
    pub(super) len: u32,
}

impl Range {
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

    /// # Panics
    ///
    /// If the program outgrew the four billion entries a pool may hold.
    #[must_use]
    pub fn of(start: usize, len: usize) -> Self {
        Self {
            start: u32::try_from(start).expect("a pool within 4G"),
            len: u32::try_from(len).expect("a run within 4G"),
        }
    }

    pub(super) fn range(self) -> std::ops::Range<usize> {
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
    pub name: Symbol,
    pub params: Range,
    pub returns: Range,
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
        args: Range,
    },
    Call {
        function: FunctionId,
        args: Range,
    },
    If {
        condition: ValueId,
        then_region: RegionId,
        else_region: RegionId,
    },
    /// Runs `body` with `initial`, then with whatever each `Continue`
    /// carries, until a `Break` leaves.
    Loop {
        initial: Range,
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
    pub values: Range,
}

impl Terminator {
    pub const UNREACHABLE: Self = Self {
        exit: Exit::Unreachable,
        values: Range { start: 0, len: 0 },
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub params: Range,
    /// The instructions, contiguous, so that walking one is a scan.
    pub ops: Range,
    pub terminator: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: Symbol,
    /// The values its parameters are, minted with it.
    pub params: Range,
    pub returns: Range,
    pub body: RegionId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub(super) platform: Vec<Platform>,
    /// Every datum end to end. A target adds its own base and nothing else,
    /// so two of them cannot disagree about the layout between.
    pub(super) data: Vec<u8>,
    /// The same, for what a program may write to.
    pub(super) globals: Vec<u8>,
    pub(super) spans: Vec<Span>,
    pub(super) functions: Vec<Function>,
    /// The class of every value, by value.
    pub(super) classes: Vec<Class>,
    /// The classes a signature names: what a platform routine or a function
    /// takes and answers.
    pub(super) signature: Vec<Class>,
    pub(super) ops: Vec<Op>,
    /// What each instruction defines, beside it rather than in it: a pass
    /// that does not care never reads this array.
    pub(super) results: Vec<Range>,
    pub(super) operands: Vec<ValueId>,
    pub(super) regions: Vec<Region>,
    /// Every name the program uses, each once, end to end.
    pub(super) names: Vec<u8>,
    pub(super) symbols: Vec<Range>,
}
impl Program {
    /// Keep a list of existing values, giving back the run it occupies.
    pub(super) fn hold(&mut self, values: &[ValueId]) -> Range {
        let start = u32::try_from(self.operands.len()).expect("a program within 4G operands");
        self.operands.extend_from_slice(values);
        Range {
            start,
            len: u32::try_from(values.len()).expect("a sane arity"),
        }
    }

    /// The name a symbol stands for.
    ///
    /// # Panics
    ///
    /// If the symbol came from another program.
    /// The classes a run of a signature names.
    #[must_use]
    pub fn signature(&self, range: Range) -> &[Class] {
        &self.signature[range.range()]
    }

    /// The name a symbol stands for.
    ///
    /// # Panics
    ///
    /// If the symbol came from another program.
    #[must_use]
    pub fn name(&self, symbol: Symbol) -> &str {
        let at = self.symbols[symbol.index()];
        std::str::from_utf8(&self.names[at.range()]).expect("a name was written as text")
    }

    #[must_use]
    pub fn values(&self, operands: Range) -> &[ValueId] {
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
