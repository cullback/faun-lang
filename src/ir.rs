//! The machine IR. Names no register, syscall, or instruction set, so the
//! same program lowers to any target.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vreg(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// The 64-bit word a register takes, whatever it is read as.
    Const(u64),
    Addr(DataId),
}

#[derive(Clone, Debug)]
pub enum Inst {
    Imm {
        dst: Vreg,
        value: u64,
    },
    DataAddr {
        dst: Vreg,
        data: DataId,
    },
    /// Write `len` bytes starting at `buf` to standard output.
    Print {
        buf: Vreg,
        len: Vreg,
    },
    Exit {
        status: Vreg,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    data: Vec<Vec<u8>>,
    insts: Vec<Inst>,
    vregs: usize,
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

    pub fn imm(&mut self, value: u64) -> Vreg {
        let dst = self.fresh();
        self.insts.push(Inst::Imm { dst, value });
        dst
    }

    /// The same word, written the way a negative number reads.
    pub fn imm_signed(&mut self, value: i64) -> Vreg {
        self.imm(value.cast_unsigned())
    }

    pub fn data_addr(&mut self, data: DataId) -> Vreg {
        let dst = self.fresh();
        self.insts.push(Inst::DataAddr { dst, data });
        dst
    }

    pub fn print(&mut self, buf: Vreg, len: Vreg) {
        self.insts.push(Inst::Print { buf, len });
    }

    pub fn exit(&mut self, status: Vreg) {
        self.insts.push(Inst::Exit { status });
    }

    #[must_use]
    pub fn insts(&self) -> &[Inst] {
        &self.insts
    }

    #[must_use]
    pub fn data(&self) -> &[Vec<u8>] {
        &self.data
    }

    /// Every register's definition, indexed by register. Definitions are the
    /// only way to mint one and are pushed in the same order, so this table
    /// saves every target from walking the instructions to resolve an operand.
    #[must_use]
    pub fn values(&self) -> Vec<Value> {
        let mut values = Vec::with_capacity(self.vregs);
        for inst in &self.insts {
            match *inst {
                Inst::Imm { value, .. } => values.push(Value::Const(value)),
                Inst::DataAddr { data, .. } => values.push(Value::Addr(data)),
                Inst::Print { .. } | Inst::Exit { .. } => {}
            }
        }
        debug_assert_eq!(values.len(), self.vregs);
        values
    }

    const fn fresh(&mut self) -> Vreg {
        self.vregs += 1;
        Vreg(self.vregs - 1)
    }
}
