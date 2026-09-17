pub mod ir;
pub mod targets;

use ir::{Class, Terminator};
use targets::{Artifact, Target};

const MESSAGE: &[u8] = b"Hello, World!\n";

#[must_use]
pub fn hello_world() -> ir::Program {
    let (mut program, main) = ir::Program::new("main");
    let message = program.intern(MESSAGE);
    let write = program.platform("write", vec![Class::Address, Class::Word], Vec::new());

    program.define(main, |b, _| {
        let buf = b.address_of(message);
        let len = b.data_len(message);
        b.platform_call(write, vec![buf, len]);

        let status = b.constant(Class::Word, 0);
        Terminator::Return(vec![status])
    });

    program
}

#[must_use]
pub fn compile(target: Target, program: &ir::Program) -> Vec<Artifact> {
    target.emit(program)
}
