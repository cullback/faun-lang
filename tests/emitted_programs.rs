//! Compile for each target, then actually run the result.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use faun::ir::Class::Word;
use faun::ir::{Binary, Class, Offset, Program, Relation, Terminator, Width};
use faun::targets::Target;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_prints_hello_world() {
    let (output, bytes) = run(Target::X86_64Linux, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
    assert_eq!(bytes, 158, "the executable changed size");
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
}

#[test]
fn the_wasm32_wasi_module_prints_hello_world() {
    let (output, bytes) = run(Target::Wasm32Wasi, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
    assert_eq!(bytes, 141, "the module changed size");
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_smallest_x86_64_linux_executable_is_headers_and_an_exit() {
    let (output, bytes) = run(Target::X86_64Linux, &empty());
    assert_eq!(output, "");
    // 120 bytes of header, then `push 60; pop rax; xor edi, edi; syscall`.
    assert_eq!(bytes, 127);
}

#[test]
fn the_smallest_wasm32_wasi_module_imports_nothing() {
    let (output, bytes) = run(Target::Wasm32Wasi, &empty());
    assert_eq!(output, "");
    // Type, function, export, code. No imports, no memory, no data.
    assert_eq!(bytes, 39);
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
        Terminator::Return(vec![status])
    });
    program
}

/// `main` calls `double(21)` and exits with it. The point is the call, so
/// the answer is the exit status rather than anything printed.
fn calls_a_function() -> Program {
    let (mut program, main) = Program::new("main");
    let double = program.declare("double", vec![Word], vec![Word]);
    program.define(double, |b, params| {
        let n = params[0];
        Terminator::Return(vec![b.binary(Binary::Add, n, n)])
    });

    program.define(main, |b, _| {
        let twenty_one = b.constant(Word, 21);
        let answer = b.call(double, vec![twenty_one]);
        Terminator::Return(answer)
    });
    program
}

/// `sum(n)` recurses, so the call graph has a cycle and the callee needs a
/// frame that survives its own recursive call. 1 + 2 + 3 = 6.
fn recurses() -> Program {
    let (mut program, main) = Program::new("main");
    let sum = program.declare("sum", vec![Word], vec![Word]);
    program.define(sum, |b, params| {
        let n = params[0];
        let zero = b.constant(Word, 0);
        let done = b.compare(Relation::Equal, n, zero);
        let answer = b.if_(
            done,
            vec![Word],
            |b| {
                let zero = b.constant(Word, 0);
                Terminator::Yield(vec![zero])
            },
            |b| {
                let one = b.constant(Word, 1);
                let less = b.binary(Binary::Sub, n, one);
                let rest = b.call(sum, vec![less]);
                Terminator::Yield(vec![b.binary(Binary::Add, n, rest[0])])
            },
        );
        Terminator::Return(answer)
    });

    program.define(main, |b, _| {
        let three = b.constant(Word, 3);
        Terminator::Return(b.call(sum, vec![three]))
    });
    program
}

/// Ask the platform for a page, write two words into it, read one back.
/// Exits with what it read, so nothing but real memory can make it pass.
fn uses_the_heap() -> Program {
    let (mut program, main) = Program::new("main");
    let grow = program.platform("grow", vec![Word], vec![Class::Address]);

    program.define(main, |b, _| {
        let size = b.constant(Word, 4096);
        let page = b.platform_call(grow, vec![size])[0];

        let answer = b.constant(Word, 42);
        let decoy = b.constant(Word, 7);
        b.store(Width::Word, page, Offset::Words(0), decoy);
        b.store(Width::Word, page, Offset::Words(1), answer);

        Terminator::Return(vec![b.load(Width::Word, page, Offset::Words(1))])
    });
    program
}

/// A byte store must not disturb its neighbour, and a byte above 127 must
/// come back zero-extended rather than as a very large word. Answers
/// `(first == 200) + second`, so both have to hold to reach 8.
fn stores_a_byte() -> Program {
    let (mut program, main) = Program::new("main");
    let grow = program.platform("grow", vec![Word], vec![Class::Address]);

    program.define(main, |b, _| {
        let size = b.constant(Word, 4096);
        let page = b.platform_call(grow, vec![size])[0];

        let high = b.constant(Word, 200);
        b.store(Width::Byte, page, Offset::Bytes(0), high);
        let neighbour = b.constant(Word, 7);
        b.store(Width::Byte, page, Offset::Bytes(1), neighbour);

        let first = b.load(Width::Byte, page, Offset::Bytes(0));
        let second = b.load(Width::Byte, page, Offset::Bytes(1));
        let expected = b.constant(Word, 200);
        let intact = b.compare(Relation::Equal, first, expected);
        Terminator::Return(vec![b.binary(Binary::Add, intact, second)])
    });
    program
}

/// A counter in a writable datum, bumped by a function called three times.
/// Nothing but storage outliving a call can answer 3.
fn counts_in_a_global() -> Program {
    let (mut program, main) = Program::new("main");
    let counter = program.global(&[0; 8]);

    let bump = program.declare("bump", Vec::new(), Vec::new());
    program.define(bump, |b, _| {
        let at = b.address_of(counter);
        let seen = b.load(Width::Word, at, Offset::Words(0));
        let one = b.constant(Word, 1);
        let next = b.binary(Binary::Add, seen, one);
        b.store(Width::Word, at, Offset::Words(0), next);
        Terminator::Return(Vec::new())
    });

    program.define(main, |b, _| {
        b.call(bump, Vec::new());
        b.call(bump, Vec::new());
        b.call(bump, Vec::new());
        let at = b.address_of(counter);
        Terminator::Return(vec![b.load(Width::Word, at, Offset::Words(0))])
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
        Terminator::Return(vec![b.convert(Word, sum)])
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
        Terminator::Return(vec![b.compare(Relation::Less, left, right)])
    });
    program
}

/// `while n != 0 { print "tick"; n -= 1 }`, written the way the IR has it:
/// a loop carrying `n`, and an `if` that either breaks or goes round again.
fn countdown(times: u64) -> Program {
    let (mut program, main) = Program::new("main");
    let tick = program.intern(b"tick\n".as_slice());
    let write = program.platform("write", vec![Class::Address, Word], Vec::new());

    program.define(main, |b, _| {
        let start = b.constant(Word, times);
        b.loop_(vec![start], Vec::new(), |b, params| {
            let n = params[0];
            let zero = b.constant(Word, 0);
            let done = b.compare(Relation::Equal, n, zero);
            b.if_(
                done,
                Vec::new(),
                |_| Terminator::Break(Vec::new()),
                |b| {
                    let buf = b.address_of(tick);
                    let len = b.constant(Word, 5);
                    b.platform_call(write, vec![buf, len]);
                    let one = b.constant(Word, 1);
                    let next = b.binary(Binary::Sub, n, one);
                    Terminator::Continue(vec![next])
                },
            );
            Terminator::Unreachable
        });

        let status = b.constant(Word, 0);
        Terminator::Return(vec![status])
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
