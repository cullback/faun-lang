//! Lowering barb into the machine tier.
//!
//! This is where a value stops being a term and becomes storage, which is
//! this tier's job rather than barb's or a target's. One rule so far:
//!
//! - a spine-shaped type whose elements carry nothing is a number, and a
//!   number is a machine word. `Zero` is nought, `Succ(n)` is `n + 1`, and
//!   matching one asks whether it is nought.
//!
//! That is sound only while the number fits a word. A bound says when, and
//! nothing derives bounds yet, so this is a placeholder for the rule rather
//! than the rule.

use crate::barb;

use super::model::{Binary, Class, FunctionId, Program, Relation, ValueId};
use super::validate::validate;

/// Lower a whole program, or say what it holds that this tier cannot yet
/// represent.
///
/// # Errors
///
/// Names the first type whose representation is not decided.
///
/// # Panics
///
/// If `program` has no entry.
pub fn lower(program: &barb::Program) -> Result<Program, String> {
    let entry = program.entry().expect("a program to lower has an entry");
    let classes: Vec<Class> = (0..program.types().len())
        .map(|at| represent(program, barb::TypeId::at(at)))
        .collect::<Result<_, _>>()?;

    let (mut machine, main) = Program::new(program.name(program.function(entry).name));
    let mut functions: Vec<FunctionId> = Vec::with_capacity(program.functions().len());
    for at in 0..program.functions().len() {
        let id = barb::FnId::at(at);
        if id == entry {
            functions.push(main);
            continue;
        }
        let function = program.function(id);
        let params: Vec<Class> = program
            .params(id)
            .iter()
            .map(|type_| classes[type_.index()])
            .collect();
        let name = program.name(function.name);
        let returns = [classes[function.result.index()]];
        functions.push(machine.declare(name, &params, &returns));
    }

    let lowering = Lowering {
        barb: program,
        classes,
        functions,
    };
    for at in 0..program.functions().len() {
        lowering.function(&mut machine, barb::FnId::at(at));
    }
    validate(&machine)?;
    Ok(machine)
}

/// The class a type's values take. Only numbers are decided so far.
fn represent(program: &barb::Program, id: barb::TypeId) -> Result<Class, String> {
    let name = program.name(program.type_(id).name);
    let barb::Shape::Spine { cons, .. } = barb::shape(program, id) else {
        return Err(format!("`{name}` has no representation in this tier yet"));
    };
    if program.fields(cons).len() == 1 {
        return Ok(Class::Word);
    }
    Err(format!(
        "`{name}` carries something per element, which has no representation in this tier yet"
    ))
}

struct Lowering<'a> {
    barb: &'a barb::Program,
    /// The class each barb type takes, by type.
    classes: Vec<Class>,
    /// The machine function each barb function became, by function.
    functions: Vec<FunctionId>,
}

impl Lowering<'_> {
    fn function(&self, machine: &mut Program, id: barb::FnId) {
        let body = self.barb.function(id).body;
        machine.define(self.functions[id.index()], |b, params| {
            let mut locals = params.to_vec();
            let answer = self.body(b, body, &mut locals);
            b.ret(&[answer])
        });
    }

    /// Each binding takes the next local, then the tail answers.
    fn body(&self, b: &mut super::Builder, body: barb::Body, locals: &mut Vec<ValueId>) -> ValueId {
        for binding in self.barb.bindings(body.bindings).to_vec() {
            let value = self.expr(b, binding, locals);
            locals.push(value);
        }
        self.expr(b, body.tail, locals)
    }

    fn expr(&self, b: &mut super::Builder, expr: barb::ExprId, locals: &[ValueId]) -> ValueId {
        match self.barb.expr(expr) {
            barb::Expr::Atom(atom) => locals[atom.0.index()],
            barb::Expr::Static(id, value) => {
                let bytes = self.barb.known_bytes(value);
                let number = barb::as_number(self.barb, id, bytes).expect("a number");
                b.constant(self.classes[id.index()], number)
            }
            barb::Expr::Con(ctor, args) => self.construct(b, ctor, args, locals),
            barb::Expr::Call(function, args) => {
                let args: Vec<ValueId> = self
                    .barb
                    .atoms(args)
                    .iter()
                    .map(|atom| locals[atom.0.index()])
                    .collect();
                b.call(self.functions[function.index()], &args)[0]
            }
            barb::Expr::Match(scrutinee, arms) => {
                let scrutinee = locals[scrutinee.0.index()];
                self.branch(b, scrutinee, arms, locals)
            }
        }
    }

    /// A number's empty constructor is nought, and its other one counts up.
    fn construct(
        &self,
        b: &mut super::Builder,
        ctor: barb::CtorId,
        args: barb::Range,
        locals: &[ValueId],
    ) -> ValueId {
        let owner = self.barb.ctor(ctor).owner;
        let class = self.classes[owner.index()];
        let Some(&atom) = self.barb.atoms(args).first() else {
            return b.constant(class, 0);
        };
        let one = b.constant(class, 1);
        b.binary(Binary::Add, locals[atom.0.index()], one)
    }

    /// Matching a number asks whether it is nought; the other arm counts the
    /// scrutinee back down to what it wraps.
    fn branch(
        &self,
        b: &mut super::Builder,
        scrutinee: ValueId,
        arms: barb::Range,
        locals: &[ValueId],
    ) -> ValueId {
        let arms = self.barb.arms(arms).to_vec();
        let owner = self.barb.ctor(arms[0].ctor).owner;
        let class = self.classes[owner.index()];
        let barb::Shape::Spine { nil, .. } = barb::shape(self.barb, owner) else {
            unreachable!("only numbers are represented");
        };
        let empty = arms
            .iter()
            .find(|arm| arm.ctor == nil)
            .expect("an empty arm");
        let counted = arms
            .iter()
            .find(|arm| arm.ctor != nil)
            .expect("a counting arm");

        let nought = b.constant(class, 0);
        let is_nought = b.compare(Relation::Equal, scrutinee, nought);
        let results = b.if_(
            is_nought,
            &[class],
            |b| {
                let mut inner = locals.to_vec();
                let answer = self.body(b, empty.body, &mut inner);
                b.yield_(&[answer])
            },
            |b| {
                let mut inner = locals.to_vec();
                let one = b.constant(class, 1);
                inner.push(b.binary(Binary::Sub, scrutinee, one));
                let answer = self.body(b, counted.body, &mut inner);
                b.yield_(&[answer])
            },
        );
        results[0]
    }
}
