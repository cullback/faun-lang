//! Compile for each target, then actually run the result.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use faun::targets::Target;

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn the_x86_64_linux_executable_prints_hello_world() {
    let dir = scratch("x86-64");
    let path = dir.join("hello");
    let bytes = build(Target::X86_64Linux, &path);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = Command::new(&path).output().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert!(output.status.success(), "exited with {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Hello, World!\n");
    assert_eq!(bytes, 156, "the executable changed size");
}

#[test]
fn the_wasm32_wasi_module_prints_hello_world() {
    // The dev shell provides wasmtime; outside it, say so rather than fail.
    if Command::new("wasmtime").arg("--version").output().is_err() {
        eprintln!("skipping: wasmtime is not on PATH");
        return;
    }

    let dir = scratch("wasm32");
    let path = dir.join("hello.wasm");
    let bytes = build(Target::Wasm32Wasi, &dir.join("hello"));

    let output = Command::new("wasmtime").arg(&path).output().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert!(output.status.success(), "exited with {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Hello, World!\n");
    assert_eq!(bytes, 141, "the module changed size");
}

/// Compile hello world for `target`, writing it next to `output`. Returns the
/// size of the single file every target produces so far.
fn build(target: Target, output: &Path) -> usize {
    let artifacts = faun::compile(target, &faun::hello_world());
    assert_eq!(artifacts.len(), 1);

    let mut path = output.to_path_buf().into_os_string();
    path.push(artifacts[0].extension);
    fs::write(PathBuf::from(path), &artifacts[0].bytes).unwrap();

    artifacts[0].bytes.len()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("faun-test-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}
