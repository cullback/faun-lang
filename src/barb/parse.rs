//! A small surface for writing barb programs as text.
//!
//! Nothing is blessed: there are inductive types and there are rewrites, and
//! a number is whatever a program builds out of its own constructors. Types
//! are inferred, since every one of them is a name a declaration already
//! gave, so there is nothing to infer but which.
//!
//! A function matches on at most one argument, with patterns one constructor
//! deep. Anything further is refused by name rather than read wrongly.
//!
//! ```text
//! type Nat Zero Succ(Nat)
//!
//! add(Zero b) -> b
//! add(Succ(a) b) -> Succ(add(a b))
//!
//! main() -> add(Succ(Succ(Zero)) Succ(Succ(Zero)))
//! ```

use std::collections::HashMap;

use super::ir::{Atom, CtorId, FnId, Program, TypeId};

/// What a clause matches in one argument: a name, or a constructor and the
/// names its fields take.
#[derive(Clone, Debug)]
enum Pattern {
    Bind(String),
    Ctor(String, Vec<String>),
}

/// A name applied to arguments, of which there may be none.
#[derive(Clone, Debug)]
struct Term(String, Vec<Self>);

#[derive(Clone, Debug)]
struct Clause {
    patterns: Vec<Pattern>,
    /// What the clause works out on the way, each bound to a name. A result
    /// that is not wanted is bound to one that is not read.
    steps: Vec<(String, Term)>,
    /// What the clause answers.
    body: Term,
}

/// A type as written: its name, then each constructor and its field types.
type Declared = (String, Vec<(String, Vec<String>)>);

#[derive(Default)]
struct Source {
    types: Vec<Declared>,
    /// The functions in the order they were first written.
    order: Vec<String>,
    clauses: HashMap<String, Vec<Clause>>,
}

/// Read a program.
///
/// # Errors
///
/// Names what it could not read, or what it read and cannot represent.
pub fn parse(text: &str) -> Result<Program, String> {
    let mut words = Words::new(text);
    let mut source = Source::default();

    while let Some(word) = words.peek() {
        if word == "type" {
            words.take();
            source.types.push(read_type(&mut words)?);
            continue;
        }
        let name = words.word()?;
        let patterns = read_patterns(&mut words)?;
        words.expect("->")?;
        // Everything up to the last term is bound to a name; the last is what
        // the clause answers. A step is what is followed by an equals, which
        // is what tells a body from the clause after it.
        let mut steps = Vec::new();
        while words.at(1) == Some("=") {
            let held = words.word()?;
            words.expect("=")?;
            steps.push((held, read_term(&mut words)?));
        }
        let body = read_term(&mut words)?;
        if !source.clauses.contains_key(&name) {
            source.order.push(name.clone());
        }
        let clause = Clause {
            patterns,
            steps,
            body,
        };
        source.clauses.entry(name).or_default().push(clause);
    }
    build(&source)
}

/// A type and its constructors, which run until the next declaration: there
/// is nothing between them to say where one ends, so what ends them is a
/// `type` or a clause.
fn read_type(words: &mut Words) -> Result<Declared, String> {
    let name = words.word()?;
    let mut ctors = Vec::new();
    while words.peek().is_some_and(|word| word != "type") && !words.starts_clause() {
        let ctor = words.word()?;
        let fields = if words.peek() == Some("(") {
            read_names(words)?
        } else {
            Vec::new()
        };
        ctors.push((ctor, fields));
    }
    Ok((name, ctors))
}

/// A parenthesised run of bare words.
fn read_names(words: &mut Words) -> Result<Vec<String>, String> {
    words.expect("(")?;
    let mut names = Vec::new();
    while words.peek() != Some(")") {
        names.push(words.word()?);
    }
    words.expect(")")?;
    Ok(names)
}

fn read_patterns(words: &mut Words) -> Result<Vec<Pattern>, String> {
    words.expect("(")?;
    let mut patterns = Vec::new();
    while words.peek() != Some(")") {
        let head = words.word()?;
        patterns.push(if words.peek() == Some("(") {
            Pattern::Ctor(head, read_names(words)?)
        } else {
            Pattern::Bind(head)
        });
    }
    words.expect(")")?;
    Ok(patterns)
}

fn read_term(words: &mut Words) -> Result<Term, String> {
    let head = words.word()?;
    if words.peek() != Some("(") {
        return Ok(Term(head, Vec::new()));
    }
    words.take();
    let mut args = Vec::new();
    while words.peek() != Some(")") {
        args.push(read_term(words)?);
    }
    words.expect(")")?;
    Ok(Term(head, args))
}

const PUNCTUATION: [&str; 7] = ["(", ")", ",", "=", "|", ":", "->"];

/// Words and punctuation, in order.
struct Words {
    words: Vec<String>,
    at: usize,
}

impl Words {
    fn new(text: &str) -> Self {
        let mut words = Vec::new();
        let mut held = String::new();
        let mut rest = text.chars().peekable();
        while let Some(character) = rest.next() {
            if character == '#' {
                flush(&mut held, &mut words);
                while rest.peek().is_some_and(|held| *held != '\n') {
                    rest.next();
                }
            } else if character == '-' && rest.peek() == Some(&'>') {
                rest.next();
                flush(&mut held, &mut words);
                words.push("->".to_owned());
            } else if "(),=|:".contains(character) {
                flush(&mut held, &mut words);
                words.push(character.to_string());
            } else if character.is_whitespace() {
                flush(&mut held, &mut words);
            } else {
                held.push(character);
            }
        }
        flush(&mut held, &mut words);
        Self { words, at: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.at(0)
    }

    /// The word `ahead` past the next one.
    fn at(&self, ahead: usize) -> Option<&str> {
        self.words.get(self.at + ahead).map(String::as_str)
    }

    fn take(&mut self) -> Option<String> {
        let word = self.words.get(self.at).cloned();
        self.at += 1;
        word
    }

    /// A name. Punctuation is never one, so a stray comma is refused rather
    /// than bound as though it were a variable.
    fn word(&mut self) -> Result<String, String> {
        match self.take() {
            Some(word) if !PUNCTUATION.contains(&word.as_str()) => Ok(word),
            Some(word) => Err(format!("expected a name, found `{word}`")),
            None => Err("the program ends early".to_owned()),
        }
    }

    /// Whether what comes next is a clause rather than a constructor: a name,
    /// a parenthesised run, and then an arrow. A constructor's own run is
    /// followed by anything else.
    fn starts_clause(&self) -> bool {
        if self.at(1) != Some("(") {
            return false;
        }
        let mut at = 1;
        let mut depth = 0usize;
        loop {
            match self.at(at) {
                Some("(") => depth += 1,
                Some(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return self.at(at + 1) == Some("->");
                    }
                }
                Some(_) => {}
                None => return false,
            }
            at += 1;
        }
    }

    fn expect(&mut self, want: &str) -> Result<(), String> {
        match self.take() {
            Some(word) if word == want => Ok(()),
            other => Err(format!("expected `{want}`, found {other:?}")),
        }
    }
}

fn flush(held: &mut String, words: &mut Vec<String>) {
    if !held.is_empty() {
        words.push(std::mem::take(held));
    }
}

/// Turn what was read into a program.
fn build(source: &Source) -> Result<Program, String> {
    let mut program = Program::default();
    declare_builtin(&mut program, source)?;
    declare_types(&mut program, source)?;
    let ctors = catalogue(&program);
    // A bare name in a pattern is a constructor if one is declared with that
    // name, and a binder otherwise. Nothing in the text says which, so this
    // is the first point that can tell -- and everything after reads the
    // resolved clauses, inference included.
    let clauses: HashMap<&str, Vec<Clause>> = source
        .clauses
        .iter()
        .map(|(name, held)| {
            (
                name.as_str(),
                held.iter().map(|c| resolve(c, &ctors)).collect(),
            )
        })
        .collect();

    let arity = arities(source, &clauses)?;
    colour(source)?;
    let told = infer(source, &clauses, &program, &ctors, &arity)?;
    let mut functions: HashMap<&str, FnId> = HashMap::new();
    for (name, _) in EFFECTS {
        if let Some((params, result)) = told.get(name) {
            functions.insert(name, program.declare(name, params, *result));
        }
    }
    for name in &source.order {
        let (params, result) = &told[name];
        functions.insert(name, program.declare(name, params, *result));
    }

    // Where a program starts, by name, since nothing in the text says so. It
    // is effectful because a program that does nothing observable does
    // nothing: the status it leaves with is something it says, not something
    // that falls out of how it was called.
    let main = functions
        .get("main!")
        .ok_or_else(|| "no `main!` to start at".to_owned())?;
    program.set_entry(*main);
    define(&mut program, source, &clauses, &told, &functions)
}

/// Compile every function's clauses into its body.
fn define(
    program: &mut Program,
    source: &Source,
    clauses: &HashMap<&str, Vec<Clause>>,
    told: &HashMap<String, Inferred>,
    functions: &HashMap<&str, FnId>,
) -> Result<Program, String> {
    let ctors = catalogue(program);
    let lowering = Lowering {
        ctors: &ctors,
        functions,
    };
    for name in &source.order {
        let held = &clauses[name.as_str()];
        let (params, _) = &told[name];
        lowering.define(program, functions[name.as_str()], held, params)?;
    }
    Ok(std::mem::take(program))
}

/// The authority a program is handed, and what an effect answers when it
/// answers nothing. Nothing can build a `Host`, because its constructor is
/// not among the names a term may use, so the only way to hold one is to
/// have been handed it.
fn declare_builtin(program: &mut Program, source: &Source) -> Result<(), String> {
    for name in BUILTIN {
        if source.types.iter().any(|(held, _)| held == name) {
            return Err(format!("`{name}` is the compiler's, not a program's"));
        }
        let id = program.declare_type(name);
        program.define_type(id, &[(name, &[])]);
    }
    Ok(())
}

/// Every type the program declares, then the constructors of each, so that
/// a field may name a type declared later.
fn declare_types(program: &mut Program, source: &Source) -> Result<(), String> {
    let mut types: HashMap<&str, TypeId> = HashMap::new();
    for (name, _) in &source.types {
        types.insert(name, program.declare_type(name));
    }
    for (name, ctors) in &source.types {
        let held: Vec<(&str, Vec<TypeId>)> = ctors
            .iter()
            .map(|(ctor, fields)| {
                let fields = fields
                    .iter()
                    .map(|field| look(&types, field, "type"))
                    .collect::<Result<_, _>>()?;
                Ok((ctor.as_str(), fields))
            })
            .collect::<Result<_, String>>()?;
        let held: Vec<(&str, &[TypeId])> = held
            .iter()
            .map(|(ctor, fields)| (*ctor, fields.as_slice()))
            .collect();
        program.define_type(types[name.as_str()], &held);
    }
    Ok(())
}

/// How many arguments each name takes, which every clause of one has to
/// agree on. An effect takes the authority and what it was blessed with.
fn arities<'a>(
    source: &'a Source,
    clauses: &HashMap<&str, Vec<Clause>>,
) -> Result<HashMap<&'a str, usize>, String> {
    let mut arity = HashMap::new();
    for name in &source.order {
        let held = &clauses[name.as_str()];
        let width = held[0].patterns.len();
        if held.iter().any(|clause| clause.patterns.len() != width) {
            return Err(format!("`{name}` takes a different number each clause"));
        }
        arity.insert(name.as_str(), width);
    }
    for (name, takes) in EFFECTS {
        arity.insert(name, takes + 1);
    }
    Ok(arity)
}

/// A name ending in `!` may perform observable work, and a name that does
/// not may not call one that does. One colour, stated where it is written.
fn colour(source: &Source) -> Result<(), String> {
    fn calls(term: &Term, each: &mut impl FnMut(&str)) {
        each(&term.0);
        for arg in &term.1 {
            calls(arg, each);
        }
    }
    for name in &source.order {
        if name.ends_with('!') {
            continue;
        }
        for clause in &source.clauses[name] {
            let mut found = None;
            for term in clause
                .steps
                .iter()
                .map(|(_, term)| term)
                .chain([&clause.body])
            {
                calls(term, &mut |called| {
                    if called.ends_with('!') {
                        found.get_or_insert_with(|| called.to_owned());
                    }
                });
            }
            if let Some(called) = found {
                return Err(format!("`{name}` is pure and calls `{called}`"));
            }
        }
    }
    Ok(())
}

/// A pattern naming a declared constructor is that constructor, not a name
/// the clause binds.
fn resolve(clause: &Clause, ctors: &HashMap<String, CtorId>) -> Clause {
    let patterns = clause
        .patterns
        .iter()
        .map(|pattern| match pattern {
            Pattern::Bind(name) if ctors.contains_key(name) => {
                Pattern::Ctor(name.clone(), Vec::new())
            }
            held => held.clone(),
        })
        .collect();
    Clause {
        patterns,
        steps: clause.steps.clone(),
        body: clause.body.clone(),
    }
}

/// Which argument the clauses match on, if any. One is allowed, so that a
/// single `match` serves: more is a pattern-match compiler.
fn matched_argument(clauses: &[Clause]) -> Result<Option<usize>, String> {
    let mut matched = None;
    for clause in clauses {
        for (at, pattern) in clause.patterns.iter().enumerate() {
            if matches!(pattern, Pattern::Bind(_)) {
                continue;
            }
            match matched {
                None => matched = Some(at),
                Some(already) if already == at => {}
                Some(already) => {
                    return Err(format!(
                        "this matches on argument {already} and on argument {at}; one at a time"
                    ));
                }
            }
        }
    }
    Ok(matched)
}

/// The types the compiler declares: the authority a program is handed, and
/// what an effect answers when it answers nothing. Neither carries anything,
/// so both are erased, and `Host`'s constructor is left out of the names a
/// term may use so that no program can make one for itself.
const BUILTIN: [&str; 2] = ["Host", "Unit"];

/// The effects a target answers for, and how many arguments each takes past
/// the authority. A program declares none of these: they are the interface
/// to the outside, so the compiler knows their names and a target provides
/// them. What they take beyond the authority is whatever they are given.
const EFFECTS: [(&str, usize); 1] = [("exit!", 1)];

/// Every constructor a term may name.
fn catalogue(program: &Program) -> HashMap<String, CtorId> {
    let mut ctors = HashMap::new();
    // From past the authority, which is the one a program may not build.
    for at in 1..program.types().len() {
        let id = TypeId::at(at);
        for tag in 0..u32::try_from(program.type_(id).ctors.len()).expect("a sane count") {
            let ctor = program.ctor_at(id, tag);
            ctors.insert(program.name(program.ctor(ctor).name).to_owned(), ctor);
        }
    }
    ctors
}

fn look<T: Copy>(table: &HashMap<&str, T>, name: &str, what: &str) -> Result<T, String> {
    table
        .get(name)
        .copied()
        .ok_or_else(|| format!("no {what} named `{name}`"))
}

struct Lowering<'a> {
    ctors: &'a HashMap<String, CtorId>,
    functions: &'a HashMap<&'a str, FnId>,
}

/// The names a clause has bound, and what each stands for.
type Scope = Vec<(String, Atom)>;

impl Lowering<'_> {
    /// Compile one function's clauses into its body. A builder answers an
    /// atom rather than a result, so a refusal is carried out around it.
    fn define(
        &self,
        program: &mut Program,
        id: FnId,
        clauses: &[Clause],
        types: &[TypeId],
    ) -> Result<(), String> {
        let matched = matched_argument(clauses)?;
        let mut failed = None;
        program.define(id, |b, args| {
            match self.clauses(b, clauses, args, types, matched, Scope::new()) {
                Ok(atom) => atom,
                Err(error) => {
                    failed = Some(error);
                    args.first().copied().unwrap_or(Atom(super::ir::Local(0)))
                }
            }
        });
        failed.map_or(Ok(()), Err)
    }

    /// The clauses of one function: a match on the argument they agree to
    /// match on, with each arm the clause that names that constructor.
    fn clauses(
        &self,
        b: &mut super::Builder,
        clauses: &[Clause],
        args: &[Atom],
        types: &[TypeId],
        matched: Option<usize>,
        scope: Scope,
    ) -> Result<Atom, String> {
        let Some(matched) = matched else {
            let clause = &clauses[0];
            let scope = bind(scope, &clause.patterns, args)?;
            return self.body(b, clause, scope);
        };
        let scrutinee = *args
            .get(matched)
            .ok_or_else(|| format!("no argument {matched} to match on"))?;
        let owner = types[matched];

        let mut failed = None;
        let answer = b.match_(scrutinee, owner, |b, ctor, fields| {
            match self.arm(b, clauses, (args, matched), (ctor, fields), &scope) {
                Ok(atom) => atom,
                Err(error) => {
                    failed.get_or_insert(error);
                    scrutinee
                }
            }
        });
        failed.map_or(Ok(answer), Err)
    }

    /// The clause for one constructor: the one naming it, or failing that a
    /// clause that binds the argument rather than matching it.
    fn arm(
        &self,
        b: &mut super::Builder,
        clauses: &[Clause],
        (args, matched): (&[Atom], usize),
        (ctor, fields): (CtorId, &[Atom]),
        scope: &Scope,
    ) -> Result<Atom, String> {
        let wanted = self
            .ctors
            .iter()
            .find(|(_, held)| **held == ctor)
            .map(|(name, _)| name.clone())
            .expect("a constructor of the program");
        let clause = clauses
            .iter()
            .find(|clause| match &clause.patterns[matched] {
                Pattern::Ctor(name, _) => *name == wanted,
                Pattern::Bind(_) => true,
            })
            .ok_or_else(|| format!("no clause covers `{wanted}`"))?;

        let mut scope = scope.clone();
        for (at, pattern) in clause.patterns.iter().enumerate() {
            // The matched argument binds the fields the constructor revealed;
            // every other one binds whatever name the clause gave it.
            if at == matched
                && let Pattern::Ctor(_, names) = pattern
            {
                if names.len() != fields.len() {
                    return Err(format!("`{wanted}` takes {} fields", fields.len()));
                }
                for (name, field) in names.iter().zip(fields) {
                    scope.push((name.clone(), *field));
                }
                continue;
            }
            if let Pattern::Bind(name) = pattern {
                scope.push((name.clone(), args[at]));
            }
        }
        self.body(b, clause, scope)
    }

    /// A clause's steps in order, each bound where it named it, then what it
    /// answers. Order is the order they were written: the tier below keeps a
    /// body's bindings in the order they arrive, so an effect that must
    /// happen first is written first.
    fn body(&self, b: &mut super::Builder, clause: &Clause, scope: Scope) -> Result<Atom, String> {
        let mut scope = scope;
        for (held, step) in &clause.steps {
            let atom = self.term(b, step, &scope)?;
            scope.push((held.clone(), atom));
        }
        self.term(b, &clause.body, &scope)
    }

    fn term(&self, b: &mut super::Builder, term: &Term, scope: &Scope) -> Result<Atom, String> {
        let Term(name, args) = term;
        if args.is_empty()
            && let Some((_, atom)) = scope.iter().rev().find(|(held, _)| held == name)
        {
            return Ok(*atom);
        }
        let args: Vec<Atom> = args
            .iter()
            .map(|arg| self.term(b, arg, scope))
            .collect::<Result<_, _>>()?;
        if let Some(ctor) = self.ctors.get(name) {
            return Ok(b.con(*ctor, &args));
        }
        let id = look(self.functions, name, "function")?;
        Ok(b.call(id, &args))
    }
}

/// Bind a clause's names to the arguments, where it matches none of them.
fn bind(mut scope: Scope, patterns: &[Pattern], args: &[Atom]) -> Result<Scope, String> {
    for (pattern, arg) in patterns.iter().zip(args) {
        match pattern {
            Pattern::Bind(name) => scope.push((name.clone(), *arg)),
            Pattern::Ctor(name, _) => return Err(format!("`{name}` matches where nothing does")),
        }
    }
    Ok(scope)
}

/// What a function takes and answers, worked out rather than declared.
type Inferred = (Vec<TypeId>, TypeId);

/// Slots that must hold the same type, and the type they hold once anything
/// pins it. Every type here is a name some declaration already gave, so
/// there is nothing to infer but which one.
struct Unify {
    parent: Vec<usize>,
    known: Vec<Option<TypeId>>,
}

impl Unify {
    fn fresh(&mut self) -> usize {
        self.parent.push(self.parent.len());
        self.known.push(None);
        self.parent.len() - 1
    }

    fn find(&mut self, slot: usize) -> usize {
        let mut at = slot;
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }
        at
    }

    fn union(&mut self, left: usize, right: usize) -> Result<(), String> {
        let (left, right) = (self.find(left), self.find(right));
        if left == right {
            return Ok(());
        }
        let held = match (self.known[left], self.known[right]) {
            (Some(one), Some(other)) if one != other => {
                return Err("a value is used at two types".to_owned());
            }
            (held, other) => held.or(other),
        };
        self.parent[right] = left;
        self.known[left] = held;
        Ok(())
    }

    fn pin(&mut self, slot: usize, id: TypeId) -> Result<(), String> {
        let slot = self.find(slot);
        match self.known[slot] {
            Some(held) if held != id => Err("a value is used at two types".to_owned()),
            _ => {
                self.known[slot] = Some(id);
                Ok(())
            }
        }
    }

    fn get(&mut self, slot: usize) -> Option<TypeId> {
        let slot = self.find(slot);
        self.known[slot]
    }
}

/// Work out what every function takes and answers.
fn infer(
    source: &Source,
    clauses: &HashMap<&str, Vec<Clause>>,
    program: &Program,
    ctors: &HashMap<String, CtorId>,
    arity: &HashMap<&str, usize>,
) -> Result<HashMap<String, Inferred>, String> {
    let mut unify = Unify {
        parent: Vec::new(),
        known: Vec::new(),
    };
    // Each function takes a run of slots: one per argument, then its result.
    let base = slots(source, arity, &mut unify)?;

    for name in &source.order {
        for clause in &clauses[name.as_str()] {
            let mut scope = bound(clause, base[name.as_str()], &mut unify, program, ctors)?;
            for (held, step) in &clause.steps {
                let slot = term(step, &scope, &mut unify, program, ctors, &base)?;
                scope.insert(held.as_str(), slot);
            }
            let answer = term(&clause.body, &scope, &mut unify, program, ctors, &base)?;
            let result = base[name.as_str()] + arity[name.as_str()];
            unify.union(answer, result)?;
        }
    }

    let mut told = HashMap::new();
    for name in source.order.iter().map(String::as_str) {
        told.insert(name.to_owned(), settled(&mut unify, &base, arity, name)?);
    }
    // An effect no program calls settles nothing, and needs to: there is
    // nothing to declare and nothing to answer for.
    for (name, _) in EFFECTS {
        if let Ok(held) = settled(&mut unify, &base, arity, name) {
            told.insert(name.to_owned(), held);
        }
    }
    Ok(told)
}

/// A run of slots per function: one per argument, then its result. A
/// platform states what it takes, so its slots start out settled.
fn slots<'a>(
    source: &'a Source,
    arity: &HashMap<&str, usize>,
    unify: &mut Unify,
) -> Result<HashMap<&'a str, usize>, String> {
    let mut base = HashMap::new();
    for name in &source.order {
        let at = unify.parent.len();
        for _ in 0..=arity[name.as_str()] {
            unify.fresh();
        }
        base.insert(name.as_str(), at);
    }
    // An effect takes the authority and answers nothing; what it takes
    // between is whatever it is given, which is what the argument settles.
    let (host, unit) = (TypeId::at(0), TypeId::at(1));
    for (name, takes) in EFFECTS {
        let at = unify.parent.len();
        let authority = unify.fresh();
        unify.pin(authority, host)?;
        for _ in 0..takes {
            unify.fresh();
        }
        let answer = unify.fresh();
        unify.pin(answer, unit)?;
        base.insert(name, at);
    }
    Ok(base)
}

/// What a clause's patterns bind: an argument's own slot where the pattern
/// names it, and a fresh slot per field where it takes one apart.
fn bound<'a>(
    clause: &'a Clause,
    base: usize,
    unify: &mut Unify,
    program: &Program,
    ctors: &HashMap<String, CtorId>,
) -> Result<HashMap<&'a str, usize>, String> {
    let mut scope = HashMap::new();
    for (at, pattern) in clause.patterns.iter().enumerate() {
        match pattern {
            Pattern::Bind(held) => {
                scope.insert(held.as_str(), base + at);
            }
            Pattern::Ctor(ctor, fields) => {
                let id = *ctors
                    .get(ctor)
                    .ok_or_else(|| format!("no constructor named `{ctor}`"))?;
                unify.pin(base + at, program.ctor(id).owner)?;
                for (field, held) in program.fields(id).to_vec().iter().zip(fields) {
                    let slot = unify.fresh();
                    unify.pin(slot, *field)?;
                    scope.insert(held.as_str(), slot);
                }
            }
        }
    }
    Ok(scope)
}

/// What a name takes and answers, once everything it is used at agrees.
fn settled(
    unify: &mut Unify,
    base: &HashMap<&str, usize>,
    arity: &HashMap<&str, usize>,
    name: &str,
) -> Result<Inferred, String> {
    let at = base[name];
    let params = (0..arity[name])
        .map(|argument| tell(unify, at + argument, name))
        .collect::<Result<_, _>>()?;
    let result = tell(unify, at + arity[name], name)?;
    Ok((params, result))
}

fn tell(unify: &mut Unify, slot: usize, name: &str) -> Result<TypeId, String> {
    unify
        .get(slot)
        .ok_or_else(|| format!("nothing says what type `{name}` works over"))
}

/// The slot a term's type lives in, constraining what it is built from.
fn term(
    held: &Term,
    scope: &HashMap<&str, usize>,
    unify: &mut Unify,
    program: &Program,
    ctors: &HashMap<String, CtorId>,
    base: &HashMap<&str, usize>,
) -> Result<usize, String> {
    let Term(name, args) = held;
    if let Some(slot) = scope.get(name.as_str()) {
        if args.is_empty() {
            return Ok(*slot);
        }
        return Err(format!("`{name}` is a value, not something to apply"));
    }
    if let Some(ctor) = ctors.get(name) {
        let fields = program.fields(*ctor).to_vec();
        if fields.len() != args.len() {
            return Err(format!("`{name}` takes {} fields", fields.len()));
        }
        for (argument, field) in args.iter().zip(fields) {
            let slot = term(argument, scope, unify, program, ctors, base)?;
            unify.pin(slot, field)?;
        }
        let answer = unify.fresh();
        unify.pin(answer, program.ctor(*ctor).owner)?;
        return Ok(answer);
    }
    let at = *base.get(name.as_str()).ok_or_else(|| {
        if name == BUILTIN[0] {
            format!("`{name}` cannot be built; it is what `main!` is handed")
        } else {
            format!("nothing named `{name}` is in scope")
        }
    })?;
    for (argument, index) in args.iter().zip(0..) {
        let slot = term(argument, scope, unify, program, ctors, base)?;
        unify.union(slot, at + index)?;
    }
    Ok(at + args.len())
}
