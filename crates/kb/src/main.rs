//! kb — the KOOMPI Linux build engine.
//!
//! One binary, because the constraint the whole spec serves is that a person
//! can read the core in a day, and that includes the thing that builds it.

mod boot;
mod build;
mod cpio;
mod elf;
mod err;
mod graph;
mod image;
mod lint;
mod provenance;
mod read;
mod recipe;
mod sha256;
mod store;
mod target;
mod toml;

use err::{Error, Result, bail};
use std::path::{Path, PathBuf};

const USAGE: &str = "\
kb — the KOOMPI Linux build engine

  kb lint                              check every recipe against every target
  kb targets                           list the declared targets
  kb build <recipe> --target <name>    build a recipe and everything under it
  kb sysroot <recipe> --target <name>  print the sysroot that recipe builds against
  kb image <target>                    build the target's contents and assemble its image
  kb check-provenance <target>         fail if anything in the image is not ours
  kb boot <target> [--smoke]           boot the image in QEMU; --smoke runs the selftest

options
  --target <name>   required by build and sysroot
  --jobs <n>        parallel make jobs (default: all cores)
  --timeout <secs>  how long --smoke waits for a verdict (default: 300)
  --memory <mib>    guest memory (default: 4096)
";

fn main() {
    if let Err(e) = run() {
        eprintln!("kb: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        print!("{USAGE}");
        return Ok(());
    };

    let root = repo_root()?;
    let recipes_dir = root.join("recipes");

    match command.as_str() {
        "lint" => {
            let recipes = recipe::load_all(&recipes_dir)?;
            let targets = target::load_all(&root)?;
            lint::run(&recipes, &targets)
        }
        "targets" => {
            for t in target::load_all(&root)? {
                println!("{:<20} {}", t.name, t.triple);
            }
            Ok(())
        }
        "build" => {
            let opts = Options::parse(&args[1..])?;
            let Some(name) = opts.positional.first() else {
                bail!("build needs a recipe name\n\n{USAGE}")
            };
            let Some(target_name) = &opts.target else {
                bail!("build needs --target\n\n{USAGE}")
            };

            let recipes = recipe::load_all(&recipes_dir)?;
            let targets = target::load_all(&root)?;
            // a recipe that names an architecture builds fine and breaks the gate silently
            lint::run(&recipes, &targets)?;

            let target = target::Target::load(&root, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            let id = engine.build(&recipes, name, &target)?;
            println!("{}", engine.store.path(&id).display());
            Ok(())
        }
        "sysroot" => {
            let opts = Options::parse(&args[1..])?;
            let Some(name) = opts.positional.first() else {
                bail!("sysroot needs a recipe name\n\n{USAGE}")
            };
            let Some(target_name) = &opts.target else {
                bail!("sysroot needs --target\n\n{USAGE}")
            };
            let recipes = recipe::load_all(&recipes_dir)?;
            let target = target::Target::load(&root, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            println!("{}", engine.sysroot_of(&recipes, name, &target)?.display());
            Ok(())
        }
        "image" => {
            let opts = Options::parse(&args[1..])?;
            let Some(target_name) = opts.positional.first() else {
                bail!("image needs a target name\n\n{USAGE}")
            };
            let recipes = recipe::load_all(&recipes_dir)?;
            let targets = target::load_all(&root)?;
            lint::run(&recipes, &targets)?;
            let target = target::Target::load(&root, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            let image = image::assemble(&engine, &recipes, &target)?;
            println!("{}", image.dir.display());
            Ok(())
        }
        "check-provenance" => {
            let opts = Options::parse(&args[1..])?;
            let Some(target_name) = opts.positional.first() else {
                bail!("check-provenance needs a target name\n\n{USAGE}")
            };
            let target = target::Target::load(&root, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            provenance::check(&engine, &target)
        }
        "boot" => {
            let opts = Options::parse(&args[1..])?;
            let Some(target_name) = opts.positional.first() else {
                bail!("boot needs a target name\n\n{USAGE}")
            };
            let target = target::Target::load(&root, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            boot::run(
                &engine,
                &target,
                &boot::Options {
                    smoke: opts.smoke,
                    timeout: std::time::Duration::from_secs(opts.timeout),
                    memory_mb: opts.memory,
                },
            )
        }
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

struct Options {
    positional: Vec<String>,
    target: Option<String>,
    jobs: usize,
    smoke: bool,
    timeout: u64,
    memory: u32,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options> {
        let mut opts = Options {
            positional: Vec::new(),
            target: None,
            jobs: cores(),
            smoke: false,
            timeout: 300,
            memory: 4096,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--target" => {
                    opts.target = Some(
                        it.next()
                            .ok_or_else(|| Error::new("--target needs a value"))?
                            .clone(),
                    );
                }
                "--jobs" => {
                    opts.jobs = number(it.next(), "--jobs")?;
                    if opts.jobs == 0 {
                        bail!("--jobs must be at least 1");
                    }
                }
                "--timeout" => opts.timeout = number(it.next(), "--timeout")?,
                "--memory" => opts.memory = number(it.next(), "--memory")?,
                "--smoke" => opts.smoke = true,
                other if other.starts_with('-') => bail!("unknown option `{other}`"),
                other => opts.positional.push(other.to_string()),
            }
        }
        Ok(opts)
    }
}

fn number<T: std::str::FromStr>(value: Option<&String>, flag: &str) -> Result<T> {
    let v = value.ok_or_else(|| Error::new(format!("{flag} needs a value")))?;
    v.parse()
        .map_err(|_| Error::new(format!("{flag} `{v}` is not a number")))
}

/// Attempt one's bottleneck was one machine at load 18, so the default is
/// every core and the knob is visible.
fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// The repo is wherever `recipes/` and `targets/` sit together, so kb works
/// from any subdirectory.
fn repo_root() -> Result<PathBuf> {
    let mut dir: &Path = &std::env::current_dir()?;
    loop {
        if dir.join("recipes").is_dir() && dir.join("targets").is_dir() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => bail!("no recipes/ and targets/ in this directory or any above it"),
        }
    }
}

#[cfg(test)]
mod crate_layout {
    /// A file in `src/` that nobody declares is a file that never compiles,
    /// and its tests never run. That happened twice while writing this crate
    /// and both times `cargo test` stayed green, so it gets a test.
    #[test]
    fn every_source_file_is_declared() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let root = std::fs::read_to_string(dir.join("main.rs")).unwrap();
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            if stem == "main" {
                continue;
            }
            if !root.contains(&format!("mod {stem};")) {
                missing.push(stem);
            }
        }
        assert!(missing.is_empty(), "src/main.rs is missing: mod {missing:?};");
    }
}
