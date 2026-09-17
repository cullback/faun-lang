pub mod ir;
pub mod targets;

use targets::{Artifact, Target};

const MESSAGE: &[u8] = b"Hello, World!\n";
const MESSAGE_LEN: u64 = MESSAGE.len() as u64;

#[must_use]
pub fn hello_world() -> ir::Program {
    let mut program = ir::Program::new();
    let message = program.intern(MESSAGE);

    let buf = program.data_addr(message);
    let len = program.imm(MESSAGE_LEN);
    program.print(buf, len);

    let status = program.imm(0);
    program.exit(status);

    program
}

#[must_use]
pub fn compile(target: Target, program: &ir::Program) -> Vec<Artifact> {
    target.emit(program)
}
