//! A small surface for writing barb programs as text.
//!
//! Enough to exercise the tier, and no more. Declarations carry their types,
//! because barb needs them and inferring them is a checker rather than a
//! parser; a function matches on at most one argument, with patterns one
//! constructor deep. Anything further is refused by name rather than read
//! wrongly.
//!
//! ```text
//! type Nat = Zero | Succ(Nat)
//!
//! add Nat Nat : Nat
//! add(Zero b) -> b
//! add(Succ(a) b) -> Succ(add(a b))
//!
//! main : Nat
//! main() -> add(2 2)
//! ```

use std::collections::HashMap;

use super::constant::{Shape, shape};
use super::ir::{Atom, CtorId, FnId, Program, TypeId};

/// What a clause matches in one argument: a name, or a constructor and the
/// names its fields take.
#[derive(Clone, Debug)]
enum Pattern {
    Bind(String),
    Ctor(String, Vec<String>),
}

#[derive(Clone, Debug)]
enum Term {
    Name(String),
    Number(u64),
    Apply(String, Vec<Self>),
}

#[derive(Clone, Debug)]
struct Clause {
    patterns: Vec<Pattern>,
    body: Term,
}

/// A type as written: its name, then each constructor and its field types.
type Declared = (String, Vec<(String, Vec<String>)>);
/// A signature as written: a name, its argument types, and its result type.
type Signature = (String, Vec<String>, String);

#[derive(Default)]
struct Source {
    types: Vec<Declared>,
    signatures: Vec<Signature>,
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
        // A clause takes its patterns in parentheses; a signature lists the
        // types it takes and ends in the one it answers.
        let name = words.word()?;
        if words.peek().as_deref() == Some("(") {
            let patterns = read_patterns(&mut words)?;
            words.expect("->")?;
            let body = read_term(&mut words)?;
            let clause = Clause { patterns, body };
            source.clauses.entry(name).or_default().push(clause);
        } else {
            let mut params = Vec::new();
            while words.peek().as_deref() != Some(":") {
                params.push(words.word()?);
            }
            words.expect(":")?;
            source.signatures.push((name, params, words.word()?));
        }
    }
    build(&source)
}

fn read_type(words: &mut Words) -> Result<Declared, String> {
    let name = words.word()?;
    words.expect("=")?;
    let mut ctors = Vec::new();
    loop {
        let ctor = words.word()?;
        let fields = if words.peek().as_deref() == Some("(") {
            read_names(words)?
        } else {
            Vec::new()
        };
        ctors.push((ctor, fields));
        if words.peek().as_deref() != Some("|") {
            return Ok((name, ctors));
        }
        words.take();
    }
}

/// A parenthesised run of bare words.
fn read_names(words: &mut Words) -> Result<Vec<String>, String> {
    words.expect("(")?;
    let mut names = Vec::new();
    while words.peek().as_deref() != Some(")") {
        names.push(words.word()?);
    }
    words.expect(")")?;
    Ok(names)
}

fn read_patterns(words: &mut Words) -> Result<Vec<Pattern>, String> {
    words.expect("(")?;
    let mut patterns = Vec::new();
    while words.peek().as_deref() != Some(")") {
        let head = words.word()?;
        patterns.push(if words.peek().as_deref() == Some("(") {
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
    if let Ok(number) = head.parse::<u64>() {
        return Ok(Term::Number(number));
    }
    if words.peek().as_deref() != Some("(") {
        return Ok(Term::Name(head));
    }
    words.take();
    let mut args = Vec::new();
    while words.peek().as_deref() != Some(")") {
        args.push(read_term(words)?);
    }
    words.expect(")")?;
    Ok(Term::Apply(head, args))
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
            if character == '-' && rest.peek() == Some(&'>') {
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

    fn peek(&self) -> Option<String> {
        self.words.get(self.at).cloned()
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
    let mut types: HashMap<&str, TypeId> = HashMap::new();
    for (name, _) in &source.types {
        types.insert(name, program.declare_type(name));
    }
    for (name, ctors) in &source.types {
        let ctors: Vec<(&str, Vec<TypeId>)> = ctors
            .iter()
            .map(|(ctor, fields)| {
                let fields = fields
                    .iter()
                    .map(|field| look(&types, field, "type"))
                    .collect::<Result<_, _>>()?;
                Ok((ctor.as_str(), fields))
            })
            .collect::<Result<_, String>>()?;
        let ctors: Vec<(&str, &[TypeId])> = ctors
            .iter()
            .map(|(ctor, fields)| (*ctor, fields.as_slice()))
            .collect();
        program.define_type(types[name.as_str()], &ctors);
    }

    let mut functions: HashMap<&str, FnId> = HashMap::new();
    for (name, params, result) in &source.signatures {
        let params: Vec<TypeId> = params
            .iter()
            .map(|param| look(&types, param, "type"))
            .collect::<Result<_, _>>()?;
        let result = look(&types, result, "type")?;
        functions.insert(name, program.declare(name, &params, result));
    }

    // `main` is where a program starts, by name, since nothing in the text
    // says so and the tier below needs an entry.
    if let Some(main) = functions.get("main") {
        program.set_entry(*main);
    }
    define(&mut program, source, &types, &functions)
}

/// Compile every function's clauses into its body.
fn define(
    program: &mut Program,
    source: &Source,
    types: &HashMap<&str, TypeId>,
    functions: &HashMap<&str, FnId>,
) -> Result<Program, String> {
    let ctors = catalogue(program);
    let number = only_number(program);
    // A bare name in a pattern is a constructor if one is declared with that
    // name, and a binder otherwise. Nothing in the text says which, so this
    // is the first point that can tell.
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
    let lowering = Lowering {
        ctors: &ctors,
        functions,
        number,
    };
    for (name, params, _) in &source.signatures {
        let held = clauses
            .get(name.as_str())
            .ok_or_else(|| format!("`{name}` is declared and never defined"))?;
        let types: Vec<TypeId> = params
            .iter()
            .map(|param| look(types, param, "type"))
            .collect::<Result<_, _>>()?;
        lowering.define(program, functions[name.as_str()], held, &types)?;
    }
    Ok(std::mem::take(program))
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

/// Every constructor in the program, by name.
fn catalogue(program: &Program) -> HashMap<String, CtorId> {
    let mut ctors = HashMap::new();
    for at in 0..program.types().len() {
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
    /// The type a bare numeral takes, when exactly one type holds numbers.
    number: Option<TypeId>,
}

/// The one type whose values are numbers, when the program declares one. A
/// numeral means nothing without it, and means two things with two.
fn only_number(program: &Program) -> Option<TypeId> {
    let mut found = None;
    for at in 0..program.types().len() {
        let id = TypeId::at(at);
        let Shape::Spine { cons, .. } = shape(program, id) else {
            continue;
        };
        if program.fields(cons).len() != 1 {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(id);
    }
    found
}

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
            return self.term(b, &clause.body, &scope);
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
        self.term(b, &clause.body, &scope)
    }

    fn term(&self, b: &mut super::Builder, term: &Term, scope: &Scope) -> Result<Atom, String> {
        match term {
            Term::Name(name) => self.name(b, name, scope),
            Term::Number(value) => {
                let id = self.number.ok_or_else(|| {
                    format!("`{value}` needs exactly one number type in the program")
                })?;
                let held = b.number(id, *value);
                Ok(b.known(id, held))
            }
            Term::Apply(name, args) => {
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
    }

    fn name(&self, b: &mut super::Builder, name: &str, scope: &Scope) -> Result<Atom, String> {
        if let Some((_, atom)) = scope.iter().rev().find(|(held, _)| held == name) {
            return Ok(*atom);
        }
        let ctor = self
            .ctors
            .get(name)
            .ok_or_else(|| format!("nothing named `{name}` is in scope"))?;
        Ok(b.con(*ctor, &[]))
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
