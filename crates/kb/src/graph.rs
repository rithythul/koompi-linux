//! Dependency ordering.
//!
//! Build edges and runtime edges are kept apart. Attempt one's planner broke a
//! cycle by cutting a runtime edge on the assumption the chroot already had
//! it; over a minimal seed it did not, and the build failed a long way from
//! the cause.
//!
//! This engine cuts nothing at all. A cycle is reported with every edge in it
//! labelled, and the build stops. That is stricter than the rule the plan
//! states, and when a cycle that genuinely needs breaking turns up, the place
//! to relax it is here, in the open.

use crate::err::{Result, bail};
use crate::recipe::Recipe;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Build,
    Runtime,
}

impl Edge {
    fn label(self) -> &'static str {
        match self {
            Edge::Build => "build",
            Edge::Runtime => "runtime",
        }
    }
}

/// Every recipe `root` needs, in an order where each one comes after everything
/// it depends on, with `root` last.
pub fn order(recipes: &BTreeMap<String, Recipe>, root: &str) -> Result<Vec<String>> {
    if !recipes.contains_key(root) {
        bail!("no recipe named `{root}`");
    }
    let mut out = Vec::new();
    let mut done = BTreeSet::new();
    let mut stack: Vec<(String, Edge)> = Vec::new();
    visit(recipes, root, Edge::Build, &mut stack, &mut done, &mut out)?;
    Ok(out)
}

fn visit(
    recipes: &BTreeMap<String, Recipe>,
    name: &str,
    via: Edge,
    stack: &mut Vec<(String, Edge)>,
    done: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    if done.contains(name) {
        return Ok(());
    }
    if let Some(at) = stack.iter().position(|(n, _)| n == name) {
        // The edge *leaving* frame i is how frame i+1 was entered, and the
        // edge leaving the last frame is how we got back here.
        let mut path = String::new();
        for i in at..stack.len() {
            let leaving = stack.get(i + 1).map(|(_, e)| *e).unwrap_or(via);
            path.push_str(&format!("{} -({})-> ", stack[i].0, leaving.label()));
        }
        path.push_str(name);
        bail!("dependency cycle: {path}\nno edge is cut automatically; break it in the recipes");
    }

    let recipe = match recipes.get(name) {
        Some(r) => r,
        None => {
            let referrer = stack
                .last()
                .map(|(n, _)| n.as_str())
                .unwrap_or("the command line");
            bail!("`{referrer}` depends on `{name}`, which has no recipe");
        }
    };

    stack.push((name.to_string(), via));
    for dep in &recipe.deps.build {
        visit(recipes, dep, Edge::Build, stack, done, out)?;
    }
    for dep in &recipe.deps.runtime {
        visit(recipes, dep, Edge::Runtime, stack, done, out)?;
    }
    stack.pop();

    done.insert(name.to_string());
    out.push(name.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn recipe(name: &str, build: &[&str], runtime: &[&str]) -> Recipe {
        use crate::recipe::*;
        Recipe {
            name: name.into(),
            version: "0".into(),
            kind: Kind::Target,
            source: Some(Source {
                url: "u".into(),
                sha256: "0".repeat(64),
                strip: 1,
            }),
            build: Build {
                system: System::Make,
                out_of_tree: false,
                configure: vec![],
                make: None,
                install: None,
                env: vec![],
                script: None,
            },
            deps: Deps {
                build: build.iter().map(|s| s.to_string()).collect(),
                runtime: runtime.iter().map(|s| s.to_string()).collect(),
            },
            path: PathBuf::from(format!("recipes/{name}.toml")),
            raw: Vec::new(),
        }
    }

    fn set(rs: Vec<Recipe>) -> BTreeMap<String, Recipe> {
        rs.into_iter().map(|r| (r.name.clone(), r)).collect()
    }

    #[test]
    fn deps_come_first_and_root_is_last() {
        let rs = set(vec![
            recipe("gcc", &["binutils"], &["glibc"]),
            recipe("binutils", &[], &[]),
            recipe("glibc", &["binutils"], &[]),
        ]);
        let o = order(&rs, "gcc").unwrap();
        assert_eq!(o, vec!["binutils", "glibc", "gcc"]);
    }

    #[test]
    fn each_recipe_appears_once() {
        let rs = set(vec![
            recipe("top", &["a", "b"], &[]),
            recipe("a", &["shared"], &[]),
            recipe("b", &["shared"], &[]),
            recipe("shared", &[], &[]),
        ]);
        let o = order(&rs, "top").unwrap();
        assert_eq!(o, vec!["shared", "a", "b", "top"]);
    }

    #[test]
    fn a_cycle_names_every_edge_in_it() {
        let rs = set(vec![
            recipe("a", &["b"], &[]),
            recipe("b", &[], &["a"]),
        ]);
        let e = order(&rs, "a").unwrap_err().to_string();
        assert!(e.contains("a -(build)-> b -(runtime)-> a"), "{e}");
        assert!(e.contains("no edge is cut automatically"), "{e}");
    }

    #[test]
    fn a_missing_dep_names_who_wanted_it() {
        let rs = set(vec![recipe("a", &["ghost"], &[])]);
        let e = order(&rs, "a").unwrap_err().to_string();
        assert_eq!(e, "`a` depends on `ghost`, which has no recipe");
    }
}
