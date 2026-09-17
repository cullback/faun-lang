pub mod ir;
pub mod targets;

use ir::{Class, Terminator};
use targets::{Artifact, Target};

const MESSAGE: &[u8] = b"Hello, World!\n";
#[expect(clippy::as_conversions, reason = "TryFrom is not const")]
const MESSAGE_LEN: u64 = MESSAGE.len() as u64;

#[must_use]
pub fn hello_world() -> ir::Program {
    let mut program = ir::Program::new();
    let message = program.intern(MESSAGE);
    let write = program.platform("write", vec![Class::Address, Class::Word], Vec::new());
    let exit = program.platform("exit", vec![Class::Word], Vec::new());

    program.build(|b| {
        let buf = b.address_of(message);
        let len = b.constant(Class::Word, MESSAGE_LEN);
        b.call(write, vec![buf, len]);

        let status = b.constant(Class::Word, 0);
        b.call(exit, vec![status]);
        Terminator::Unreachable
    });

    program
}

#[must_use]
pub fn compile(target: Target, program: &ir::Program) -> Vec<Artifact> {
    target.emit(program)
}
