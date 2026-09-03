//! The gate behind Gate 0 criterion 3.
//!
//! A recipe may not contain an architecture. If it does, the two targets are
//! not coming from one recipe set, they are coming from one recipe set with
//! the fork hidden inside an `if`. The check is crude on purpose: every token
//! in every recipe is compared against every architecture literal declared by
//! every target, and a match fails.
//!
//! Tokens are split on characters that cannot appear in a triple, so `x86` and
//! `x86_64` are different tokens and a target whose `kernel_arch` is `x86`
//! does not condemn every recipe that mentions `x86_64`.

use crate::err::{Result, bail};
use crate::recipe::Recipe;
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

/// Every `file:line: token` where a recipe names an architecture.
pub fn architecture_literals(recipe: &Recipe, targets: &[Target]) -> Vec<String> {
    let reserved: Vec<&str> = targets.iter().flat_map(Target::reserved_tokens).collect();
    let text = String::from_utf8_lossy(&recipe.raw);
    let mut hits = Vec::new();

    for (n, line) in text.lines().enumerate() {
        // A comment may legitimately discuss an architecture; the rule is
        // about what the build does, not about what the recipe explains.
        let code = line.split('#').next().unwrap_or("");
        for token in code.split(|c: char| !is_token_char(c)) {
            if !token.is_empty() && reserved.contains(&token) {
                hits.push(format!(
                    "{}:{}: `{token}` — put it in the target file and use $TRIPLE, $ARCH or $KARCH",
                    recipe.path.display(),
                    n + 1
                ));
            }
        }
    }
    hits
}

pub fn run(root: &Path, recipes: &BTreeMap<String, Recipe>, targets: &[Target]) -> Result<()> {
    let mut problems = Vec::new();

    // A fragment no target lists is a decision nobody is making.
    let listed: BTreeSet<&str> = targets.iter().flat_map(|t| t.kernel_fragments.iter().map(String::as_str)).collect();
    let dir = root.join("config/kernel");
    let mut on_disk: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| crate::err::Error::new(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "config"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    on_disk.sort();
    for name in &on_disk {
        if !listed.contains(name.as_str()) {
            problems.push(format!("config/kernel/{name}.config: no target lists it in kernel_config"));
        }
    }

    for recipe in recipes.values() {
        problems.extend(architecture_literals(recipe, targets));

        for dep in recipe.all_deps() {
            if !recipes.contains_key(dep) {
                problems.push(format!(
                    "{}: depends on `{dep}`, which has no recipe",
                    recipe.path.display()
                ));
            }
            if dep == recipe.name {
                problems.push(format!("{}: depends on itself", recipe.path.display()));
            }
        }
    }

    // Every target must be orderable from every recipe, or "one recipe set"
    // is only true for the target that happens to have been tried.
    for recipe in recipes.keys() {
        if let Err(e) = crate::graph::order(recipes, recipe) {
            problems.push(e.to_string());
        }
    }

    for target in targets {
        for name in &target.contents {
            match recipes.get(name) {
                None => problems.push(format!(
                    "targets/{}.toml: contents names `{name}`, which has no recipe",
                    target.name
                )),
                Some(r) if r.kind != crate::recipe::Kind::Target => problems.push(format!(
                    "targets/{}.toml: contents names `{name}`, a host tool, which never enters an image",
                    target.name
                )),
                Some(_) => {}
            }
        }
    }

    if problems.is_empty() {
        println!(
            "lint: {} recipes, {} targets, clean",
            recipes.len(),
            targets.len()
        );
        return Ok(());
    }
    for p in &problems {
        eprintln!("  {p}");
    }
    bail!("{} problem(s)", problems.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target(name: &str, triple: &str, arch: &str, karch: &str) -> Target {
        Target::for_test(name, triple, arch, karch)
    }

    fn recipe_with(body: &str) -> Recipe {
        use crate::recipe::*;
        Recipe {
            name: "r".into(),
            version: "1".into(),
            kind: Kind::Target,
            source: Some(Source { url: "u".into(), sha256: "0".repeat(64), license: "MIT".into(), strip: 1 }),
            build: Build {
                system: System::Make,
                out_of_tree: false,
                configure: vec![],
                make: None,
                install: None,
                env: vec![],
                script: None,
                remove: vec![],
            },
            deps: Deps { build: vec![], runtime: vec![] },
            checks: vec![],
            path: PathBuf::from("recipes/r.toml"),
            raw: body.as_bytes().to_vec(),
        }
    }

    fn targets() -> Vec<Target> {
        vec![
            target("cloud", "x86_64-koompi-linux-gnu", "x86_64", "x86"),
            target("headless", "aarch64-koompi-linux-gnu", "aarch64", "arm64"),
        ]
    }

    #[test]
    fn a_triple_in_a_recipe_is_caught() {
        let hits = architecture_literals(
            &recipe_with("configure = [\"--host=x86_64-koompi-linux-gnu\"]\n"),
            &targets(),
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("recipes/r.toml:1"), "{}", hits[0]);
    }

    #[test]
    fn an_arch_in_a_recipe_is_caught() {
        let hits = architecture_literals(&recipe_with("env = 1\nARCH = \"arm64\"\n"), &targets());
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains(":2:"), "{}", hits[0]);
    }

    #[test]
    fn the_variables_are_fine() {
        let hits = architecture_literals(
            &recipe_with("configure = [\"--host=$TRIPLE\", \"--with-arch=$ARCH\"]\n"),
            &targets(),
        );
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn a_short_arch_does_not_match_inside_a_longer_token() {
        // kernel_arch "x86" must not condemn a recipe mentioning "x86_64_defconfig".
        let hits = architecture_literals(&recipe_with("make = [\"x86_64_defconfig\"]\n"), &targets());
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn comments_may_discuss_architectures() {
        let hits = architecture_literals(
            &recipe_with("# on aarch64 this needs the sysroot flag\nmake = []\n"),
            &targets(),
        );
        assert!(hits.is_empty(), "{hits:?}");
    }
}
