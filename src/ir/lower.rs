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

use super::model::{Binary, Class, FunctionId, PlatformId, Program, Relation, ValueId};
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
pub fn lower(program: &barb::Program, facts: &barb::Facts) -> Result<Program, String> {
    let entry = program.entry().expect("a program to lower has an entry");
    let classes: Vec<Option<Class>> = (0..program.types().len())
        .map(|at| represent(program, barb::TypeId::at(at)))
        .collect::<Result<_, _>>()?;

    let (mut machine, main) = Program::new(program.name(program.function(entry).name));
    let (functions, platforms) = declare(program, facts, &classes, &mut machine, (entry, main));

    let lowering = Lowering {
        barb: program,
        facts,
        classes,
        functions,
        platforms,
    };
    for at in 0..program.functions().len() {
        let id = barb::FnId::at(at);
        if facts.operation(id).is_none() && program.function(id).body.is_some() {
            lowering.function(&mut machine, id);
        }
    }
    validate(&machine)?;
    Ok(machine)
}

/// Every definition, as what it becomes: a function, an instruction, or a
/// routine something outside provides.
fn declare(
    program: &barb::Program,
    facts: &barb::Facts,
    classes: &[Option<Class>],
    machine: &mut Program,
    (entry, main): (barb::FnId, FunctionId),
) -> (Vec<Option<FunctionId>>, Vec<(barb::FnId, PlatformId)>) {
    // `None` where a definition became an instruction and has no function.
    let mut functions: Vec<Option<FunctionId>> = Vec::with_capacity(program.functions().len());
    let mut platforms: Vec<(barb::FnId, PlatformId)> = Vec::new();
    for at in 0..program.functions().len() {
        let id = barb::FnId::at(at);
        if id == entry {
            functions.push(Some(main));
            continue;
        }
        // A definition the tier below has an instruction for is never
        // emitted: its callers write the instruction instead.
        if facts.operation(id).is_some() {
            functions.push(None);
            continue;
        }
        let function = program.function(id);
        // A parameter or a result that carries nothing takes no slot.
        let params: Vec<Class> = program
            .params(id)
            .iter()
            .filter_map(|type_| classes[type_.index()])
            .collect();
        let returns: Vec<Class> = classes[function.result.index()].into_iter().collect();
        let name = program.name(function.name);
        // One something else answers for is a platform routine rather than a
        // function: the program declares it and a target provides it.
        functions.push(Some(if function.body.is_some() {
            machine.declare(name, &params, &returns)
        } else {
            // The `!` says the program may observe this one working; the
            // routine a target provides is named without it.
            let routine = name.strip_suffix('!').unwrap_or(name);
            platforms.push((id, machine.platform(routine, &params, &returns)));
            main
        }));
    }
    (functions, platforms)
}

/// The class a type's values take. Only numbers are decided so far.
fn represent(program: &barb::Program, id: barb::TypeId) -> Result<Option<Class>, String> {
    let name = program.name(program.type_(id).name);
    if erased(program, id) {
        return Ok(None);
    }
    let barb::Shape::Spine { cons, .. } = barb::shape(program, id) else {
        return Err(format!("`{name}` has no representation in this tier yet"));
    };
    if program.fields(cons).len() == 1 {
        return Ok(Some(Class::Word));
    }
    Err(format!(
        "`{name}` carries something per element, which has no representation in this tier yet"
    ))
}

/// A type with one constructor and no fields says nothing by being one value
/// rather than another, so it needs no bits and no argument. The authority a
/// program is handed is such a type, and so is what an effect answers when it
/// answers nothing.
fn erased(program: &barb::Program, id: barb::TypeId) -> bool {
    let ctors = program.ctors(id);
    ctors.len() == 1 && program.types_of(ctors[0].fields).is_empty()
}

struct Lowering<'a> {
    barb: &'a barb::Program,
    facts: &'a barb::Facts,
    /// The class each barb type takes, by type, and nothing where it is
    /// erased.
    classes: Vec<Option<Class>>,
    /// The machine function each barb function became, by function, and
    /// `None` for one that became an instruction.
    functions: Vec<Option<FunctionId>>,
    /// The platform routine each declared-elsewhere function became.
    platforms: Vec<(barb::FnId, PlatformId)>,
}

impl Lowering<'_> {
    /// The class a type takes, where it takes one.
    fn class(&self, id: barb::TypeId) -> Class {
        self.classes[id.index()].expect("a type that carries something")
    }

    fn function(&self, machine: &mut Program, id: barb::FnId) {
        let body = self.barb.function(id).body.expect("a function with a body");
        let into = self.functions[id.index()].expect("a function that is not an instruction");
        let entry = self.barb.entry() == Some(id);
        // An erased parameter takes no slot, so the machine's run of them is
        // shorter than barb's and they are matched up in order.
        let erased: Vec<bool> = self
            .barb
            .params(id)
            .iter()
            .map(|type_| self.classes[type_.index()].is_none())
            .collect();
        machine.define(into, |b, params| {
            let mut taken = params.iter().copied();
            let mut locals: Vec<Option<ValueId>> = erased
                .iter()
                .map(|erased| (!erased).then(|| taken.next().expect("a parameter")))
                .collect();
            let answer = self.body(b, body, &mut locals);
            match answer {
                Some(answer) => b.ret(&[answer]),
                // The entry answers a status whatever the program says, and
                // reaches here only where nothing has already left.
                None if entry => {
                    let status = b.constant(Class::Word, 0);
                    b.ret(&[status])
                }
                None => b.ret(&[]),
            }
        });
    }

    /// Each binding takes the next local, then the tail answers.
    fn body(
        &self,
        b: &mut super::Builder,
        body: barb::Body,
        locals: &mut Vec<Option<ValueId>>,
    ) -> Option<ValueId> {
        for binding in self.barb.bindings(body.bindings).to_vec() {
            let value = self.expr(b, binding, locals);
            locals.push(value);
        }
        self.expr(b, body.tail, locals)
    }

    fn expr(
        &self,
        b: &mut super::Builder,
        expr: barb::ExprId,
        locals: &[Option<ValueId>],
    ) -> Option<ValueId> {
        match self.barb.expr(expr) {
            barb::Expr::Atom(atom) => locals[atom.0.index()],
            barb::Expr::Static(id, value) => {
                let bytes = self.barb.known_bytes(value);
                let number = barb::as_number(self.barb, id, bytes).expect("a number");
                Some(b.constant(self.class(id), number))
            }
            barb::Expr::Con(ctor, args) => self.construct(b, ctor, args, locals),
            barb::Expr::Call(function, args) => self.call(b, function, args, locals),
            barb::Expr::Match(scrutinee, arms) => {
                let scrutinee = locals[scrutinee.0.index()].expect("a scrutinee to match");
                self.branch(b, scrutinee, arms, locals)
            }
        }
    }

    /// An instruction where the tier below has one, a platform routine where
    /// something outside answers, and a call otherwise. An argument that
    /// carries nothing is not passed.
    fn call(
        &self,
        b: &mut super::Builder,
        function: barb::FnId,
        args: barb::Range,
        locals: &[Option<ValueId>],
    ) -> Option<ValueId> {
        let args: Vec<ValueId> = self
            .barb
            .atoms(args)
            .iter()
            .filter_map(|atom| locals[atom.0.index()])
            .collect();
        if self.facts.operation(function) == Some(barb::Operation::Add) {
            return Some(b.binary(Binary::Add, args[0], args[1]));
        }
        if let Some((_, platform)) = self.platforms.iter().find(|(id, _)| *id == function) {
            return b.platform_call(*platform, &args).first().copied();
        }
        let callee =
            self.functions[function.index()].expect("a call to something that stayed a function");
        b.call(callee, &args).first().copied()
    }

    /// A number's empty constructor is nought, and its other one counts up.
    /// One that carries nothing builds nothing.
    fn construct(
        &self,
        b: &mut super::Builder,
        ctor: barb::CtorId,
        args: barb::Range,
        locals: &[Option<ValueId>],
    ) -> Option<ValueId> {
        let owner = self.barb.ctor(ctor).owner;
        let class = self.classes[owner.index()]?;
        let Some(&atom) = self.barb.atoms(args).first() else {
            return Some(b.constant(class, 0));
        };
        let one = b.constant(class, 1);
        let held = locals[atom.0.index()].expect("a field that carries something");
        Some(b.binary(Binary::Add, held, one))
    }

    /// Matching a number asks whether it is nought; the other arm counts the
    /// scrutinee back down to what it wraps.
    fn branch(
        &self,
        b: &mut super::Builder,
        scrutinee: ValueId,
        arms: barb::Range,
        locals: &[Option<ValueId>],
    ) -> Option<ValueId> {
        let arms = self.barb.arms(arms).to_vec();
        let owner = self.barb.ctor(arms[0].ctor).owner;
        let class = self.class(owner);
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

        // Both arms answer alike, so one of them says what the match holds.
        let answers = self.answers(empty.body);
        let held: Vec<Class> = answers.into_iter().collect();
        let nought = b.constant(class, 0);
        let is_nought = b.compare(Relation::Equal, scrutinee, nought);
        let results = b.if_(
            is_nought,
            &held,
            |b| {
                let mut inner = locals.to_vec();
                let answer = self.body(b, empty.body, &mut inner);
                b.yield_(&answer.into_iter().collect::<Vec<_>>())
            },
            |b| {
                let mut inner = locals.to_vec();
                let one = b.constant(class, 1);
                inner.push(Some(b.binary(Binary::Sub, scrutinee, one)));
                let answer = self.body(b, counted.body, &mut inner);
                b.yield_(&answer.into_iter().collect::<Vec<_>>())
            },
        );
        results.first().copied()
    }

    /// The class a body answers, where it answers anything.
    fn answers(&self, body: barb::Body) -> Option<Class> {
        match self.barb.expr(body.tail) {
            barb::Expr::Static(id, _) => self.classes[id.index()],
            barb::Expr::Con(ctor, _) => self.classes[self.barb.ctor(ctor).owner.index()],
            barb::Expr::Call(function, _) => {
                self.classes[self.barb.function(function).result.index()]
            }
            barb::Expr::Match(_, arms) => self.answers(self.barb.arms(arms)[0].body),
            barb::Expr::Atom(_) => Some(Class::Word),
        }
    }
}
