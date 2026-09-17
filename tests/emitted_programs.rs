//! Compile for each target, then actually run the result.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use faun::ir::{Binary, Class, Program, Relation, Terminator};
use faun::targets::Target;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_prints_hello_world() {
    let (output, bytes) = run(Target::X86_64Linux, &faun::hello_world());
    assert_eq!(output, "Hello, World!\n");
    assert_eq!(bytes, 158, "the executable changed size");
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
    assert_eq!(bytes, 40);
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
    let mut program = Program::new();
    program.build(|_| Terminator::Return(Vec::new()));
    program
}

/// `while n != 0 { print "tick"; n -= 1 }`, written the way the IR has it:
/// a loop carrying `n`, and an `if` that either breaks or goes round again.
fn countdown(times: u64) -> Program {
    let mut program = Program::new();
    let tick = program.intern(b"tick\n".as_slice());
    let write = program.platform("write", vec![Class::Address, Class::Word], Vec::new());
    let exit = program.platform("exit", vec![Class::Word], Vec::new());

    program.build(|b| {
        let start = b.constant(Class::Word, times);
        b.loop_(vec![start], Vec::new(), |b, params| {
            let n = params[0];
            let zero = b.constant(Class::Word, 0);
            let done = b.compare(Relation::Equal, n, zero);
            b.if_(
                done,
                Vec::new(),
                |_| Terminator::Break(Vec::new()),
                |b| {
                    let buf = b.address_of(tick);
                    let len = b.constant(Class::Word, 5);
                    b.call(write, vec![buf, len]);
                    let one = b.constant(Class::Word, 1);
                    let next = b.binary(Binary::Sub, n, one);
                    Terminator::Continue(vec![next])
                },
            );
            Terminator::Unreachable
        });

        let status = b.constant(Class::Word, 0);
        b.call(exit, vec![status]);
        Terminator::Unreachable
    });

    program
}

/// Compile, write, execute. Returns what the program printed and how big it
/// was, so a test can pin both.
///
/// Serialised because the two halves race: another thread forking for its own
/// `Command` inherits this one's still-open write descriptor, and the exec
/// that follows fails with `ETXTBSY`.
fn run(target: Target, program: &Program) -> (String, usize) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
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
    };
    let size = usize::try_from(fs::metadata(&path).unwrap().len()).unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert!(output.status.success(), "exited with {:?}", output.status);
    (String::from_utf8(output.stdout).unwrap(), size)
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
