//! What a tight loop costs on the 6502, in cycles per iteration.
//!
//! One iteration count is subtracted from another so that startup, the
//! frame, and exit all cancel and what is left is the body alone. The
//! ceiling is a ratchet, not a goal: lower it when the target improves,
//! and do not raise it without saying why. For scale, `cc65 -O` compiles
//! the same loop to 45 cycles an iteration.

use std::fs;
use std::process::Command;

use faun::ir::Class::Word;
use faun::ir::{Binary, Program, Relation};
use faun::targets::Target;

/// `for (i = 0; i < limit; i++) acc += i;`, answering `acc`'s low byte.
fn summing_loop(limit: u64) -> Program {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let zero = b.constant(Word, 0);
        let start = b.constant(Word, 0);
        let total = b.loop_(&[start, zero], &[Word], |b, params| {
            let (i, acc) = (params[0], params[1]);
            let bound = b.constant(Word, limit);
            let going = b.compare(Relation::Less, i, bound);
            b.if_(
                going,
                &[Word],
                |b| {
                    let one = b.constant(Word, 1);
                    let next = b.binary(Binary::Add, i, one);
                    let sum = b.binary(Binary::Add, acc, i);
                    b.continue_(&[next, sum])
                },
                |b| b.break_(&[acc]),
            );
            faun::ir::Terminator::UNREACHABLE
        });
        b.ret(&[total[0]])
    });
    program
}

/// The cycles the image took, and the status it left with.
fn run(program: &Program) -> (u64, i32) {
    faun::ir::validate(program).expect("a well-formed program");
    let dir = std::env::temp_dir().join(format!(
        "faun-cost-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after the epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join("image");
    let artifacts = faun::compile(Target::Mos6502Sim65, program);
    fs::write(&path, &artifacts[0].bytes).expect("the image");

    let output = Command::new("sim65")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("sim65 on the path");
    fs::remove_dir_all(&dir).ok();

    let status = output.status.code().expect("the image was not signalled");
    // The simulator prints its tally on one stream or the other depending
    // on the build, so both are searched.
    let text = String::from_utf8_lossy(&output.stderr).into_owned()
        + &String::from_utf8_lossy(&output.stdout);
    let cycles = text
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_suffix(" cycles")?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no cycle count in {text:?}"));
    (cycles, status)
}

#[test]
fn a_loop_iteration_stays_within_its_cycle_ceiling() {
    const CEILING: u64 = 38;
    let (low, high) = (100u64, 1100u64);

    let (cheap, small) = run(&summing_loop(low));
    let (dear, large) = run(&summing_loop(high));

    // The sums the loop is supposed to reach, as the machine keeps them:
    // the low byte is all an exit status carries.
    assert_eq!(
        small,
        i32::from(4950u16.to_le_bytes()[0]),
        "the sum to {low}"
    );
    assert_eq!(
        large,
        i32::from(14626u16.to_le_bytes()[0]),
        "the sum to {high}"
    );

    let each = (dear - cheap) / (high - low);
    assert!(
        each <= CEILING,
        "an iteration costs {each} cycles; the ceiling is {CEILING}"
    );
}
