use std::error::Error;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as Process;
use std::process::ExitCode;

use bpaf::OptionParser;
use bpaf::Parser;
use bpaf::choice;
use bpaf::construct;
use bpaf::long;
use bpaf::positional;
use bpaf::short;

use faun::targets::Target;

#[derive(Clone, Debug)]
enum Command {
    Build {
        source: PathBuf,
        output: PathBuf,
        target: Target,
    },
    Run {
        source: PathBuf,
    },
}

fn cli() -> OptionParser<Command> {
    let source = || positional::<PathBuf>("PATH").help("The program to compile");

    let output = short('o')
        .long("output")
        .help("Where to write the result")
        .argument::<PathBuf>("PATH")
        .fallback(PathBuf::from("a.out"))
        .debug_fallback();

    let target = long("target")
        .help("Machine to compile for")
        .argument::<String>("TARGET")
        .parse(|name| Target::from_name(&name).ok_or_else(|| unknown(&name)))
        .fallback_with(|| Target::HOST.ok_or(NO_HOST))
        .debug_fallback();

    let build = construct!(Command::Build {
        output,
        target,
        source(),
    })
    .to_options()
    .descr("Compile the program to an executable")
    .command("build");

    let run = construct!(Command::Run { source() })
        .to_options()
        .descr("Compile the program for this machine and run it")
        .command("run");

    choice([build.boxed(), run.boxed()])
        .to_options()
        .descr("The Faun compiler")
        .version(env!("CARGO_PKG_VERSION"))
}

const NO_HOST: &str = "no backend for this machine, so --target is required";

fn unknown(name: &str) -> String {
    let known: Vec<_> = Target::ALL.into_iter().map(Target::name).collect();
    format!("unknown target `{name}`, expected: {}", known.join(", "))
}

fn main() -> ExitCode {
    match dispatch(cli().run()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("faun: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(command: Command) -> Result<ExitCode, Box<dyn Error>> {
    match command {
        Command::Build {
            source,
            output,
            target,
        } => {
            let program = read(&source)?;
            for path in write(target, &output, &program)? {
                println!("wrote {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Run { source } => {
            let program = read(&source)?;
            let target = Target::HOST.ok_or(NO_HOST)?;
            let dir = std::env::temp_dir().join(format!("faun-run-{}", std::process::id()));
            fs::create_dir_all(&dir)?;

            let status = write(target, &dir.join("program"), &program)?
                .first()
                .map(Process::new)
                .expect("a target emits at least one file")
                .status();
            fs::remove_dir_all(&dir)?;

            let code = status?.code().unwrap_or(1);
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
        }
    }
}

/// The program at `path`, read and lowered.
fn read(path: &Path) -> Result<faun::ir::Program, Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    faun::read(&text).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn write(
    target: Target,
    output: &Path,
    program: &faun::ir::Program,
) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    for artifact in faun::compile(target, program) {
        let mut path = output.to_path_buf().into_os_string();
        path.push(artifact.extension);
        let path = PathBuf::from(path);

        fs::write(&path, &artifact.bytes)?;

        #[cfg(unix)]
        if artifact.executable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }

        written.push(path);
    }

    Ok(written)
}
