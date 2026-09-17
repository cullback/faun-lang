//! Building well-shaped programs.

use super::model::{
    Binary, Class, DataId, Exit, Function, FunctionId, Offset, Op, Origin, Platform, PlatformId,
    Program, Range, Region, RegionId, Relation, Span, Symbol, Terminator, ValueId, Width, wrap,
};

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
            signature: Vec::new(),
            ops: Vec::new(),
            results: Vec::new(),
            operands: Vec::new(),
            regions: Vec::new(),
            names: Vec::new(),
            symbols: Vec::new(),
        };
        let entry = program.declare(entry, &[], &[Class::Word]);
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

    /// The symbol for `name`, the same symbol every time. This runs once
    /// per declaration and nowhere else, so the scan is over what a program
    /// declares rather than over its terms.
    ///
    /// # Panics
    ///
    /// If the program outgrew the names it may have.
    pub fn symbol(&mut self, name: &str) -> Symbol {
        let held = |at: &Range| &self.names[at.range()] == name.as_bytes();
        if let Some(at) = self.symbols.iter().position(held) {
            return Symbol::at(at);
        }
        let at = self.names.len();
        self.names.extend_from_slice(name.as_bytes());
        self.symbols.push(Range::of(at, name.len()));
        Symbol::at(self.symbols.len() - 1)
    }

    pub fn platform(&mut self, name: &str, params: &[Class], returns: &[Class]) -> PlatformId {
        let name = self.symbol(name);
        let params = self.sign(params);
        let returns = self.sign(returns);
        self.platform.push(Platform {
            name,
            params,
            returns,
        });
        PlatformId::at(self.platform.len() - 1)
    }

    /// Separate from defining, so a body may call a function declared after
    /// it, or itself.
    pub fn declare(&mut self, name: &str, params: &[Class], returns: &[Class]) -> FunctionId {
        let params = self.mint(params);
        self.regions.push(Region {
            params: Range::default(),
            ops: Range::default(),
            terminator: Terminator::UNREACHABLE,
        });
        let name = self.symbol(name);
        let returns = self.sign(returns);
        self.functions.push(Function {
            name,
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
    /// Hold a run of classes, for a signature.
    fn sign(&mut self, classes: &[Class]) -> Range {
        let at = self.signature.len();
        self.signature.extend_from_slice(classes);
        Range::of(at, classes.len())
    }

    /// One value of `class`, and the run of one that names it.
    fn mint_one(&mut self, class: Class) -> (ValueId, Range) {
        let start = self.operands.len();
        self.classes.push(class);
        let value = ValueId::at(self.classes.len() - 1);
        self.operands.push(value);
        (value, Range::of(start, 1))
    }

    /// A run of values for the classes a signature already names, read where
    /// they lie rather than copied out first.
    fn mint_from(&mut self, signature: Range) -> Range {
        let start = self.operands.len();
        for at in signature.range() {
            let class = self.signature[at];
            self.classes.push(class);
            self.operands.push(ValueId::at(self.classes.len() - 1));
        }
        Range::of(start, signature.len())
    }

    fn mint(&mut self, classes: &[Class]) -> Range {
        let start = u32::try_from(self.operands.len()).expect("a program within 4G operands");
        for &class in classes {
            self.classes.push(class);
            self.operands.push(ValueId::at(self.classes.len() - 1));
        }
        Range {
            start,
            len: u32::try_from(classes.len()).expect("a sane arity"),
        }
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
    params: Range,
    ops: Vec<Op>,
    results: Vec<Range>,
}

impl Builder<'_> {
    pub fn constant(&mut self, class: Class, value: u64) -> ValueId {
        let value = wrap(class, value);
        self.push_one(Op::Constant { class, value }, class)
    }

    /// The same word, written the way a negative number reads.
    pub fn constant_signed(&mut self, class: Class, value: i64) -> ValueId {
        self.constant(class, value.cast_unsigned())
    }

    pub fn address_of(&mut self, data: DataId) -> ValueId {
        self.push_one(Op::AddressOf(data), Class::Address)
    }

    pub fn data_len(&mut self, data: DataId) -> ValueId {
        let len = self.program.datum(data).len;
        self.constant(Class::Word, u64::from(len))
    }

    pub fn binary(&mut self, op: Binary, left: ValueId, right: ValueId) -> ValueId {
        let class = self.program.class(left);
        self.push_one(Op::Binary { op, left, right }, class)
    }

    pub fn compare(&mut self, relation: Relation, left: ValueId, right: ValueId) -> ValueId {
        let op = Op::Compare {
            relation,
            left,
            right,
        };
        self.push_one(op, Class::Word)
    }

    pub fn load(&mut self, width: Width, address: ValueId, offset: Offset) -> ValueId {
        let op = Op::Load {
            width,
            address,
            offset,
        };
        self.push_one(op, Class::Word)
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
        self.push_one(Op::Convert { class, value }, class)
    }

    pub fn platform_call(&mut self, platform: PlatformId, args: &[ValueId]) -> Vec<ValueId> {
        let returns = self.program.platform[platform.index()].returns;
        let args = self.program.hold(args);
        self.push_from(Op::PlatformCall { platform, args }, returns)
    }

    pub fn call(&mut self, function: FunctionId, args: &[ValueId]) -> Vec<ValueId> {
        let returns = self.program.functions[function.index()].returns;
        let args = self.program.hold(args);
        self.push_from(Op::Call { function, args }, returns)
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

    /// The same, for results a signature already names.
    fn push_from(&mut self, op: Op, returns: Range) -> Vec<ValueId> {
        let results = self.program.mint_from(returns);
        let values = self.program.values(results).to_vec();
        self.ops.push(op);
        self.results.push(results);
        values
    }

    /// One result, which is every operation but a call and a construct that
    /// carries values out. Answers the value itself, so that the common case
    /// never builds a list to take the first of.
    fn push_one(&mut self, op: Op, class: Class) -> ValueId {
        let (value, results) = self.program.mint_one(class);
        self.ops.push(op);
        self.results.push(results);
        value
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
            ops: Range { start, len },
            terminator,
        }
    }
}
