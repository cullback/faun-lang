//! What a target is entitled to assume.

use super::model::{Class, Exit, Op, Program, RegionId};

/// What a program has to be true of before a target sees it. These are the
/// mistakes a builder can make that a target would otherwise miscompile in
/// silence rather than reject.
///
/// # Errors
///
/// Names the first disagreement found.
pub fn validate(program: &Program) -> Result<(), String> {
    for function in program.functions() {
        check(program, &function.name, function.body, &function.returns)?;
    }
    Ok(())
}

fn check(program: &Program, name: &str, region: RegionId, returns: &[Class]) -> Result<(), String> {
    for op in program.ops(region) {
        match *op {
            Op::Binary { left, right, .. } | Op::Compare { left, right, .. } => {
                let (left, right) = (program.class(left), program.class(right));
                if left != right {
                    return Err(format!("{name}: {left:?} and {right:?} in one operation"));
                }
            }
            Op::If {
                then_region,
                else_region,
                ..
            } => {
                check(program, name, then_region, returns)?;
                check(program, name, else_region, returns)?;
            }
            Op::Loop { body, .. } => check(program, name, body, returns)?,
            _ => {}
        }
    }

    let terminator = program.region(region).terminator;
    if terminator.exit == Exit::Return {
        let found: Vec<_> = program
            .values(terminator.values)
            .iter()
            .map(|&value| program.class(value))
            .collect();
        if found != returns {
            return Err(format!(
                "{name} returns {returns:?} but leaves with {found:?}"
            ));
        }
    }
    Ok(())
}
