use super::*;

/// The mistake `Convert` exists to make visible: an entry that says it
/// leaves with a word, leaving with eight bits instead.
#[test]
fn a_width_that_changes_without_saying_so_is_rejected() {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let byte = b.constant(Class::Fixed { bits: 8 }, 250);
        b.ret(&[byte])
    });

    let error = validate(&program).unwrap_err();
    assert!(error.contains("main returns"), "{error}");

    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let byte = b.constant(Class::Fixed { bits: 8 }, 250);
        let word = b.convert(Class::Word, byte);
        b.ret(&[word])
    });
    assert!(validate(&program).is_ok());
}

#[test]
fn operands_of_one_operation_must_agree() {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let byte = b.constant(Class::Fixed { bits: 8 }, 1);
        let word = b.constant(Class::Word, 1);
        let sum = b.binary(Binary::Add, byte, word);
        let sum = b.convert(Class::Word, sum);
        b.ret(&[sum])
    });

    let error = validate(&program).unwrap_err();
    assert!(error.contains("in one operation"), "{error}");
}

#[test]
fn an_instruction_stays_small() {
    assert!(size_of::<Op>() <= 24, "{} bytes", size_of::<Op>());
}
