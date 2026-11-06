//! A small reader of the SpiceDB schema language, for the in-memory engine
//! ([`super::memory::MemoryAuthz`]) and for checks of relationships before they are written.
//!
//! It reads what `contracts/authz/schema.zed` uses: `definition`, `relation` with subject types
//! (`user`, `group#member`, `user:*`), and `permission` with `+` (union), `&` (intersection),
//! `-` (exclusion), `->` (arrow) and parentheses. Comments are `//` and `/* */`.
//!
//! The operators are read from left to right with the same precedence. The contract schema puts
//! parentheses around every mixed expression, so this gives the same result as SpiceDB.

use crate::error::{Error, Result};
use std::collections::HashMap;

/// The contract schema, compiled into the binary.
pub const CONTRACT_SCHEMA: &str = include_str!("../../../../contracts/authz/schema.zed");

/// A subject type that a relation allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllowedSubject {
    /// `user`: a plain object of that type.
    Object(String),
    /// `group#member`: a subject set.
    Set(String, String),
    /// `user:*`: the wildcard of that type.
    Wildcard(String),
}

/// A permission expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// A relation or permission of the same definition.
    Ref(String),
    /// `relation->permission`.
    Arrow(String, String),
    Union(Box<Expr>, Box<Expr>),
    Intersect(Box<Expr>, Box<Expr>),
    Exclude(Box<Expr>, Box<Expr>),
}

/// One `definition`.
#[derive(Clone, Debug, Default)]
pub struct Definition {
    pub relations: HashMap<String, Vec<AllowedSubject>>,
    pub permissions: HashMap<String, Expr>,
}

/// A parsed schema.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub definitions: HashMap<String, Definition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Sym(&'static str),
}

fn tokenize(src: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if src[i..].starts_with("/*") {
            match src[i + 2..].find("*/") {
                Some(end) => i += end + 4,
                None => return Err(bad("unclosed comment")),
            }
        } else if src[i..].starts_with("->") {
            out.push(Tok::Sym("->"));
            i += 2;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'/' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'/') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_string()));
        } else {
            let s = match c {
                b'{' => "{",
                b'}' => "}",
                b'(' => "(",
                b')' => ")",
                b':' => ":",
                b'|' => "|",
                b'+' => "+",
                b'-' => "-",
                b'&' => "&",
                b'#' => "#",
                b'*' => "*",
                b'=' => "=",
                _ => return Err(bad(&format!("unexpected character {:?}", c as char))),
            };
            out.push(Tok::Sym(s));
            i += 1;
        }
    }
    Ok(out)
}

fn bad(msg: &str) -> Error {
    Error::Internal(format!("authz schema: {msg}"))
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn ident(&mut self) -> Result<String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            t => Err(bad(&format!("expected a name, got {t:?}"))),
        }
    }
    fn sym(&mut self, s: &'static str) -> Result<()> {
        match self.next() {
            Some(Tok::Sym(x)) if x == s => Ok(()),
            t => Err(bad(&format!("expected {s}, got {t:?}"))),
        }
    }
    fn is_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(x)) if *x == s)
    }

    fn schema(&mut self) -> Result<Schema> {
        let mut schema = Schema::default();
        while self.peek().is_some() {
            let kw = self.ident()?;
            if kw != "definition" {
                return Err(bad(&format!("expected definition, got {kw}")));
            }
            let name = self.ident()?;
            self.sym("{")?;
            let mut def = Definition::default();
            while !self.is_sym("}") {
                match self.ident()?.as_str() {
                    "relation" => {
                        let rel = self.ident()?;
                        self.sym(":")?;
                        let mut allowed = vec![self.allowed()?];
                        while self.is_sym("|") {
                            self.next();
                            allowed.push(self.allowed()?);
                        }
                        def.relations.insert(rel, allowed);
                    }
                    "permission" => {
                        let perm = self.ident()?;
                        self.sym("=")?;
                        let e = self.expr()?;
                        def.permissions.insert(perm, e);
                    }
                    other => return Err(bad(&format!("unexpected {other}"))),
                }
            }
            self.sym("}")?;
            schema.definitions.insert(name, def);
        }
        Ok(schema)
    }

    fn allowed(&mut self) -> Result<AllowedSubject> {
        let t = self.ident()?;
        if self.is_sym("#") {
            self.next();
            Ok(AllowedSubject::Set(t, self.ident()?))
        } else if self.is_sym(":") {
            self.next();
            self.sym("*")?;
            Ok(AllowedSubject::Wildcard(t))
        } else {
            Ok(AllowedSubject::Object(t))
        }
    }

    fn expr(&mut self) -> Result<Expr> {
        let mut left = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Sym(s @ ("+" | "&" | "-"))) => *s,
                _ => return Ok(left),
            };
            self.next();
            let right = self.term()?;
            left = match op {
                "+" => Expr::Union(Box::new(left), Box::new(right)),
                "&" => Expr::Intersect(Box::new(left), Box::new(right)),
                _ => Expr::Exclude(Box::new(left), Box::new(right)),
            };
        }
    }

    fn term(&mut self) -> Result<Expr> {
        if self.is_sym("(") {
            self.next();
            let e = self.expr()?;
            self.sym(")")?;
            return Ok(e);
        }
        let name = self.ident()?;
        if self.is_sym("->") {
            self.next();
            let perm = self.ident()?;
            Ok(Expr::Arrow(name, perm))
        } else {
            Ok(Expr::Ref(name))
        }
    }
}

impl Schema {
    /// Parses a schema in the SpiceDB schema language.
    pub fn parse(src: &str) -> Result<Self> {
        let mut p = Parser {
            toks: tokenize(src)?,
            pos: 0,
        };
        let s = p.schema()?;
        s.validate()?;
        Ok(s)
    }

    /// The contract schema (`contracts/authz/schema.zed`).
    pub fn contract() -> Self {
        Self::parse(CONTRACT_SCHEMA).expect("contracts/authz/schema.zed parses")
    }

    fn validate(&self) -> Result<()> {
        for (dname, def) in &self.definitions {
            for (rel, allowed) in &def.relations {
                for a in allowed {
                    let t = match a {
                        AllowedSubject::Object(t)
                        | AllowedSubject::Wildcard(t)
                        | AllowedSubject::Set(t, _) => t,
                    };
                    if !self.definitions.contains_key(t) {
                        return Err(bad(&format!("{dname}#{rel}: unknown type {t}")));
                    }
                }
            }
            for (perm, e) in &def.permissions {
                self.validate_expr(dname, def, perm, e)?;
            }
        }
        Ok(())
    }

    fn validate_expr(&self, dname: &str, def: &Definition, perm: &str, e: &Expr) -> Result<()> {
        match e {
            Expr::Ref(n) => {
                if !def.relations.contains_key(n) && !def.permissions.contains_key(n) {
                    return Err(bad(&format!("{dname}#{perm}: unknown name {n}")));
                }
            }
            Expr::Arrow(r, _) => {
                if !def.relations.contains_key(r) {
                    return Err(bad(&format!("{dname}#{perm}: unknown relation {r}")));
                }
            }
            Expr::Union(a, b) | Expr::Intersect(a, b) | Expr::Exclude(a, b) => {
                self.validate_expr(dname, def, perm, a)?;
                self.validate_expr(dname, def, perm, b)?;
            }
        }
        Ok(())
    }

    /// Whether `object_type#name` is a relation or a permission.
    pub fn has_name(&self, object_type: &str, name: &str) -> bool {
        self.definitions
            .get(object_type)
            .is_some_and(|d| d.relations.contains_key(name) || d.permissions.contains_key(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_schema_parses() {
        let s = Schema::contract();
        let node = &s.definitions["node"];
        assert!(node.relations.contains_key("link_viewer"));
        assert!(node.permissions.contains_key("share"));
        assert_eq!(
            node.relations["editors_share_off"],
            vec![AllowedSubject::Wildcard("user".into())]
        );
        assert!(
            node.relations["manager"]
                .contains(&AllowedSubject::Set("tenant".into(), "administer".into()))
        );
    }

    #[test]
    fn unknown_names_are_refused() {
        assert!(Schema::parse("definition a { permission p = nope }").is_err());
        assert!(Schema::parse("definition a { relation r: b }").is_err());
    }
}
