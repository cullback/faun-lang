//! A target owns everything below the IR: the machine, the environment it
//! calls into, and the files it writes.
//!
//! - Targets share [`bytes`] and nothing else.
//! - Duplication beats an abstraction fitting neither. Their parallel moves
//!   have no common shape: wasm pushes every source before setting any
//!   destination and lets its operand stack swap, where x86-64 routes
//!   through scratch.

pub(crate) mod bytes;
pub mod mos6502_sim65;
pub mod wasm32_wasi;
pub mod x86_64_linux;

use std::fmt;

use crate::ir::Program;

#[derive(Debug)]
pub struct Artifact {
    /// Appended to the output name, such as `.wasm`. Empty when the target
    /// writes a bare executable.
    pub extension: &'static str,
    pub bytes: Vec<u8>,
    pub executable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    X86_64Linux,
    Wasm32Wasi,
    Mos6502Sim65,
}

impl Target {
    pub const ALL: [Self; 3] = [Self::X86_64Linux, Self::Wasm32Wasi, Self::Mos6502Sim65];

    /// The target native to the machine the compiler is running on, if there
    /// is a backend for it. `None` means `--target` is not optional here.
    pub const HOST: Option<Self> = if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        Some(Self::X86_64Linux)
    } else {
        None
    };

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::Wasm32Wasi => "wasm32-wasi",
            Self::Mos6502Sim65 => "mos6502-sim65",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }

    #[must_use]
    pub fn emit(self, program: &Program) -> Vec<Artifact> {
        match self {
            Self::X86_64Linux => x86_64_linux::emit(program),
            Self::Wasm32Wasi => wasm32_wasi::emit(program),
            Self::Mos6502Sim65 => mos6502_sim65::emit(program),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
