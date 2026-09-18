use super::*;

/// `Nat`, `U8`, and `List_U8`, which between them reach every case the
/// encoding has.
pub(super) fn types() -> (Program, TypeId, TypeId, TypeId) {
    let mut program = Program::default();
    let nat = program.declare_type("Nat");
    program.define_type(nat, &[("Zero", &[]), ("Succ", &[nat])]);
    let byte = program.declare_type("U8");
    program.define_type(byte, &[("Zero", &[]), ("Succ", &[byte])]);
    let list = program.declare_type("List_U8");
    program.define_type(list, &[("Nil", &[]), ("Cons", &[byte, list])]);
    (program, nat, byte, list)
}

pub(super) fn nat(program: &Program, id: TypeId, value: u64) -> Value {
    let mut term = Value(program.ctor_at(id, 0), Vec::new());
    for _ in 0..value {
        term = Value(program.ctor_at(id, 1), vec![term]);
    }
    term
}

fn list(program: &Program, list: TypeId, byte: TypeId, bytes: &[u8]) -> Value {
    let mut term = Value(program.ctor_at(list, 0), Vec::new());
    for value in bytes.iter().rev() {
        term = Value(
            program.ctor_at(list, 1),
            vec![nat(program, byte, u64::from(*value)), term],
        );
    }
    term
}

#[test]
fn a_name_is_interned_once_and_reads_back() {
    let (mut program, nat, byte, list) = types();
    // `Zero` names a constructor of both `Nat` and `U8`; it is one symbol.
    let on_nat = program.ctor(program.ctor_at(nat, 0)).name;
    let on_byte = program.ctor(program.ctor_at(byte, 0)).name;
    assert_eq!(on_nat, on_byte, "one name, one symbol");
    assert_eq!(program.name(on_nat), "Zero");
    assert_eq!(program.name(program.type_(list).name), "List_U8");

    // Interning is idempotent, so a name costs one entry however often it is
    // written, and asking for one already held answers what is already there.
    assert_eq!(program.symbol("Zero"), on_nat);

    // Every name the program uses, once each and end to end: naming things
    // costs no allocation beyond the pool growing.
    let names = "NatZeroSuccU8List_U8NilCons";
    assert_eq!(
        program.names_text(),
        names,
        "each name once, in declaration order"
    );
}

#[test]
fn a_term_stays_small() {
    assert_eq!(size_of::<Expr>(), 16);
    assert_eq!(size_of::<Arm>(), 16);
    assert_eq!(size_of::<Body>(), 12);
    assert_eq!(size_of::<Atom>(), 4);
}

#[test]
fn a_spine_is_recognised_and_a_branching_type_is_not() {
    let (mut program, nat, _, list) = types();
    assert!(matches!(shape(&program, nat), Shape::Spine { .. }));
    assert!(matches!(shape(&program, list), Shape::Spine { .. }));

    let tree = program.declare_type("Tree");
    program.define_type(tree, &[("Leaf", &[]), ("Node", &[tree, tree])]);
    assert_eq!(shape(&program, tree), Shape::Tagged, "two recursive fields");

    let extra = program.declare_type("Three");
    program.define_type(extra, &[("A", &[]), ("B", &[extra]), ("C", &[])]);
    assert_eq!(shape(&program, extra), Shape::Tagged, "a third constructor");
}

#[test]
fn a_natural_costs_the_logarithm_of_its_value() {
    let (mut program, nat_id, _, _) = types();
    let thousand = nat(&program, nat_id, 1000);
    let id = program.intern(nat_id, &thousand);
    assert_eq!(
        program.known_bytes(id),
        [0xE8, 0x07],
        "1000, seven bits a byte"
    );
    assert_eq!(
        decode(&program, nat_id, program.known_bytes(id)).0,
        thousand
    );
    assert_eq!(
        as_number(&program, nat_id, program.known_bytes(id)),
        Some(1000)
    );
}

/// The same bytes, without ever building the chain they stand for. `decode`
/// and the tree a caller hands `intern` both cost the magnitude; these do not.
#[test]
fn a_number_and_a_run_of_bytes_can_be_written_outright() {
    let (mut program, nat_id, byte, list_id) = types();
    let chain = program.intern(nat_id, &nat(&program, nat_id, 1000));
    let direct = program.intern_number(nat_id, 1000);
    assert_eq!(program.known_bytes(chain), program.known_bytes(direct));

    let built = list(&program, list_id, byte, b"Hello");
    let chain = program.intern(list_id, &built);
    let direct = program.intern_bytes(list_id, b"Hello");
    assert_eq!(program.known_bytes(chain), program.known_bytes(direct));
}

#[test]
fn an_ascii_string_is_its_own_bytes_after_the_count() {
    let (mut program, _, byte, list_id) = types();
    let hello = list(&program, list_id, byte, b"Hello");
    let id = program.intern(list_id, &hello);
    assert_eq!(program.known_bytes(id), b"\x05Hello");

    let lifted = as_bytes(&program, list_id, program.known_bytes(id));
    assert_eq!(lifted, Some(&b"Hello"[..]), "liftable as it stands");
    assert_eq!(decode(&program, list_id, program.known_bytes(id)).0, hello);
}

/// A byte past 127 takes two bytes to encode, so the run is no longer the
/// value and a target has to decode it.
#[test]
fn a_string_past_ascii_is_not_liftable() {
    let (mut program, _, byte, list_id) = types();
    let cafe = list(&program, list_id, byte, "café".as_bytes());
    let id = program.intern(list_id, &cafe);
    assert!(as_bytes(&program, list_id, program.known_bytes(id)).is_none());
    assert_eq!(decode(&program, list_id, program.known_bytes(id)).0, cafe);
}

#[test]
fn a_record_costs_no_tag_and_packs_into_a_list() {
    let (mut program, nat_id, _, _) = types();
    let point = program.declare_type("Point");
    program.define_type(point, &[("MkPoint", &[nat_id, nat_id])]);
    let points = program.declare_type("List_Point");
    program.define_type(points, &[("Nil", &[]), ("Cons", &[point, points])]);

    let at = |program: &Program, x, y| {
        Value(
            program.ctor_at(point, 0),
            vec![nat(program, nat_id, x), nat(program, nat_id, y)],
        )
    };
    let mut value = Value(program.ctor_at(points, 0), Vec::new());
    for (x, y) in [(3u64, 4u64), (1, 2)] {
        value = Value(program.ctor_at(points, 1), vec![at(&program, x, y), value]);
    }
    let id = program.intern(points, &value);
    assert_eq!(
        program.known_bytes(id),
        [2, 1, 2, 3, 4],
        "count, then packed pairs"
    );
    assert_eq!(decode(&program, points, program.known_bytes(id)).0, value);
}

/// `add(a, b)` by recursion on the second argument, which is the shape every
/// numeric operation has before the machine tier recognises it.
#[test]
fn the_builder_writes_the_lets_itself() {
    let (mut program, nat, _, _) = types();
    let add = program.declare("add", &[nat, nat], nat);
    program.define(add, |b, args| {
        let (left, right) = (args[0], args[1]);
        b.match_(right, nat, |b, ctor, fields| match fields {
            // Zero: the left operand, untouched.
            [] => left,
            // Succ(k): Succ(add(left, k)).
            [k] => {
                let rest = b.call(add, &[left, *k]);
                b.con(ctor, &[rest])
            }
            _ => unreachable!("Nat takes at most one field"),
        })
    });

    // Two parameters, `k` in the recursive arm, and the call bound inside it.
    // Neither the constructor nor the match binds anything: each is the tail
    // of its body.
    assert_eq!(program.locals(add), 4);

    let body = program.function(add).body;
    assert!(
        program.bindings(body.bindings).is_empty(),
        "the match is the tail, with nothing bound before it"
    );
    let Expr::Match(scrutinee, arms) = program.expr(body.tail) else {
        panic!("the body answers with the match itself");
    };
    assert_eq!(scrutinee, Atom(Local(1)), "the second parameter");
    let arms = program.arms(arms).to_vec();
    assert_eq!(arms.len(), 2, "one arm per constructor, by construction");
    assert_eq!(
        program.expr(arms[0].body.tail),
        Expr::Atom(Atom(Local(0))),
        "Zero answers the left operand"
    );

    // The recursive arm binds its call and answers the constructor.
    let recursive = arms[1].body;
    let [call] = program.bindings(recursive.bindings) else {
        panic!("one binding, for the call");
    };
    assert!(matches!(program.expr(*call), Expr::Call(..)));
    assert!(matches!(program.expr(recursive.tail), Expr::Con(..)));
}

#[test]
fn two_and_two_reads_back_as_what_was_written() {
    let program = crate::two_and_two();
    assert_eq!(
        program.render(),
        "Nat = Zero | Succ(Nat)

add(v0: Nat, v1: Nat) -> Nat =
  match v1 {
    Zero =>
      v0
    Succ(v2) =>
      v3 = add(v0, v2)
      Succ(v3)
  }

main() -> Nat =
  v0 = 2
  add(v0, v0)
"
    );
}

#[test]
fn addition_is_recognised_and_its_near_miss_is_not() {
    let program = crate::two_and_two();
    let facts = recognise(&program);
    let add = FnId::at(0);
    assert_eq!(program.name(program.function(add).name), "add");
    assert_eq!(facts.operation(add), Some(Operation::Add));
    assert_eq!(
        facts.operation(FnId::at(1)),
        None,
        "main takes no arguments"
    );

    // The same shape, answering the argument it counted down rather than the
    // one it kept. That is the identity on its second argument, not addition.
    let mut program = Program::default();
    let nat = program.declare_type("Nat");
    program.define_type(nat, &[("Zero", &[]), ("Succ", &[nat])]);
    let wrong = program.declare("wrong", &[nat, nat], nat);
    program.define(wrong, |b, args| {
        let right = args[1];
        b.match_(right, nat, |b, ctor, fields| match fields {
            [] => right,
            [k] => {
                let rest = b.call(wrong, &[args[0], *k]);
                b.con(ctor, &[rest])
            }
            _ => unreachable!("Nat takes at most one field"),
        })
    });
    assert_eq!(recognise(&program).operation(wrong), None);
}

/// The surface: the same program as [`crate::two_and_two`], written out.
#[test]
fn a_program_reads_from_text() {
    let program = parse(
        "type Nat = Zero | Succ(Nat)

         add(a Zero) -> a
         add(a Succ(k)) -> Succ(add(a k))

         main() -> add(Succ(Succ(Zero)) Succ(Succ(Zero)))",
    )
    .expect("a program");
    assert_eq!(
        program.render(),
        "Nat = Zero | Succ(Nat)

add(v0: Nat, v1: Nat) -> Nat =
  match v1 {
    Zero =>
      v0
    Succ(v2) =>
      v3 = add(v0, v2)
      Succ(v3)
  }

main() -> Nat =
  v0 = Zero
  v1 = Succ(v0)
  v2 = Succ(v1)
  v3 = Zero
  v4 = Succ(v3)
  v5 = Succ(v4)
  add(v2, v5)
"
    );
}

#[test]
fn what_the_surface_will_not_read_it_names() {
    let two = "type Nat = Zero | Succ(Nat)
               f(Zero Zero) -> Zero
               f(a b) -> a";
    assert!(parse(two).unwrap_err().contains("one at a time"));

    let comma = "type Nat = Zero | Succ(Nat)
                 f(a, b) -> a";
    assert!(parse(comma).unwrap_err().contains("found `,`"));

    let missing = "type Nat = Zero | Succ(Nat)
                   f(Zero) -> g(Zero)";
    assert!(parse(missing).unwrap_err().contains("nothing named `g`"));
}

/// Steps, which is what a body needs once anything answers something worth
/// keeping: each is bound where it was named, in the order written.
#[test]
fn a_body_works_things_out_on_the_way() {
    let program = parse(
        "type Nat = Zero | Succ(Nat)

         double(n) ->
           m = add(n n)
           m

         add(a Zero) -> a
         add(a Succ(k)) -> Succ(add(a k))

         main() ->
           two = Succ(Succ(Zero))
           four = double(two)
           four",
    )
    .expect("a program");
    assert_eq!(
        program.render(),
        "Nat = Zero | Succ(Nat)

double(v0: Nat) -> Nat =
  add(v0, v0)

add(v0: Nat, v1: Nat) -> Nat =
  match v1 {
    Zero =>
      v0
    Succ(v2) =>
      v3 = add(v0, v2)
      Succ(v3)
  }

main() -> Nat =
  v0 = Zero
  v1 = Succ(v0)
  v2 = Succ(v1)
  double(v2)
"
    );
}
