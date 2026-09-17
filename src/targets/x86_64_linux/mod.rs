//! [`lower`] turns the IR into instructions but cannot know where the data
//! will sit, so it leaves a [`Reloc`] wherever an address belongs; [`elf`]
//! chooses the layout and fills them in.

mod elf;
mod encode;
mod lower;

use crate::ir::{DataId, Program};
use crate::targets::Artifact;

#[must_use]
pub fn emit(program: &Program) -> Vec<Artifact> {
    vec![Artifact {
        extension: "",
        bytes: elf::image(&lower::lower(program), program),
        executable: true,
    }]
}

/// Machine code whose data addresses are not resolved yet.
struct Code {
    bytes: Vec<u8>,
    relocs: Vec<Reloc>,
}

struct Reloc {
    offset: usize,
    data: DataId,
}
