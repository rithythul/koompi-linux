//! kb — the KOOMPI Linux build engine.
//!
//! One binary, because the constraint the whole spec serves is that a person
//! can read the core in a day, and that includes the thing that builds it.

mod build;
mod err;
mod graph;
mod lint;
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

options
  --target <name>   required by build
  --jobs <n>        parallel make jobs (default: all cores)
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
    let targets_dir = root.join("targets");

    match command.as_str() {
        "lint" => {
            let recipes = recipe::load_all(&recipes_dir)?;
            let targets = target::load_all(&targets_dir)?;
            lint::run(&recipes, &targets)
        }
        "targets" => {
            for t in target::load_all(&targets_dir)? {
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
            let targets = target::load_all(&targets_dir)?;
            // Lint before building: a recipe that names an architecture would
            // build fine and quietly break the claim the gate is testing.
            lint::run(&recipes, &targets)?;

            let target = target::Target::load(&targets_dir, target_name)?;
            let engine = build::Engine::new(&root, opts.jobs)?;
            let id = engine.build(&recipes, name, &target)?;
            println!("{}", engine.store.path(&id).display());
            Ok(())
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
}

impl Options {
    fn parse(args: &[String]) -> Result<Options> {
        let mut opts = Options {
            positional: Vec::new(),
            target: None,
            jobs: cores(),
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
                    let v = it.next().ok_or_else(|| Error::new("--jobs needs a value"))?;
                    opts.jobs = v
                        .parse()
                        .map_err(|_| Error::new(format!("--jobs `{v}` is not a number")))?;
                    if opts.jobs == 0 {
                        bail!("--jobs must be at least 1");
                    }
                }
                other if other.starts_with('-') => bail!("unknown option `{other}`"),
                other => opts.positional.push(other.to_string()),
            }
        }
        Ok(opts)
    }
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
