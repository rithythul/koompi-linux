//! Typed reads out of a parsed table, with unknown keys rejected.
//!
//! Rejecting unknown keys is not pedantry: a misspelled key that is silently
//! ignored is a recipe that builds the wrong thing and says nothing. The
//! post-mortem's `#[derive(Default)]` trap is the same failure one layer up,
//! which is why nothing here has a `Default`.

use crate::err::{Error, Result, bail};
use crate::toml::{Table, Value};
use std::collections::BTreeSet;

pub struct Reader<'a> {
    /// Where this table came from, for error messages: `recipes/gcc.toml [build]`.
    origin: String,
    table: &'a Table,
    used: BTreeSet<String>,
}

impl<'a> Reader<'a> {
    pub fn new(origin: impl Into<String>, table: &'a Table) -> Self {
        Reader {
            origin: origin.into(),
            table,
            used: BTreeSet::new(),
        }
    }

    fn take(&mut self, key: &str) -> Option<&'a Value> {
        self.used.insert(key.to_string());
        self.table.get(key)
    }

    fn wrong(&self, key: &str, got: &Value, want: &str) -> Error {
        Error::new(format!(
            "{}: `{key}` is a {} but should be {want}",
            self.origin,
            got.type_name()
        ))
    }

    pub fn str_req(&mut self, key: &str) -> Result<&'a str> {
        match self.take(key) {
            Some(Value::Str(s)) => Ok(s),
            Some(other) => Err(self.wrong(key, other, "a string")),
            None => bail!("{}: `{key}` is required", self.origin),
        }
    }

    pub fn str_opt(&mut self, key: &str) -> Result<Option<&'a str>> {
        match self.take(key) {
            Some(Value::Str(s)) => Ok(Some(s)),
            Some(other) => Err(self.wrong(key, other, "a string")),
            None => Ok(None),
        }
    }

    pub fn int_opt(&mut self, key: &str) -> Result<Option<i64>> {
        match self.take(key) {
            Some(Value::Int(n)) => Ok(Some(*n)),
            Some(other) => Err(self.wrong(key, other, "an integer")),
            None => Ok(None),
        }
    }

    pub fn bool_or(&mut self, key: &str, fallback: bool) -> Result<bool> {
        match self.take(key) {
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(self.wrong(key, other, "a boolean")),
            None => Ok(fallback),
        }
    }

    /// An array of strings, defaulting to empty. Non-string elements are an error.
    pub fn strs(&mut self, key: &str) -> Result<Vec<String>> {
        match self.take(key) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| match v {
                    Value::Str(s) => Ok(s.clone()),
                    other => bail!(
                        "{}: `{key}` contains a {}; every element must be a string",
                        self.origin,
                        other.type_name()
                    ),
                })
                .collect(),
            Some(other) => Err(self.wrong(key, other, "an array of strings")),
            None => Ok(Vec::new()),
        }
    }

    /// An array of strings, distinguishing "absent" from "explicitly empty",
    /// which is what lets a field default to something other than empty.
    pub fn strs_opt(&mut self, key: &str) -> Result<Option<Vec<String>>> {
        if !self.table.contains_key(key) {
            self.used.insert(key.to_string());
            return Ok(None);
        }
        self.strs(key).map(Some)
    }

    /// A sub-table, if present. The caller must `finish()` it too.
    pub fn table_opt(&mut self, key: &str) -> Result<Option<Reader<'a>>> {
        let origin = format!("{} [{key}]", self.origin);
        match self.take(key) {
            Some(Value::Table(t)) => Ok(Some(Reader::new(origin, t))),
            Some(other) => Err(self.wrong(key, other, "a table")),
            None => Ok(None),
        }
    }

    /// Every remaining key as a string pair, for open-ended maps like `[build.env]`.
    pub fn pairs(&mut self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for (k, v) in self.table {
            self.used.insert(k.clone());
            match v {
                Value::Str(s) => out.push((k.clone(), s.clone())),
                other => bail!(
                    "{}: `{k}` is a {}; every value here must be a string",
                    self.origin,
                    other.type_name()
                ),
            }
        }
        Ok(out)
    }

    /// Consume the reader, failing if the table had keys nobody asked for.
    pub fn finish(self) -> Result<()> {
        let extra: Vec<&str> = self
            .table
            .keys()
            .filter(|k| !self.used.contains(*k))
            .map(String::as_str)
            .collect();
        if extra.is_empty() {
            return Ok(());
        }
        bail!("{}: unknown key(s): {}", self.origin, extra.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toml::parse;

    #[test]
    fn reads_and_defaults() {
        let t = parse("name = \"z\"\nn = 3\n[deps]\nbuild = [\"a\"]\n").unwrap();
        let mut r = Reader::new("t.toml", &t);
        assert_eq!(r.str_req("name").unwrap(), "z");
        assert_eq!(r.int_opt("n").unwrap(), Some(3));
        assert_eq!(r.str_opt("missing").unwrap(), None);
        let mut d = r.table_opt("deps").unwrap().unwrap();
        assert_eq!(d.strs("build").unwrap(), vec!["a".to_string()]);
        assert_eq!(d.strs("runtime").unwrap(), Vec::<String>::new());
        d.finish().unwrap();
        r.finish().unwrap();
    }

    #[test]
    fn unknown_keys_are_an_error() {
        let t = parse("name = \"z\"\nverison = \"1\"\n").unwrap();
        let mut r = Reader::new("t.toml", &t);
        r.str_req("name").unwrap();
        let e = r.finish().unwrap_err().to_string();
        assert!(e.contains("unknown key(s): verison"), "{e}");
    }

    #[test]
    fn missing_required_names_the_file() {
        let t = parse("").unwrap();
        let e = Reader::new("recipes/z.toml", &t)
            .str_req("name")
            .unwrap_err()
            .to_string();
        assert_eq!(e, "recipes/z.toml: `name` is required");
    }

    #[test]
    fn wrong_type_names_both_types() {
        let t = parse("name = 1\n").unwrap();
        let e = Reader::new("t.toml", &t)
            .str_req("name")
            .unwrap_err()
            .to_string();
        assert!(e.contains("is a integer but should be a string"), "{e}");
    }

    #[test]
    fn array_with_a_non_string_is_an_error() {
        let t = parse("x = [\"a\", 2]\n").unwrap();
        let e = Reader::new("t.toml", &t).strs("x").unwrap_err().to_string();
        assert!(e.contains("contains a integer"), "{e}");
    }
}
