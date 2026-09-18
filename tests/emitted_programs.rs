//! Compile for each target, then actually run the result.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use faun::ir::Class::Word;
use faun::ir::{Binary, Class, Offset, Program, Relation, Width};
use faun::targets::Target;

/// What the smallest programs compile to, for every target. Ratchets: lower
/// one when a target improves, and do not raise one without saying why.
#[test]
fn minimal_programs_compile_to_the_sizes_recorded_for_them() {
    use Target::{Mos6502Sim65, Wasm32Wasi, X86_64Linux};
    for (name, program, sizes) in [
        (
            "empty",
            empty(),
            [(X86_64Linux, 127), (Wasm32Wasi, 39), (Mos6502Sim65, 30)],
        ),
        (
            "two and two",
            two_and_two(),
            [(X86_64Linux, 136), (Wasm32Wasi, 105), (Mos6502Sim65, 45)],
        ),
        (
            "hello world",
            faun::hello_world(),
            [(X86_64Linux, 158), (Wasm32Wasi, 141), (Mos6502Sim65, 63)],
        ),
    ] {
        for (target, size) in sizes {
            let bytes = faun::compile(target, &program)[0].bytes.len();
            assert_eq!(bytes, size, "{name} for {target}");
        }
    }
}

#[test]
fn every_target_adds_two_and_two() {
    for target in Target::ALL {
        assert_eq!(status(target, &two_and_two()), 4, "{target}");
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_prints_hello_world() {
    let (output, _) = run(Target::X86_64Linux, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
}

#[test]
fn the_mos6502_image_prints_hello_world() {
    let (output, _) = run(Target::Mos6502Sim65, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
}

#[test]
fn every_target_counts_in_a_global() {
    for target in Target::ALL {
        assert_eq!(status(target, &counts_in_a_global()), 3, "{target}");
    }
}

#[test]
fn every_target_wraps_a_fixed_width() {
    for target in Target::ALL {
        assert_eq!(status(target, &wraps_at_eight_bits()), 4, "{target}");
    }
}

#[test]
fn the_mos6502_image_runs_every_program() {
    assert_eq!(status(Target::Mos6502Sim65, &empty()), 0);
    assert_eq!(status(Target::Mos6502Sim65, &less(7, 9)), 1);
    assert_eq!(status(Target::Mos6502Sim65, &less(9, 7)), 0);
    assert_eq!(status(Target::Mos6502Sim65, &calls_a_function()), 42);
    assert_eq!(status(Target::Mos6502Sim65, &recurses()), 6);
    assert_eq!(
        run(Target::Mos6502Sim65, &countdown(3)).0,
        "tick\n".repeat(3)
    );
    assert_eq!(status(Target::Mos6502Sim65, &uses_the_heap()), 42);
    assert_eq!(status(Target::Mos6502Sim65, &stores_a_byte()), 8);
    assert_eq!(run(Target::Mos6502Sim65, &writes_from_the_heap()).0, "hi\n");
    assert_eq!(status(Target::Mos6502Sim65, &values_survive_a_call()), 45);
}

#[test]
fn every_target_hands_back_what_a_call_would_destroy() {
    for target in Target::ALL {
        assert_eq!(status(target, &values_survive_a_call()), 45, "{target}");
    }
}

#[test]
fn every_target_writes_from_the_heap() {
    for target in Target::ALL {
        assert_eq!(run(target, &writes_from_the_heap()).0, "hi\n", "{target}");
    }
}

#[test]
fn the_wasm32_wasi_module_prints_hello_world() {
    let (output, _) = run(Target::Wasm32Wasi, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_smallest_x86_64_linux_executable_is_headers_and_an_exit() {
    // 120 bytes of header, then `push 60; pop rax; xor edi, edi; syscall`.
    let (output, _) = run(Target::X86_64Linux, &empty());
    assert_eq!(output, "");
}

#[test]
fn the_smallest_wasm32_wasi_module_imports_nothing() {
    // Type, function, export, code. No imports, no memory, no data.
    let (output, _) = run(Target::Wasm32Wasi, &empty());
    assert_eq!(output, "");
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_uses_the_heap() {
    assert_eq!(status(Target::X86_64Linux, &uses_the_heap()), 42);
    assert_eq!(status(Target::X86_64Linux, &stores_a_byte()), 8);
}

#[test]
fn the_wasm32_wasi_module_uses_the_heap() {
    assert_eq!(status(Target::Wasm32Wasi, &uses_the_heap()), 42);
    assert_eq!(status(Target::Wasm32Wasi, &stores_a_byte()), 8);
}

/// A call and a return, on each target's own calling convention.
#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_calls_a_function() {
    assert_eq!(status(Target::X86_64Linux, &calls_a_function()), 42);
    assert_eq!(status(Target::X86_64Linux, &recurses()), 6);
}

#[test]
fn the_wasm32_wasi_module_calls_a_function() {
    assert_eq!(status(Target::Wasm32Wasi, &calls_a_function()), 42);
    assert_eq!(status(Target::Wasm32Wasi, &recurses()), 6);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_compares() {
    assert_eq!(status(Target::X86_64Linux, &less(7, 9)), 1);
    assert_eq!(status(Target::X86_64Linux, &less(9, 7)), 0);
}

#[test]
fn the_wasm32_wasi_module_compares() {
    assert_eq!(status(Target::Wasm32Wasi, &less(7, 9)), 1);
    assert_eq!(status(Target::Wasm32Wasi, &less(9, 7)), 0);
}

/// The IR's loop has to survive into both targets, which have nothing in
/// common about how they branch. Running it is the only proof that counts.
#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_counts_down() {
    assert_eq!(
        run(Target::X86_64Linux, &countdown(3)).0,
        "tick\n".repeat(3)
    );
    assert_eq!(run(Target::X86_64Linux, &countdown(0)).0, "");
}

#[test]
fn the_wasm32_wasi_module_counts_down() {
    assert_eq!(run(Target::Wasm32Wasi, &countdown(3)).0, "tick\n".repeat(3));
    assert_eq!(run(Target::Wasm32Wasi, &countdown(0)).0, "");
}

/// Nothing at all. A target's floor: headers, and whatever it takes to stop.
fn empty() -> Program {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let status = b.constant(Word, 0);
        b.ret(&[status])
    });
    program
}

fn two_and_two() -> Program {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let two = b.constant(Word, 2);
        let sum = b.binary(Binary::Add, two, two);
        b.ret(&[sum])
    });
    program
}

/// `main` calls `double(21)` and exits with it. The point is the call, so
/// the answer is the exit status rather than anything printed.
fn calls_a_function() -> Program {
    let (mut program, main) = Program::new("main");
    let double = program.declare("double", &[Word], &[Word]);
    program.define(double, |b, params| {
        let n = params[0];
        let doubled = b.binary(Binary::Add, n, n);
        b.ret(&[doubled])
    });

    program.define(main, |b, _| {
        let twenty_one = b.constant(Word, 21);
        let answer = b.call(double, &[twenty_one]);
        b.ret(&answer)
    });
    program
}

/// `sum(n)` recurses, so the call graph has a cycle and the callee needs a
/// frame that survives its own recursive call. 1 + 2 + 3 = 6.
fn recurses() -> Program {
    let (mut program, main) = Program::new("main");
    let sum = program.declare("sum", &[Word], &[Word]);
    program.define(sum, |b, params| {
        let n = params[0];
        let zero = b.constant(Word, 0);
        let done = b.compare(Relation::Equal, n, zero);
        let answer = b.if_(
            done,
            &[Word],
            |b| {
                let zero = b.constant(Word, 0);
                b.yield_(&[zero])
            },
            |b| {
                let one = b.constant(Word, 1);
                let less = b.binary(Binary::Sub, n, one);
                let rest = b.call(sum, &[less]);
                let total = b.binary(Binary::Add, n, rest[0]);
                b.yield_(&[total])
            },
        );
        b.ret(&answer)
    });

    program.define(main, |b, _| {
        let three = b.constant(Word, 3);
        let answer = b.call(sum, &[three]);
        b.ret(&answer)
    });
    program
}

/// Ask the platform for a page, write two words into it, read one back.
/// Exits with what it read, so nothing but real memory can make it pass.
fn uses_the_heap() -> Program {
    let (mut program, main) = Program::new("main");
    let grow = program.platform("grow", &[Word], &[Class::Address]);

    program.define(main, |b, _| {
        let size = b.constant(Word, 4096);
        let page = b.platform_call(grow, &[size])[0];

        let answer = b.constant(Word, 42);
        let decoy = b.constant(Word, 7);
        b.store(Width::Word, page, Offset::Words(0), decoy);
        b.store(Width::Word, page, Offset::Words(1), answer);

        let answer = b.load(Width::Word, page, Offset::Words(1));
        b.ret(&[answer])
    });
    program
}

/// Several values still wanted after a call, and a callee greedy enough to
/// take every pair it can. Anything the caller fails to hand back shows up
/// in the answer.
///
/// `clobber(x)` is `x + 2` through three intermediates; `outer(10, 3)` is
/// `13 + 7 + 12 + 10 + 3`.
fn values_survive_a_call() -> Program {
    let (mut program, main) = Program::new("main");
    let clobber = program.declare("clobber", &[Word], &[Word]);
    let outer = program.declare("outer", &[Word, Word], &[Word]);

    program.define(clobber, |b, params| {
        let x = params[0];
        let one = b.constant(Word, 1);
        let two = b.constant(Word, 2);
        let three = b.constant(Word, 3);
        let up = b.binary(Binary::Add, x, one);
        let down = b.binary(Binary::Sub, up, two);
        let back = b.binary(Binary::Add, down, three);
        b.ret(&[back])
    });

    program.define(outer, |b, params| {
        let (a, second) = (params[0], params[1]);
        let p = b.binary(Binary::Add, a, second);
        let q = b.binary(Binary::Sub, a, second);
        let r = b.call(clobber, &[a])[0];
        // Every one of these was computed before the call.
        let total = b.binary(Binary::Add, p, q);
        let total = b.binary(Binary::Add, total, r);
        let total = b.binary(Binary::Add, total, a);
        let total = b.binary(Binary::Add, total, second);
        b.ret(&[total])
    });

    program.define(main, |b, _| {
        let ten = b.constant(Word, 10);
        let three = b.constant(Word, 3);
        let answer = b.call(outer, &[ten, three]);
        b.ret(&answer)
    });
    program
}

/// Writes through an address the program worked out for itself, which is
/// the case a target cannot answer with a block of constants.
fn writes_from_the_heap() -> Program {
    let (mut program, main) = Program::new("main");
    let grow = program.platform("grow", &[Word], &[Class::Address]);
    let write = program.platform("write", &[Class::Address, Word], &[]);

    program.define(main, |b, _| {
        let size = b.constant(Word, 4096);
        let page = b.platform_call(grow, &[size])[0];

        // "hi\n", a byte at a time, since the bytes are computed too.
        for (index, byte) in [b'h', b'i', b'\n'].into_iter().enumerate() {
            let value = b.constant(Word, u64::from(byte));
            let offset = Offset::Bytes(i32::try_from(index).expect("a short greeting"));
            b.store(Width::Byte, page, offset, value);
        }

        let length = b.constant(Word, 3);
        b.platform_call(write, &[page, length]);
        let status = b.constant(Word, 0);
        b.ret(&[status])
    });
    program
}

/// A byte store must not disturb its neighbour, and a byte above 127 must
/// come back zero-extended rather than as a very large word. Answers
/// `(first == 200) + second`, so both have to hold to reach 8.
fn stores_a_byte() -> Program {
    let (mut program, main) = Program::new("main");
    let grow = program.platform("grow", &[Word], &[Class::Address]);

    program.define(main, |b, _| {
        let size = b.constant(Word, 4096);
        let page = b.platform_call(grow, &[size])[0];

        let high = b.constant(Word, 200);
        b.store(Width::Byte, page, Offset::Bytes(0), high);
        let neighbour = b.constant(Word, 7);
        b.store(Width::Byte, page, Offset::Bytes(1), neighbour);

        let first = b.load(Width::Byte, page, Offset::Bytes(0));
        let second = b.load(Width::Byte, page, Offset::Bytes(1));
        let expected = b.constant(Word, 200);
        let intact = b.compare(Relation::Equal, first, expected);
        let answer = b.binary(Binary::Add, intact, second);
        b.ret(&[answer])
    });
    program
}

/// A counter in a writable datum, bumped by a function called three times.
/// Nothing but storage outliving a call can answer 3.
fn counts_in_a_global() -> Program {
    let (mut program, main) = Program::new("main");
    let counter = program.global(&[0; 8]);

    let bump = program.declare("bump", &[], &[]);
    program.define(bump, |b, _| {
        let at = b.address_of(counter);
        let seen = b.load(Width::Word, at, Offset::Words(0));
        let one = b.constant(Word, 1);
        let next = b.binary(Binary::Add, seen, one);
        b.store(Width::Word, at, Offset::Words(0), next);
        b.ret(&[])
    });

    program.define(main, |b, _| {
        b.call(bump, &[]);
        b.call(bump, &[]);
        b.call(bump, &[]);
        let at = b.address_of(counter);
        let answer = b.load(Width::Word, at, Offset::Words(0));
        b.ret(&[answer])
    });
    program
}

/// `Fixed { bits: 8 }` is arithmetic in Z/256, so 250 + 10 is 4 on every
/// target: free on a machine with eight-bit registers, a mask on the two
/// that are wider.
fn wraps_at_eight_bits() -> Program {
    let byte = Class::Fixed { bits: 8 };
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let left = b.constant(byte, 250);
        let right = b.constant(byte, 10);
        let sum = b.binary(Binary::Add, left, right);
        // The entry returns a word, and this is where the width changes.
        let answer = b.convert(Word, sum);
        b.ret(&[answer])
    });
    program
}

/// `Relation::Less` is unsigned, and both targets carry an arm for it that
/// nothing else exercises.
fn less(left: u64, right: u64) -> Program {
    let (mut program, main) = Program::new("main");
    program.define(main, |b, _| {
        let left = b.constant(Word, left);
        let right = b.constant(Word, right);
        let answer = b.compare(Relation::Less, left, right);
        b.ret(&[answer])
    });
    program
}

/// `while n != 0 { print "tick"; n -= 1 }`, written the way the IR has it:
/// a loop carrying `n`, and an `if` that either breaks or goes round again.
fn countdown(times: u64) -> Program {
    let (mut program, main) = Program::new("main");
    let tick = program.intern(b"tick\n".as_slice());
    let write = program.platform("write", &[Class::Address, Word], &[]);

    program.define(main, |b, _| {
        let start = b.constant(Word, times);
        b.loop_(&[start], &[], |b, params| {
            let n = params[0];
            let zero = b.constant(Word, 0);
            let done = b.compare(Relation::Equal, n, zero);
            b.if_(
                done,
                &[],
                |b| b.break_(&[]),
                |b| {
                    let buf = b.address_of(tick);
                    let len = b.constant(Word, 5);
                    b.platform_call(write, &[buf, len]);
                    let one = b.constant(Word, 1);
                    let next = b.binary(Binary::Sub, n, one);
                    b.continue_(&[next])
                },
            );
            faun::ir::Terminator::UNREACHABLE
        });

        let status = b.constant(Word, 0);
        b.ret(&[status])
    });

    program
}

/// The exit status a program leaves with, which is what `main` returns.
/// WASI rejects anything outside `[0, 126)`, so a test that wants to check a
/// larger number has to reduce it to a small one first.
fn status(target: Target, program: &Program) -> i32 {
    execute(target, program).0
}

/// Compile, write, execute. Every program is validated first, since a class
/// disagreement is the kind of thing a target miscompiles in silence.
///
/// Returns what the program printed and how big it
/// was, so a test can pin both.
///
/// Serialised because the two halves race: another thread forking for its own
/// `Command` inherits this one's still-open write descriptor, and the exec
/// that follows fails with `ETXTBSY`.
fn run(target: Target, program: &Program) -> (String, usize) {
    let (code, output, size) = execute(target, program);
    assert_eq!(code, 0, "exited with {code}");
    (output, size)
}

fn execute(target: Target, program: &Program) -> (i32, String, usize) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    faun::ir::validate(program).expect("a well-formed program");
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);

    let dir = std::env::temp_dir().join(format!(
        "faun-{}-{:?}-{}",
        std::process::id(),
        target,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = write(target, &dir.join("program"), program);

    let output = match target {
        Target::X86_64Linux => Command::new(&path).output().unwrap(),
        Target::Wasm32Wasi => Command::new("wasmtime").arg(&path).output().unwrap(),
        Target::Mos6502Sim65 => Command::new("sim65").arg(&path).output().unwrap(),
    };
    let size = usize::try_from(fs::metadata(&path).unwrap().len()).unwrap();
    fs::remove_dir_all(&dir).unwrap();

    let code = output.status.code().expect("the program was not signalled");
    (code, String::from_utf8(output.stdout).unwrap(), size)
}

fn write(target: Target, output: &Path, program: &Program) -> PathBuf {
    let artifacts = faun::compile(target, program);
    assert_eq!(artifacts.len(), 1);

    let mut path = output.to_path_buf().into_os_string();
    path.push(artifacts[0].extension);
    let path = PathBuf::from(path);
    fs::write(&path, &artifacts[0].bytes).unwrap();

    #[cfg(unix)]
    if artifacts[0].executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    path
}

/// The whole way down: `2 + 2` written in barb, lowered through the machine
/// tier, compiled, and run. `Nat` is inductive all the way to the lowering,
/// which is where it becomes a word.
#[test]
fn every_target_runs_two_and_two_from_barb() {
    let barb = faun::two_and_two();
    let facts = faun::barb::recognise(&barb);
    let machine = faun::ir::lower(&barb, &facts).expect("a representable program");

    // `add` was recognised, so it is an instruction rather than a function:
    // what is left is the entry alone.
    assert_eq!(machine.functions().len(), 1, "only the entry survives");
    for target in Target::ALL {
        assert_eq!(status(target, &machine), 4, "{target}");
    }
}

/// The same program again, written out rather than built, and run.
#[test]
fn every_target_runs_two_and_two_from_text() {
    let machine = faun::read(
        "type Nat Zero Succ(Nat)

         add(a Zero) -> a
         add(a Succ(k)) -> Succ(add(a k))

         main!(h) -> exit!(h add(Succ(Succ(Zero)) Succ(Succ(Zero))))",
    )
    .expect("a program");
    for target in Target::ALL {
        assert_eq!(status(target, &machine), 4, "{target}");
    }
}

/// Every example compiles and answers what it says it does, so that one
/// cannot rot into a program that no longer reads.
#[test]
fn every_target_runs_every_example() {
    for entry in fs::read_dir("examples").expect("the examples") {
        let path = entry.expect("an example").path();
        let text = fs::read_to_string(&path).expect("an example to read");
        let machine = faun::read(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for target in Target::ALL {
            assert_eq!(
                status(target, &machine),
                4,
                "{} on {target}",
                path.display()
            );
        }
    }
}

/// The one effect there is: a program that leaves with a status it worked
/// out, through a platform routine it declared and a host it was handed.
#[test]
fn every_target_exits_with_what_it_was_told() {
    let machine = faun::read(
        "type Nat Zero Succ(Nat)

         main!(h) -> exit!(h Succ(Succ(Succ(Zero))))",
    )
    .expect("a program");
    for target in Target::ALL {
        assert_eq!(status(target, &machine), 3, "{target}");
    }
}

/// A pure name may not do observable work.
#[test]
fn a_pure_name_cannot_call_an_effect() {
    let refused = faun::read(
        "type Nat Zero Succ(Nat)

         quietly(h) -> exit!(h Zero)

         main!(h) -> quietly(h)",
    )
    .unwrap_err();
    assert!(
        refused.contains("`quietly` is pure and calls `exit!`"),
        "{refused}"
    );
}
