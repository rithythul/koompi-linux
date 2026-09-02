//! A strict TOML subset.
//!
//! This parser is deliberately narrow. It accepts tables, dotted table
//! headers, bare keys, basic strings, integers, booleans and arrays, and it
//! rejects everything else by name: inline tables, arrays of tables, literal
//! strings, multi-line strings, floats and dates.
//!
//! The narrowness is the point. spec.md's constraint is that a person can read
//! the whole core in a day, and a recipe written in a feature of TOML nobody
//! else used is a recipe that has to be read twice. The parser is how the
//! readable subset stays readable, so a rejection message says what is not in
//! the subset rather than "invalid syntax".

use crate::err::{Error, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type Table = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Int(_) => "integer",
            Value::Bool(_) => "boolean",
            Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }
}

pub fn parse(input: &str) -> Result<Table> {
    let mut p = Parser {
        s: input.as_bytes(),
        i: 0,
        line: 1,
    };
    p.document()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl<'a> Parser<'a> {
    fn fail<T>(&self, msg: impl fmt::Display) -> Result<T> {
        Err(Error::new(format!("line {}: {msg}", self.line)))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Spaces and tabs only: never crosses a line.
    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
            self.i += 1;
        }
    }

    fn comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }

    /// Whitespace, newlines and comments. Legal between array elements and
    /// between top-level items, never inside a value.
    fn trivia(&mut self) {
        loop {
            self.spaces();
            match self.peek() {
                Some(b'#') => self.comment(),
                Some(b'\n') => {
                    self.bump();
                }
                _ => return,
            }
        }
    }

    /// Everything after a value on its line must be blank or a comment.
    fn end_of_line(&mut self) -> Result<()> {
        self.spaces();
        self.comment();
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => self.fail(format!(
                "unexpected {:?} after a value; one key per line",
                c as char
            )),
        }
    }

    fn document(&mut self) -> Result<Table> {
        let mut root = Table::new();
        // Table headers seen, so `[a]` twice is an error rather than a merge.
        let mut declared: BTreeSet<Vec<String>> = BTreeSet::new();
        let mut path: Vec<String> = Vec::new();

        loop {
            self.trivia();
            match self.peek() {
                None => return Ok(root),
                Some(b'[') => {
                    self.bump();
                    if self.peek() == Some(b'[') {
                        return self
                            .fail("arrays of tables ([[x]]) are not in the subset; use an array of strings, or separate files");
                    }
                    path = self.header()?;
                    if !declared.insert(path.clone()) {
                        return self.fail(format!("table [{}] is declared twice", path.join(".")));
                    }
                    // Create it, so an empty table still exists.
                    self.descend(&mut root, &path)?;
                    self.end_of_line()?;
                }
                _ => {
                    let key = self.bare_key()?;
                    self.spaces();
                    if !self.eat(b'=') {
                        return self.fail(format!("expected `=` after key `{key}`"));
                    }
                    self.spaces();
                    let value = self.value()?;
                    let line = self.line;
                    let tbl = self.descend(&mut root, &path)?;
                    if tbl.insert(key.clone(), value).is_some() {
                        return Err(Error::new(format!(
                            "line {line}: key `{key}` is set twice in the same table"
                        )));
                    }
                    self.end_of_line()?;
                }
            }
        }
    }

    /// Walk to the table at `path`, creating tables as needed.
    fn descend<'t>(&self, root: &'t mut Table, path: &[String]) -> Result<&'t mut Table> {
        let mut cur = root;
        for (n, part) in path.iter().enumerate() {
            let entry = cur
                .entry(part.clone())
                .or_insert_with(|| Value::Table(Table::new()));
            match entry {
                Value::Table(t) => cur = t,
                other => {
                    bail!(
                        "`{}` is a {} but [{}] needs it to be a table",
                        path[..=n].join("."),
                        other.type_name(),
                        path.join(".")
                    )
                }
            }
        }
        Ok(cur)
    }

    fn header(&mut self) -> Result<Vec<String>> {
        let mut parts = Vec::new();
        loop {
            self.spaces();
            parts.push(self.bare_key()?);
            self.spaces();
            if self.eat(b']') {
                return Ok(parts);
            }
            if !self.eat(b'.') {
                return self.fail("expected `.` or `]` in a table header");
            }
        }
    }

    fn bare_key(&mut self) -> Result<String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
            self.i += 1;
        }
        if start == self.i {
            return match self.peek() {
                Some(b'"') => self.fail("quoted keys are not in the subset; use a bare key"),
                Some(c) => self.fail(format!("expected a key, found {:?}", c as char)),
                None => self.fail("expected a key, found end of file"),
            };
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn value(&mut self) -> Result<Value> {
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.basic_string()?)),
            Some(b'\'') => self.fail("literal strings ('...') are not in the subset; use \"...\""),
            Some(b'{') => self.fail("inline tables ({...}) are not in the subset; use a [table]"),
            Some(b'[') => self.array(),
            Some(b't' | b'f') => self.boolean(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.integer(),
            Some(c) => self.fail(format!("expected a value, found {:?}", c as char)),
            None => self.fail("expected a value, found end of file"),
        }
    }

    fn basic_string(&mut self) -> Result<String> {
        self.bump(); // opening quote
        if self.peek() == Some(b'"') && self.s.get(self.i + 1) == Some(&b'"') {
            return self.fail("multi-line strings are not in the subset");
        }
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some(b'\n') => return self.fail("unterminated string"),
                Some(b'"') => return Ok(out),
                Some(b'\\') => match self.bump() {
                    Some(b'n') => out.push('\n'),
                    Some(b't') => out.push('\t'),
                    Some(b'r') => out.push('\r'),
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(c) => {
                        return self.fail(format!(
                            "escape \\{} is not in the subset (only \\n \\t \\r \\\" \\\\)",
                            c as char
                        ));
                    }
                    None => return self.fail("unterminated string"),
                },
                Some(c) => {
                    // Re-decode UTF-8: bytes above ASCII arrive one at a time.
                    if c < 0x80 {
                        out.push(c as char);
                    } else {
                        let start = self.i - 1;
                        let len = utf8_len(c);
                        if start + len > self.s.len() {
                            return self.fail("truncated UTF-8 in string");
                        }
                        match std::str::from_utf8(&self.s[start..start + len]) {
                            Ok(s) => {
                                out.push_str(s);
                                self.i = start + len;
                            }
                            Err(_) => return self.fail("invalid UTF-8 in string"),
                        }
                    }
                }
            }
        }
    }

    fn boolean(&mut self) -> Result<Value> {
        for (word, val) in [("true", true), ("false", false)] {
            if self.s[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                return Ok(Value::Bool(val));
            }
        }
        self.fail("expected a value")
    }

    fn integer(&mut self) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return self.fail("floats are not in the subset");
        }
        let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        match text.parse::<i64>() {
            Ok(n) => Ok(Value::Int(n)),
            Err(_) => self.fail(format!("`{text}` is not an integer")),
        }
    }

    fn array(&mut self) -> Result<Value> {
        self.bump(); // '['
        let mut items = Vec::new();
        loop {
            self.trivia();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            if self.peek().is_none() {
                return self.fail("unterminated array");
            }
            items.push(self.value()?);
            self.trivia();
            if self.eat(b',') {
                continue;
            }
            self.trivia();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            if self.peek().is_none() {
                return self.fail("unterminated array");
            }
            return self.fail("expected `,` or `]` in an array");
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        parse(s).expect("should parse")
    }

    fn rejects(s: &str, needle: &str) {
        let e = parse(s).expect_err("should be rejected").to_string();
        assert!(e.contains(needle), "error was {e:?}, wanted {needle:?}");
    }

    #[test]
    fn scalars_and_tables() {
        let d = t(r#"
name = "zlib"
answer = 42
neg = -7
yes = true

[source]
url = "https://example.invalid/z.tar.gz"

[deps]
build = ["gcc", "binutils"]
runtime = []
"#);
        assert_eq!(d["name"], Value::Str("zlib".into()));
        assert_eq!(d["answer"], Value::Int(42));
        assert_eq!(d["neg"], Value::Int(-7));
        assert_eq!(d["yes"], Value::Bool(true));
        let Value::Table(deps) = &d["deps"] else { panic!() };
        assert_eq!(
            deps["build"],
            Value::Array(vec![Value::Str("gcc".into()), Value::Str("binutils".into())])
        );
        assert_eq!(deps["runtime"], Value::Array(vec![]));
    }

    #[test]
    fn dotted_headers_nest() {
        let d = t("[a.b]\nk = 1\n");
        let Value::Table(a) = &d["a"] else { panic!() };
        let Value::Table(b) = &a["b"] else { panic!() };
        assert_eq!(b["k"], Value::Int(1));
    }

    #[test]
    fn arrays_span_lines_and_allow_trailing_commas() {
        let d = t("x = [\n  \"a\", # why\n  \"b\",\n]\n");
        assert_eq!(
            d["x"],
            Value::Array(vec![Value::Str("a".into()), Value::Str("b".into())])
        );
    }

    #[test]
    fn comments_and_blank_lines() {
        let d = t("# lead\n\n  # indented\nk = 1 # trailing\n");
        assert_eq!(d["k"], Value::Int(1));
    }

    #[test]
    fn escapes_and_utf8() {
        let d = t(r#"a = "x\ny\t\"z\\"
b = "ស្រុកខ្មែរ"
"#);
        assert_eq!(d["a"], Value::Str("x\ny\t\"z\\".into()));
        assert_eq!(d["b"], Value::Str("ស្រុកខ្មែរ".into()));
    }

    // The rejections are the feature, so each one is tested by its message.
    #[test]
    fn rejects_what_is_not_in_the_subset() {
        rejects("x = {a = 1}\n", "inline tables");
        rejects("[[x]]\n", "arrays of tables");
        rejects("x = 'a'\n", "literal strings");
        rejects("x = \"\"\"a\"\"\"\n", "multi-line strings");
        rejects("x = 1.5\n", "floats");
        rejects("\"x\" = 1\n", "quoted keys");
        rejects(r#"x = "\a""#, "escape \\a is not in the subset");
    }

    #[test]
    fn rejects_duplicates() {
        rejects("x = 1\nx = 2\n", "set twice");
        rejects("[a]\n[a]\n", "declared twice");
    }

    #[test]
    fn rejects_malformed() {
        rejects("x 1\n", "expected `=`");
        rejects("x = \n", "expected a value");
        rejects("x = \"unterminated\n", "unterminated string");
        rejects("x = [1, 2\n", "unterminated array");
        rejects("x = 1 y = 2\n", "one key per line");
        rejects("[a\n", "expected `.` or `]`");
    }

    #[test]
    fn reports_the_line() {
        let e = parse("a = 1\n\nb = {}\n").unwrap_err().to_string();
        assert!(e.starts_with("line 3:"), "error was {e:?}");
    }
}
