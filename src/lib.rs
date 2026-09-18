pub mod barb;
mod index;
pub mod ir;
pub mod targets;

use ir::Class;
use targets::{Artifact, Target};

const MESSAGE: &[u8] = b"Hello, World!\n";

#[must_use]
pub fn hello_world() -> ir::Program {
    let (mut program, main) = ir::Program::new("main");
    let message = program.intern(MESSAGE);
    let write = program.platform("write", &[Class::Address, Class::Word], &[]);

    program.define(main, |b, _| {
        let buf = b.address_of(message);
        let len = b.data_len(message);
        b.platform_call(write, &[buf, len]);

        let status = b.constant(Class::Word, 0);
        b.ret(&[status])
    });

    program
}

/// `2 + 2`, written in barb.
///
/// `Nat` is the inductive type it is, so addition recurses on its second
/// argument and the literal is a known value. Nothing here says a number is
/// a word; the machine tier decides that when it lowers this.
#[must_use]
pub fn two_and_two() -> barb::Program {
    let mut program = barb::Program::default();
    let nat = program.declare_type("Nat");
    program.define_type(nat, &[("Zero", &[]), ("Succ", &[nat])]);

    let add = program.declare("add", &[nat, nat], nat);
    program.define(add, |b, args| {
        let (left, right) = (args[0], args[1]);
        b.match_(right, nat, |b, ctor, fields| match fields {
            [] => left,
            [k] => {
                let rest = b.call(add, &[left, *k]);
                b.con(ctor, &[rest])
            }
            _ => unreachable!("Nat takes at most one field"),
        })
    });

    let two = program.intern_number(nat, 2);
    let main = program.declare("main", &[], nat);
    program.define(main, |b, _| {
        let value = b.known(nat, two);
        b.call(add, &[value, value])
    });
    program.set_entry(main);
    program
}

/// Read a program and lower it to the tier a target compiles: what it says,
/// then what each definition computes, then how to compute it.
///
/// # Errors
///
/// Names what it could not read, or what it read and cannot represent.
pub fn read(text: &str) -> Result<ir::Program, String> {
    let program = barb::parse(text)?;
    let facts = barb::recognise(&program);
    ir::lower(&program, &facts)
}

#[must_use]
pub fn compile(target: Target, program: &ir::Program) -> Vec<Artifact> {
    target.emit(program)
}
