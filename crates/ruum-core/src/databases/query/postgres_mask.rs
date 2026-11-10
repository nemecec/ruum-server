//! The privacy filter of the query API (card Q-2; design 8.5; decisions 0004-3, 0004-4, 0004-5,
//! 0004-37 and 0005 B-2): the class of each result column, the markers of an agent's result, the
//! columns that a write reads or writes (`403 class_read_required`), and the destructive
//! statements of an agent.
//!
//! **The engine holds the line; this module labels.** On an engine with a lens
//! (`DatabaseEngine::has_lens`, the Postgres engine), the statement of an agent without
//! `read_personal` runs in a `masked_read` or `masked_write` session that sees only the views of
//! `ruum_masked`, so every clause reads pseudonyms, and `secret` columns and `personal` columns
//! of a tenant-defined type (0005 B-2) read as the lens constant
//! ([`SECRET_CONSTANT`](ruum_dbengine::postgres::lens::SECRET_CONSTANT)). An agent with
//! `read_personal` and a person without `manage` read through `ruum_nosecret`. What this module
//! computes from the text of a statement (the columns it reads) only chooses the class and the
//! marker of a result column, and refuses agent writes early. A mistake here can hide too much or
//! label a column wrongly, but it cannot show a value that the lens hides.
//!
//! # The class of a result column
//!
//! `ResultColumn.data_class` is the strictest class of the columns that the result column reads
//! (`normal` < `personal` < `secret`):
//!
//! - a plain column (the engine names its source column) has the class of that column;
//! - an expression of the top-level `SELECT` has the strictest class of the columns it names; a
//!   name that the server cannot resolve counts as `personal`; an expression that names no
//!   column (a constant, a parameter, `count(*)`, `current_user`) is `normal` and never masked;
//! - when the server cannot tell which item of the select list a result column is (a set
//!   operation, two wildcards), the class is the strictest class of every column the statement
//!   names, and at least `personal`.
//!
//! `EXPLAIN` gives the plan as text: its column is `normal`. A plan names columns and the
//! statement's own constants; it holds no row value (the lens views are in the plan).
//!
//! The classes of the columns come from the column store (card K-1) and the head manifest's
//! `data_classes`, the stricter of the two ([`ClassMap`]): a column that is in neither counts as
//! `personal`.
//!
//! # Markers ([`mark_result`])
//!
//! For a caller that reads through the `masked` lens:
//!
//! - `secret` column: every value that is not `NULL` becomes `{"$ruum": "hidden", "class":
//!   "secret"}` (`masking: hidden`);
//! - `personal` column: a value that is a pseudonym (`p_` and 12 hex characters) becomes
//!   `{"$ruum": "pseudonym", "id": ...}`; the lens constant (a `personal` column of a
//!   tenant-defined type, decision 0004-37 a) becomes `{"$ruum": "hidden", "class":
//!   "personal"}`; another value of an expression over pseudonyms (for example
//!   `length(email)`) stays as the engine computed it. Another value of a plain column (the
//!   engine names its source) can only come from a lens that is behind the classes: it becomes
//!   the hidden marker, and the server logs a warning (fail closed, review-Q-2-1 F-6). `masking`
//!   is `hidden` when every value that is not `NULL` is the constant (or hidden so), else
//!   `pseudonym`.
//!
//! For a caller that reads through the `nosecret` lens (an agent with `read_personal`, a person
//! without `manage`): every value of a `secret` column that is not `NULL` becomes the `secret`
//! marker. `personal` values are shown.
//!
//! For both lenses, a value that is exactly the lens constant in a column of a table or view
//! (the engine names a source) becomes the hidden marker even when the column's class is
//! `normal`: the lens masks the `personal` columns of a tenant view over a `secret` column like
//! `secret` (`lens.rs`, review-Q-1 N-9), and the platform does not know that class.
//!
//! # Writes ([`Footprint::write_class`])
//!
//! A write statement (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, also in a `WITH`) reads or writes
//! every column it names anywhere: the target columns, `SET`, `WHERE`, subqueries, `RETURNING`,
//! the `SELECT` of an `INSERT`. An `INSERT` without a column list writes every column of its
//! table. A name that the server cannot resolve counts as `personal` (fail closed).
//!
//! # Destructive statements ([`Footprint::destructive`])
//!
//! `TRUNCATE`, a `DELETE` or `UPDATE` without `WHERE` (also inside a `WITH`), and schema changes
//! of an agent never run at once (contract: `202` with an approval, card A-2).

use super::own_schema;
use ruum_dbengine::{DataClass, EngineKind};
use sqlparser::dialect::{Dialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::tokenizer::{Token, Tokenizer};
use std::collections::{HashMap, HashSet};

/// The lens constant of a masked value (`ruum_dbengine::postgres::lens::SECRET_CONSTANT`).
pub use ruum_dbengine::postgres::lens::SECRET_CONSTANT;

/// The stricter of two classes (`normal` < `personal` < `secret`).
pub fn stricter(a: DataClass, b: DataClass) -> DataClass {
    fn rank(c: DataClass) -> u8 {
        match c {
            DataClass::Normal => 0,
            DataClass::Personal => 1,
            DataClass::Secret => 2,
        }
    }
    if rank(b) > rank(a) { b } else { a }
}

// ------------------------------------------------------------------------------- classes

/// The classes of the columns of the database's own schema (`public`, SQLite `main`), by table.
/// Names are as the engine has them (case-sensitive).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassMap {
    tables: HashMap<String, HashMap<String, DataClass>>,
}

impl ClassMap {
    /// Adds a column with `class`, or makes its class stricter.
    pub fn add(&mut self, table: &str, column: &str, class: DataClass) {
        let cols = self.tables.entry(table.to_string()).or_default();
        let c = cols.entry(column.to_string()).or_insert(class);
        *c = stricter(*c, class);
    }

    /// The class of a column, if the map has it.
    pub fn class(&self, table: &str, column: &str) -> Option<DataClass> {
        self.tables.get(table).and_then(|t| t.get(column)).copied()
    }

    /// The table of `name` as written (an unquoted name folds to lower case on Postgres; SQLite
    /// compares names without case).
    fn table_name(&self, name: &Name) -> Option<String> {
        self.tables
            .keys()
            .find(|t| name.matches(t))
            .map(String::to_string)
    }

    fn column_names<'a>(&'a self, table: &str) -> impl Iterator<Item = (&'a String, DataClass)> {
        self.tables
            .get(table)
            .into_iter()
            .flat_map(|t| t.iter().map(|(k, v)| (k, *v)))
    }

    /// The strictest class of every column of a table (`personal` for a table the map does not
    /// have).
    fn whole_table(&self, table: &str) -> DataClass {
        match self.tables.get(table) {
            Some(cols) => cols.values().copied().fold(DataClass::Normal, stricter),
            None => DataClass::Personal,
        }
    }
}

/// Builds the [`ClassMap`] of the database's own schema: each live column of the column store
/// with its effective class, made stricter by the head manifest's `data_classes`.
pub fn class_map(
    engine: EngineKind,
    store: &[(String, String, String, DataClass)],
    manifest: &HashMap<String, DataClass>,
) -> ClassMap {
    let own = own_schema(engine);
    let mut map = ClassMap::default();
    for (schema, table, column, class) in store {
        if schema == own {
            let key = super::class_key(schema, table, column);
            let from_manifest = manifest.get(&key).copied().unwrap_or(DataClass::Personal);
            map.add(table, column, stricter(*class, from_manifest));
        }
    }
    // A manifest key of a column that the store does not have yet (a store that lags behind):
    // never less than `personal`.
    for (key, class) in manifest {
        if let Some((schema, table, column)) = parse_class_key(key)
            && schema == own
            && map.class(&table, &column).is_none()
        {
            map.add(&table, &column, stricter(*class, DataClass::Personal));
        }
    }
    map
}

/// Splits a `data_classes` key (`schema.table.column`; a part with a dot or a double quote is
/// quoted as in SQL) into its three parts.
pub fn parse_class_key(key: &str) -> Option<(String, String, String)> {
    let mut parts = Vec::new();
    let mut chars = key.chars().peekable();
    loop {
        let mut part = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next()? {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        part.push('"');
                    }
                    '"' => break,
                    c => part.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == '.' {
                    break;
                }
                part.push(c);
                chars.next();
            }
        }
        parts.push(part);
        match chars.next() {
            Some('.') => continue,
            None => break,
            Some(_) => return None,
        }
    }
    match <[String; 3]>::try_from(parts) {
        Ok([s, t, c]) if !s.is_empty() && !t.is_empty() && !c.is_empty() => Some((s, t, c)),
        _ => None,
    }
}

// --------------------------------------------------------------------------- the footprint

/// A name in a statement: unquoted names compare without case (Postgres folds them to lower
/// case; SQLite compares without case), quoted names exactly (SQLite: without case too).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Name {
    text: String,
    quoted: bool,
    fold: bool,
}

impl Name {
    fn matches(&self, engine_name: &str) -> bool {
        if self.fold || !self.quoted {
            self.text.eq_ignore_ascii_case(engine_name)
        } else {
            self.text == engine_name
        }
    }
}

/// One reference of a statement to a column.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ref {
    /// A column, with the qualifier it was written with.
    Column(Option<Name>, Name),
    /// Every column of a table or alias (`t.*`, `*`).
    Whole(Option<Name>),
}

/// The references of a part of a statement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Refs {
    refs: Vec<Ref>,
}

/// An item of the select list of the top-level `SELECT`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// `*` or `t.*`: one or more result columns, each a plain column.
    Wildcard,
    Expr(Refs),
}

/// What the server reads from the text of one statement: the column references, the select list
/// of the top-level `SELECT`, the tables and their aliases. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    fold: bool,
    /// Every reference of the statement.
    all: Refs,
    /// The items of the top-level select list; `None` when the statement has none, or a set
    /// operation makes the columns come from more than one select list.
    items: Option<Vec<Item>>,
    /// The tables that the statement names (words that are table names of the [`ClassMap`] are
    /// found at resolve time; these are the names written in a table position).
    table_words: Vec<Name>,
    /// Alias -> what it stands for (`Some(table name)`, `None` for a subquery or a CTE).
    aliases: Vec<(Name, Option<Name>)>,
    /// Aliases with a column list (`people p(a, b)`): the alias, its table and the new column
    /// names. The names map to the table's columns by position, which the server does not know,
    /// so each counts as the strictest column of the table (review-Q-2-2 P-4).
    alias_lists: Vec<(Name, Name, Vec<Name>)>,
    /// `INSERT INTO t` without a column list (also `DEFAULT VALUES`), or `MERGE ... INSERT
    /// VALUES`: every column of `t` is written.
    insert_all: Option<Name>,
    explain: bool,
    destructive: bool,
    writes: bool,
}

/// Words that are never a column or an alias in the statements of the query API.
const RESERVED: &[&str] = &[
    "ALL",
    "ANALYSE",
    "ANALYZE",
    "AND",
    "ANY",
    "ARRAY",
    "AS",
    "ASC",
    "ASYMMETRIC",
    "AT",
    "BETWEEN",
    "BOTH",
    "BY",
    "CASE",
    "CAST",
    "COLLATE",
    "CONFLICT",
    "CROSS",
    "CURRENT",
    "CURRENT_CATALOG",
    "CURRENT_DATE",
    "CURRENT_ROLE",
    "CURRENT_SCHEMA",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DO",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXCLUDE",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FROM",
    "FULL",
    "GLOB",
    "GROUP",
    "GROUPS",
    "HAVING",
    "ILIKE",
    "IN",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "KEY",
    "LAST",
    "LATERAL",
    "LEADING",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "LOCKED",
    "MATCH",
    "MATCHED",
    "MATERIALIZED",
    "MERGE",
    "NATURAL",
    "NEXT",
    "NO",
    "NOT",
    "NOTHING",
    "NOTNULL",
    "NOWAIT",
    "NULL",
    "NULLS",
    "OF",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OTHERS",
    "OUTER",
    "OVER",
    "OVERLAPS",
    "PARTITION",
    "PRECEDING",
    "PRECISION",
    "RANGE",
    "RECURSIVE",
    "REGEXP",
    "RETURNING",
    "RIGHT",
    "ROW",
    "ROWS",
    "SELECT",
    "SESSION_USER",
    "SET",
    "SHARE",
    "SIMILAR",
    "SKIP",
    "SOME",
    "SYMMETRIC",
    "TABLE",
    "THEN",
    "TIES",
    "TO",
    "TRAILING",
    "TRUE",
    "TRUNCATE",
    "UNBOUNDED",
    "UNION",
    "UNKNOWN",
    "UPDATE",
    "USER",
    "USING",
    "VALUES",
    "VARYING",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHIN",
    "WITHOUT",
];

/// Words that end the select list of a `SELECT` (at the same depth).
const AFTER_SELECT_LIST: &[&str] = &[
    "FROM",
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "WINDOW",
    "FETCH",
    "FOR",
    "INTO",
];

/// Words that cannot be the alias of a table (they start the next clause).
const NOT_AN_ALIAS: &[&str] = &[
    "WHERE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "FULL",
    "CROSS",
    "NATURAL",
    "ON",
    "USING",
    "GROUP",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "SET",
    "VALUES",
    "RETURNING",
    "WINDOW",
    "HAVING",
    "FETCH",
    "FOR",
    "DEFAULT",
    "SELECT",
    "LATERAL",
    "TABLESAMPLE",
    "WHEN",
    "THEN",
    "AND",
    "OR",
    "AS",
    "WITH",
    "DO",
    "OUTER",
    "INDEXED",
    "NOT",
];

/// Schemas whose names a statement may write (`pg_catalog.x`, ...): never the database's own
/// columns.
const SYSTEM_SCHEMAS: &[&str] = &["pg_catalog", "information_schema", "extensions", "temp"];

fn dialect(engine: EngineKind) -> Box<dyn Dialect> {
    match engine {
        EngineKind::Postgres => Box::new(PostgreSqlDialect {}),
        EngineKind::Sqlite => Box::new(SQLiteDialect {}),
    }
}

/// A significant token: whitespace and comments left out.
#[derive(Clone, Debug, PartialEq)]
enum T {
    /// An unquoted word, upper case for keyword checks, with its text.
    Word(String, String),
    /// A quoted name.
    Quoted(String),
    LParen,
    RParen,
    Comma,
    Period,
    Star,
    DoubleColon,
    SemiColon,
    /// A literal or a parameter.
    Lit,
    /// Any other token (operators).
    Other,
}

impl T {
    fn kw(&self) -> Option<&str> {
        match self {
            T::Word(up, _) => Some(up.as_str()),
            _ => None,
        }
    }

    fn is_kw(&self, k: &str) -> bool {
        self.kw() == Some(k)
    }

    fn name(&self, fold: bool) -> Option<Name> {
        match self {
            T::Word(_, text) => Some(Name {
                text: text.clone(),
                quoted: false,
                fold,
            }),
            T::Quoted(text) => Some(Name {
                text: text.clone(),
                quoted: true,
                fold,
            }),
            _ => None,
        }
    }

    fn is_name(&self) -> bool {
        matches!(self, T::Word(..) | T::Quoted(_))
    }
}

fn tokens(engine: EngineKind, sql: &str) -> Option<Vec<T>> {
    let dialect = dialect(engine);
    let raw = Tokenizer::new(&*dialect, sql).tokenize().ok()?;
    Some(
        raw.into_iter()
            .filter(|t| !matches!(t, Token::Whitespace(_) | Token::EOF))
            .map(|t| match t {
                Token::Word(w) => match w.quote_style {
                    None => T::Word(w.value.to_ascii_uppercase(), w.value),
                    Some(_) => T::Quoted(w.value),
                },
                Token::LParen => T::LParen,
                Token::RParen => T::RParen,
                Token::Comma => T::Comma,
                Token::Period => T::Period,
                Token::Mul => T::Star,
                Token::DoubleColon => T::DoubleColon,
                Token::SemiColon => T::SemiColon,
                Token::Number(..)
                | Token::SingleQuotedString(_)
                | Token::DoubleQuotedString(_)
                | Token::DollarQuotedString(_)
                | Token::NationalStringLiteral(_)
                | Token::EscapedStringLiteral(_)
                | Token::UnicodeStringLiteral(_)
                | Token::HexStringLiteral(_)
                | Token::SingleQuotedByteStringLiteral(_)
                | Token::Placeholder(_) => T::Lit,
                _ => T::Other,
            })
            .collect(),
    )
}

/// The paren depth before each token.
fn depths(ts: &[T]) -> Vec<i32> {
    let mut d = 0;
    ts.iter()
        .map(|t| {
            let here = d;
            match t {
                T::LParen => d += 1,
                T::RParen => d -= 1,
                _ => {}
            }
            here
        })
        .collect()
}

/// The index after a balanced group that starts at `i` (`ts[i]` is `(`).
fn skip_group(ts: &[T], i: usize) -> usize {
    let mut d = 0;
    let mut j = i;
    while j < ts.len() {
        match ts[j] {
            T::LParen => d += 1,
            T::RParen => {
                d -= 1;
                if d == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    ts.len()
}

impl Footprint {
    /// Reads the footprint of one statement in the dialect of `engine`. A statement that does
    /// not tokenize gives an empty footprint with every class unknown (the statement rules
    /// refuse it before).
    pub fn of(engine: EngineKind, sql: &str, tables: &ClassMap) -> Footprint {
        let fold = engine == EngineKind::Sqlite;
        let Some(ts) = tokens(engine, sql) else {
            return Footprint {
                fold,
                all: Refs {
                    refs: vec![Ref::Whole(None)],
                },
                ..Footprint::default()
            };
        };
        let mut f = Footprint {
            fold,
            ..Footprint::default()
        };
        f.read_names(&ts, tables);
        f.all = f.refs_in(&ts, tables);
        f.items = f.select_list(&ts, tables);
        f.explain = ts.first().is_some_and(|t| t.is_kw("EXPLAIN"));
        f.destructive = destructive(&ts);
        f.writes = writes(&ts);
        f.insert_all = insert_all(&ts, fold);
        f
    }

    /// Finds the table words, the aliases and the CTE names.
    fn read_names(&mut self, ts: &[T], tables: &ClassMap) {
        let fold = self.fold;
        for i in 0..ts.len() {
            let Some(name) = ts[i].name(fold) else {
                continue;
            };
            let prev = i.checked_sub(1).map(|p| &ts[p]);
            let next = ts.get(i + 1);
            // A CTE: `WITH name AS (`, `, name AS (`, also with a column list
            // (`name (a, b) AS (`) or `[NOT] MATERIALIZED`.
            let as_at = if matches!(next, Some(T::LParen)) {
                skip_group(ts, i + 1)
            } else {
                i + 1
            };
            let mut body = as_at + 1;
            while ts
                .get(body)
                .is_some_and(|t| t.is_kw("NOT") || t.is_kw("MATERIALIZED"))
            {
                body += 1;
            }
            let cte = prev
                .is_some_and(|p| matches!(p, T::Comma) || p.is_kw("WITH") || p.is_kw("RECURSIVE"))
                && ts.get(as_at).is_some_and(|t| t.is_kw("AS"))
                && matches!(ts.get(body), Some(T::LParen));
            if cte && !ts[i].kw().is_some_and(|k| RESERVED.contains(&k)) {
                self.aliases.push((name, None));
                continue;
            }
            let is_table = tables.table_name(&name).is_some()
                && !matches!(next, Some(T::Period))
                && !matches!(prev, Some(T::Period));
            if !is_table {
                continue;
            }
            self.table_words.push(name.clone());
            // An alias: `t a`, `t AS a`.
            let mut j = i + 1;
            if ts.get(j).is_some_and(|t| t.is_kw("AS")) {
                j += 1;
            }
            if let Some(alias) = ts.get(j).and_then(|t| alias_name(t, fold)) {
                if matches!(ts.get(j + 1), Some(T::LParen)) {
                    let end = skip_group(ts, j + 1);
                    let cols = ts[j + 2..end.saturating_sub(1).max(j + 2)]
                        .iter()
                        .filter_map(|t| t.name(fold))
                        .collect();
                    self.alias_lists.push((alias.clone(), name.clone(), cols));
                }
                self.aliases.push((alias, Some(name)));
            }
        }
        // Aliases of subqueries: `) a`, `) AS a`.
        for i in 0..ts.len() {
            if ts[i] != T::RParen {
                continue;
            }
            let mut j = i + 1;
            let had_as = ts.get(j).is_some_and(|t| t.is_kw("AS"));
            if had_as {
                j += 1;
            }
            // `CAST(x AS t)` is `)` after; `(...) AS (` is a CTE body.
            if let Some(alias) = ts.get(j).and_then(|t| alias_name(t, fold))
                && !matches!(ts.get(j + 1), Some(T::LParen))
            {
                self.aliases.push((alias, None));
            }
        }
    }

    /// Whether `name` is a table of the statement or an alias (of a table, a subquery or a CTE).
    fn is_relation(&self, name: &Name, tables: &ClassMap) -> bool {
        self.alias(name).is_some() || tables.table_name(name).is_some()
    }

    /// The table that `name` (a table or an alias) stands for, if it is a table.
    fn relation_table(&self, name: &Name, tables: &ClassMap) -> Option<String> {
        match self.alias(name) {
            Some(t) => t.as_ref().and_then(|n| tables.table_name(n)),
            None => tables.table_name(name),
        }
    }

    /// The table of an alias with a column list whose list holds `column`, or whose alias is
    /// `qualifier` (see `alias_lists`).
    fn renamed_table(
        &self,
        qualifier: Option<&Name>,
        column: &Name,
        tables: &ClassMap,
    ) -> Option<String> {
        self.alias_lists.iter().find_map(|(alias, table, cols)| {
            let hit = match qualifier {
                Some(q) => {
                    q.text.eq_ignore_ascii_case(&alias.text)
                        && (q.quoted == alias.quoted || self.fold)
                }
                None => cols.iter().any(|c| {
                    c.text.eq_ignore_ascii_case(&column.text)
                        && (c.quoted == column.quoted || self.fold)
                }),
            };
            if hit { tables.table_name(table) } else { None }
        })
    }

    fn alias(&self, name: &Name) -> Option<&Option<Name>> {
        self.aliases
            .iter()
            .find(|(a, _)| {
                a.text.eq_ignore_ascii_case(&name.text) && (a.quoted == name.quoted || self.fold)
            })
            .map(|(_, t)| t)
    }

    /// The column references in `ts` (see the module docs).
    fn refs_in(&self, ts: &[T], tables: &ClassMap) -> Refs {
        let mut refs = Vec::new();
        let mut i = 0;
        while i < ts.len() {
            let t = &ts[i];
            let prev = i.checked_sub(1).map(|p| &ts[p]);
            let next = ts.get(i + 1);
            if *t == T::Star && matches!(prev, Some(T::Comma) | None)
                || *t == T::Star
                    && prev.is_some_and(|p| {
                        p.is_kw("SELECT")
                            || p.is_kw("DISTINCT")
                            || p.is_kw("ALL")
                            || p.is_kw("RETURNING")
                    })
            {
                refs.push(Ref::Whole(None));
                i += 1;
                continue;
            }
            if !t.is_name() {
                i += 1;
                continue;
            }
            // Functional notation (review-Q-2-2 P-3): Postgres reads `email(p)` as `p.email`
            // when `p` is a table or an alias, and calls a function `f(p)` with the whole row.
            if matches!(next, Some(T::LParen))
                && !matches!(prev, Some(T::DoubleColon))
                && !prev.is_some_and(|p| p.is_kw("AS"))
                && ts.get(i + 2).is_some_and(T::is_name)
                && matches!(ts.get(i + 3), Some(T::RParen))
                && let Some(arg) = ts[i + 2].name(self.fold)
                && self.is_relation(&arg, tables)
            {
                let func = t.name(self.fold).expect("a name");
                let column = self
                    .relation_table(&arg, tables)
                    .is_some_and(|table| tables.column_names(&table).any(|(c, _)| func.matches(c)));
                refs.push(if column {
                    Ref::Column(Some(arg), func)
                } else {
                    Ref::Whole(Some(arg))
                });
                i += 4;
                continue;
            }
            // A type after `::`, an alias after `AS`, a function name.
            if matches!(prev, Some(T::DoubleColon))
                || prev.is_some_and(|p| p.is_kw("AS"))
                || matches!(next, Some(T::LParen))
            {
                i += 1;
                continue;
            }
            let name = t.name(self.fold).expect("a name");
            // A reserved word, unless a column of the database has that name (Postgres allows
            // non-reserved keywords such as `key` or `first` as column names).
            if t.kw().is_some_and(|k| RESERVED.contains(&k))
                && !tables
                    .tables
                    .values()
                    .any(|cols| cols.keys().any(|c| name.matches(c)))
            {
                i += 1;
                continue;
            }
            // A qualified name: `q.c`, `q.*`, `s.t.c`.
            if matches!(next, Some(T::Period)) {
                if SYSTEM_SCHEMAS
                    .iter()
                    .any(|s| name.text.eq_ignore_ascii_case(s))
                {
                    // Skip the whole chain.
                    let mut j = i + 1;
                    while matches!(ts.get(j), Some(T::Period)) && ts.get(j + 1).is_some() {
                        j += 2;
                    }
                    i = j;
                    continue;
                }
                match ts.get(i + 2) {
                    Some(T::Star) => {
                        refs.push(Ref::Whole(Some(name)));
                        i += 3;
                    }
                    Some(c) if c.is_name() => {
                        // `s.t.c`: the last two parts.
                        if matches!(ts.get(i + 3), Some(T::Period))
                            && ts.get(i + 4).is_some_and(T::is_name)
                        {
                            let q = c.name(self.fold).expect("a name");
                            let col = ts[i + 4].name(self.fold).expect("a name");
                            refs.push(Ref::Column(Some(q), col));
                            i += 5;
                        } else {
                            refs.push(Ref::Column(Some(name), c.name(self.fold).expect("a name")));
                            i += 3;
                        }
                    }
                    _ => i += 2,
                }
                continue;
            }
            // A table, an alias or a CTE name; else a column.
            let is_table = tables.table_name(&name).is_some()
                && !tables
                    .tables
                    .values()
                    .any(|cols| cols.keys().any(|c| name.matches(c)));
            if is_table || self.alias(&name).is_some() {
                // A table or alias in an expression is its whole row (`p::text`,
                // `row_to_json(p)`, `select p`): every column of it (review-Q-2-2 P-3, P-4).
                if in_expression(ts, i) {
                    refs.push(Ref::Whole(Some(name)));
                }
                i += 1;
                continue;
            }
            // A column named like a table counts as both (stricter).
            refs.push(Ref::Column(None, name));
            i += 1;
        }
        Refs { refs }
    }

    /// The items of the select list of the top-level `SELECT` (after a `WITH`), or `None`.
    fn select_list(&self, ts: &[T], tables: &ClassMap) -> Option<Vec<Item>> {
        let depth = depths(ts);
        let mut i = 0;
        // `WITH [RECURSIVE] name [(cols)] AS [NOT] [MATERIALIZED] ( ... ) [, ...]`.
        if ts.first()?.is_kw("WITH") {
            i = 1;
            loop {
                while i < ts.len() && ts[i] != T::LParen {
                    i += 1;
                }
                // A column list `name (a, b) AS (...)`: skip it and look for the body.
                let after = skip_group(ts, i);
                if ts.get(after).is_some_and(|t| t.is_kw("AS")) {
                    i = after;
                    continue;
                }
                i = after;
                if matches!(ts.get(i), Some(T::Comma)) {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        if !ts.get(i)?.is_kw("SELECT") || depth[i] != 0 {
            return None;
        }
        // A set operation at depth 0 anywhere after: the columns come from several lists.
        if ts[i..].iter().zip(&depth[i..]).any(|(t, d)| {
            *d == 0 && (t.is_kw("UNION") || t.is_kw("INTERSECT") || t.is_kw("EXCEPT"))
        }) {
            return None;
        }
        i += 1;
        if ts.get(i).is_some_and(|t| t.is_kw("ALL")) {
            i += 1;
        } else if ts.get(i).is_some_and(|t| t.is_kw("DISTINCT")) {
            i += 1;
            if ts.get(i).is_some_and(|t| t.is_kw("ON")) && matches!(ts.get(i + 1), Some(T::LParen))
            {
                i = skip_group(ts, i + 1);
            }
        }
        let start = i;
        let mut end = ts.len();
        for j in start..ts.len() {
            if depth[j] == 0
                && (ts[j].kw().is_some_and(|k| AFTER_SELECT_LIST.contains(&k))
                    || ts[j] == T::SemiColon)
            {
                end = j;
                break;
            }
        }
        let mut items = Vec::new();
        let mut item_start = start;
        for j in start..=end {
            if j == end || (ts[j] == T::Comma && depth[j] == 0) {
                let part = &ts[item_start..j];
                items.push(self.item(part, tables));
                item_start = j + 1;
            }
        }
        Some(items)
    }

    fn item(&self, part: &[T], tables: &ClassMap) -> Item {
        match part {
            [T::Star] => return Item::Wildcard,
            [q, T::Period, T::Star] if q.is_name() => return Item::Wildcard,
            _ => {}
        }
        // A trailing alias: `expr AS a`, or `expr a` after a name, a literal or `)`.
        let mut body = part;
        if let [rest @ .., last] = part
            && last.is_name()
            && !last.kw().is_some_and(|k| RESERVED.contains(&k))
            && let [.., before] = rest
            && (before.is_name() || matches!(before, T::RParen | T::Lit))
            && !before
                .kw()
                .is_some_and(|k| RESERVED.contains(&k) && k != "AS")
        {
            body = if before.is_kw("AS") {
                &rest[..rest.len() - 1]
            } else {
                rest
            };
        }
        Item::Expr(self.refs_in(body, tables))
    }

    /// The class of one reference.
    fn class_of(&self, r: &Ref, tables: &ClassMap) -> DataClass {
        let resolve_table = |q: &Name| -> Option<Option<String>> {
            // Some(Some(table)): a table; Some(None): a subquery or CTE; None: unknown.
            if let Some(t) = self.alias(q) {
                return Some(t.as_ref().and_then(|n| tables.table_name(n)));
            }
            tables.table_name(q).map(Some)
        };
        let named_tables: HashSet<String> = self
            .table_words
            .iter()
            .filter_map(|n| tables.table_name(n))
            .collect();
        let by_name = |c: &Name| -> DataClass {
            let mut hit: Option<DataClass> = None;
            for t in &named_tables {
                for (col, class) in tables.column_names(t) {
                    if c.matches(col) {
                        hit = Some(hit.map_or(class, |h| stricter(h, class)));
                    }
                }
            }
            hit.unwrap_or(DataClass::Personal)
        };
        // A column of an alias with a column list: the strictest column of its table.
        if let Ref::Column(q, c) = r
            && let Some(table) = self.renamed_table(q.as_ref(), c, tables)
        {
            let base = match q {
                Some(_) => DataClass::Normal,
                None => by_name(c),
            };
            return stricter(base, tables.whole_table(&table));
        }
        match r {
            Ref::Column(Some(q), c) => match resolve_table(q) {
                Some(Some(table)) => tables
                    .column_names(&table)
                    .find(|(col, _)| c.matches(col))
                    .map(|(_, class)| class)
                    .unwrap_or(DataClass::Personal),
                // A subquery's column or an unknown qualifier: by name, at least personal.
                _ => stricter(by_name(c), DataClass::Personal),
            },
            Ref::Column(None, c) => by_name(c),
            Ref::Whole(Some(q)) => match resolve_table(q) {
                Some(Some(table)) => tables.whole_table(&table),
                _ => named_tables
                    .iter()
                    .map(|t| tables.whole_table(t))
                    .fold(DataClass::Personal, stricter),
            },
            Ref::Whole(None) => named_tables
                .iter()
                .map(|t| tables.whole_table(t))
                .fold(DataClass::Normal, stricter)
                .max_personal_if(named_tables.is_empty()),
        }
    }

    /// The strictest class of `refs`; `normal` when there is none.
    fn class_of_refs(&self, refs: &Refs, tables: &ClassMap) -> DataClass {
        refs.refs
            .iter()
            .map(|r| self.class_of(r, tables))
            .fold(DataClass::Normal, stricter)
    }

    /// The class of the whole statement: every column it names, at least `personal`.
    pub fn statement_class(&self, tables: &ClassMap) -> DataClass {
        stricter(self.class_of_refs(&self.all, tables), DataClass::Personal)
    }

    /// The class of each result column whose source the engine does not name, for a result of
    /// `n` columns: by its item of the select list when the server can tell which one it is,
    /// else [`Footprint::statement_class`]. `EXPLAIN` is `normal`.
    pub fn column_classes(&self, n: usize, tables: &ClassMap) -> Vec<DataClass> {
        if self.explain {
            return vec![DataClass::Normal; n];
        }
        let fallback = self.statement_class(tables);
        let Some(items) = &self.items else {
            return vec![fallback; n];
        };
        let wildcards = items.iter().filter(|i| **i == Item::Wildcard).count();
        let exprs = items.len() - wildcards;
        let per_item: Vec<DataClass> = match wildcards {
            0 if items.len() == n => items
                .iter()
                .map(|i| match i {
                    Item::Expr(r) => self.class_of_refs(r, tables),
                    Item::Wildcard => fallback,
                })
                .collect(),
            1 if n >= exprs => {
                let expand = n - exprs;
                let mut v = Vec::with_capacity(n);
                for i in items {
                    match i {
                        Item::Expr(r) => v.push(self.class_of_refs(r, tables)),
                        Item::Wildcard => v.extend(std::iter::repeat_n(fallback, expand)),
                    }
                }
                v
            }
            _ => vec![fallback; n],
        };
        per_item
    }

    /// The strictest class of the columns that a write reads or writes (module docs), or
    /// `normal` when it names none.
    pub fn write_class(&self, tables: &ClassMap) -> DataClass {
        let mut class = self.class_of_refs(&self.all, tables);
        if let Some(t) = &self.insert_all {
            let whole = tables
                .table_name(t)
                .map_or(DataClass::Personal, |t| tables.whole_table(&t));
            class = stricter(class, whole);
        }
        class
    }

    /// `TRUNCATE`, or a `DELETE` or `UPDATE` without `WHERE` (module docs).
    pub fn destructive(&self) -> bool {
        self.destructive
    }

    /// Whether the statement changes rows anywhere: `INSERT`, `UPDATE`, `DELETE`, `MERGE` or
    /// `TRUNCATE`, also inside a `WITH` (the statement rules classify a query with a
    /// data-modifying `WITH` as a read).
    pub fn writes(&self) -> bool {
        self.writes
    }
}

trait MaxPersonal {
    fn max_personal_if(self, cond: bool) -> Self;
}

impl MaxPersonal for DataClass {
    fn max_personal_if(self, cond: bool) -> Self {
        if cond {
            stricter(self, DataClass::Personal)
        } else {
            self
        }
    }
}

/// Words after which a name is a table (or a CTE), not an expression.
const TABLE_POSITION: &[&str] = &[
    "FROM", "JOIN", "UPDATE", "INTO", "ONLY", "TABLE", "LATERAL", "USING", "TRUNCATE", "VIEW",
    "EXISTS", "ANALYZE", "VACUUM",
];

/// Words that start a clause, for [`in_expression`].
const CLAUSES: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "SET",
    "ON",
    "USING",
    "RETURNING",
    "VALUES",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "WINDOW",
    "WHEN",
    "THEN",
];

/// Whether the table or alias name at `i` stands in an expression (its whole row), not in a
/// table position: not after `FROM`, `JOIN`, `UPDATE`, `INTO`, `ONLY`, `TABLE`, `LATERAL`,
/// `USING` (or `TRUNCATE`, `VIEW`, `IF EXISTS`, `ANALYZE`, `VACUUM`), not after a comma of a
/// `FROM` list, not the definition of an alias (after a table name or `)`), not the name of a
/// CTE (`x AS (`).
fn in_expression(ts: &[T], i: usize) -> bool {
    let prev = i.checked_sub(1).map(|p| &ts[p]);
    let next = ts.get(i + 1);
    if next.is_some_and(|t| t.is_kw("AS")) || matches!(next, Some(T::Period)) {
        return false;
    }
    match prev {
        // The start of a select item (the footprint reads items one by one).
        None => true,
        Some(p) if p.kw().is_some_and(|k| TABLE_POSITION.contains(&k)) => false,
        // `people p`, `) s`: the definition of an alias.
        Some(T::RParen) => false,
        Some(p) if p.is_name() && !p.kw().is_some_and(|k| RESERVED.contains(&k)) => false,
        Some(T::Comma) => {
            // A comma of a `FROM` list, or of an expression list.
            let depth = depths(ts);
            let d = depth[i];
            let mut j = i;
            while j > 0 {
                j -= 1;
                if depth[j] < d {
                    return true;
                }
                if depth[j] == d
                    && let Some(k) = ts[j].kw()
                    && CLAUSES.contains(&k)
                {
                    return !matches!(k, "FROM" | "USING");
                }
            }
            true
        }
        Some(_) => true,
    }
}

/// A word that can be an alias.
fn alias_name(t: &T, fold: bool) -> Option<Name> {
    match t {
        T::Word(up, _)
            if NOT_AN_ALIAS.contains(&up.as_str()) || RESERVED.contains(&up.as_str()) =>
        {
            None
        }
        T::Word(..) | T::Quoted(_) => t.name(fold),
        _ => None,
    }
}

/// Whether the statement has a `TRUNCATE`, or a `DELETE` or `UPDATE` without a `WHERE` at its
/// own depth (not `DO UPDATE` of `ON CONFLICT`, `FOR UPDATE`, `THEN UPDATE` / `THEN DELETE` of
/// `MERGE`).
fn destructive(ts: &[T]) -> bool {
    let depth = depths(ts);
    for (i, t) in ts.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| &ts[p]);
        if t.is_kw("TRUNCATE") {
            return true;
        }
        let is_dml = t.is_kw("DELETE") || t.is_kw("UPDATE");
        if !is_dml
            || prev.is_some_and(|p| {
                p.is_kw("DO")
                    || p.is_kw("FOR")
                    || p.is_kw("THEN")
                    || p.is_kw("NO")
                    || p.is_kw("KEY")
            })
        {
            continue;
        }
        // `ON DELETE CASCADE` of a schema change is not a statement.
        if prev.is_some_and(|p| p.is_kw("ON")) {
            continue;
        }
        let d = depth[i];
        let mut has_where = false;
        for j in i + 1..ts.len() {
            if depth[j] < d || (ts[j] == T::RParen && depth[j] == d) {
                break;
            }
            if depth[j] == d && ts[j].is_kw("WHERE") {
                has_where = true;
                break;
            }
        }
        if !has_where {
            return true;
        }
    }
    false
}

/// Whether a statement changes rows (see [`Footprint::writes`]): a DML keyword that is not part
/// of `FOR UPDATE`, `FOR NO KEY UPDATE`, `ON DELETE` / `ON UPDATE` of a key or a trigger.
fn writes(ts: &[T]) -> bool {
    ts.iter().enumerate().any(|(i, t)| {
        let prev = i.checked_sub(1).map(|p| &ts[p]);
        (t.is_kw("INSERT")
            || t.is_kw("UPDATE")
            || t.is_kw("DELETE")
            || t.is_kw("MERGE")
            || t.is_kw("TRUNCATE"))
            && !prev
                .is_some_and(|p| p.is_kw("FOR") || p.is_kw("KEY") || p.is_kw("ON") || p.is_kw("OR"))
    })
}

/// The table of `INSERT INTO t` without a column list (`VALUES`, `SELECT`, `DEFAULT VALUES`,
/// `WITH`), or of `MERGE INTO t ... INSERT VALUES`.
fn insert_all(ts: &[T], fold: bool) -> Option<Name> {
    for (i, t) in ts.iter().enumerate() {
        if t.is_kw("INSERT") && ts.get(i + 1).is_some_and(|t| t.is_kw("INTO")) {
            // `INSERT INTO t [AS a] (cols)`: the table may be qualified (refused before).
            let table = ts.get(i + 2)?.name(fold)?;
            let mut j = i + 3;
            if ts.get(j).is_some_and(|t| t.is_kw("AS")) {
                j += 2;
            } else if ts.get(j).is_some_and(|t| alias_name(t, fold).is_some())
                && !ts
                    .get(j)
                    .is_some_and(|t| t.is_kw("VALUES") || t.is_kw("SELECT"))
            {
                j += 1;
            }
            let column_list = matches!(ts.get(j), Some(T::LParen))
                && !ts
                    .get(j + 1)
                    .is_some_and(|t| t.is_kw("SELECT") || t.is_kw("WITH") || t.is_kw("VALUES"));
            if !column_list {
                return Some(table);
            }
        }
        if t.is_kw("MERGE") && ts.get(i + 1).is_some_and(|t| t.is_kw("INTO")) {
            let table = ts.get(i + 2)?.name(fold)?;
            let bare_insert = ts.windows(2).any(|w| {
                w[0].is_kw("INSERT")
                    && (w[1].is_kw("VALUES") || w[1].is_kw("DEFAULT") || w[1].is_kw("SELECT"))
            });
            if bare_insert {
                return Some(table);
            }
        }
    }
    None
}

// ------------------------------------------------------------------------------- markers

/// Which lens a caller reads through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lens {
    /// `owner`, `secret_read`: the tables, no masking.
    Base,
    /// `read`, `write`: `secret` columns masked.
    NoSecret,
    /// `masked_read`, `masked_write`: `personal` columns as pseudonyms, `secret` masked.
    Masked,
}

/// Whether `s` has the shape of a pseudonym of the platform (`p_` and 12 hex characters).
pub fn is_pseudonym(s: &str) -> bool {
    s.len() == 2 + crate::privacy::PSEUDONYM_HEX_LEN
        && s.starts_with("p_")
        && s[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One result column for [`mark_result`]: its class and whether the engine named a source
/// column in the database's own schema (a column that the lens covers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnFacts {
    pub class: DataClass,
    pub has_source: bool,
}

/// What [`mark_result`] decided for one column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marked {
    pub class: DataClass,
    /// `None`, `Some(true)` hidden, `Some(false)` pseudonym.
    pub masking: Option<bool>,
}

/// Turns the cells of a result into markers for a caller of `lens` (module docs). Returns the
/// class and the masking of each column.
pub fn mark_result(
    lens: Lens,
    columns: &[ColumnFacts],
    rows: &mut [Vec<serde_json::Value>],
) -> Vec<Marked> {
    use serde_json::Value as J;
    let hidden = |class: DataClass| {
        serde_json::to_value(match class {
            DataClass::Secret => crate::privacy::HiddenValue::SECRET,
            _ => crate::privacy::HiddenValue::PERSONAL,
        })
        .expect("a marker serializes")
    };
    let pseudonym = |id: &str| serde_json::json!({ "$ruum": "pseudonym", "id": id });
    let mut out = Vec::with_capacity(columns.len());
    for (i, c) in columns.iter().enumerate() {
        let mut class = c.class;
        let cells = || rows.iter().filter_map(|r| r.get(i));
        let constant_seen = cells().any(|v| v.as_str() == Some(SECRET_CONSTANT));
        // The lens masked a column like secret that the platform labels less strictly (a tenant
        // view over a secret column): take the lens's word.
        if lens != Lens::Base && c.has_source && constant_seen {
            class = match (lens, class) {
                (_, DataClass::Normal) | (Lens::NoSecret, _) => DataClass::Secret,
                (_, other) => other,
            };
        }
        let masking = match (lens, class) {
            (Lens::Base, _) | (_, DataClass::Normal) => None,
            (Lens::NoSecret, DataClass::Personal) => None,
            (_, DataClass::Secret) => {
                let m = hidden(DataClass::Secret);
                for r in rows.iter_mut() {
                    if let Some(cell) = r.get_mut(i)
                        && !cell.is_null()
                    {
                        *cell = m.clone();
                    }
                }
                Some(true)
            }
            (Lens::Masked, DataClass::Personal) => {
                let m = hidden(DataClass::Personal);
                let mut all_hidden = true;
                let mut any = false;
                let mut stale = false;
                for r in rows.iter_mut() {
                    let Some(cell) = r.get_mut(i) else { continue };
                    let next = match &*cell {
                        J::String(s) if s == SECRET_CONSTANT => Some(m.clone()),
                        J::String(s) if is_pseudonym(s) => {
                            all_hidden = false;
                            Some(pseudonym(s))
                        }
                        J::Null => None,
                        // A plain column of a table or view: through a current lens it holds a
                        // pseudonym, the constant or `NULL`. Anything else means the lens is
                        // behind the classes (review-Q-2-1 F-6): fail closed.
                        _ if c.has_source => {
                            stale = true;
                            Some(m.clone())
                        }
                        // An expression over pseudonyms (`length(email)`).
                        _ => {
                            all_hidden = false;
                            None
                        }
                    };
                    if !cell.is_null() {
                        any = true;
                    }
                    if let Some(n) = next {
                        *cell = n;
                    }
                }
                if stale {
                    tracing::warn!(
                        column = i,
                        "a personal column read through the masked lens held a value that is no \
                         pseudonym: the lens is behind the classes; the value is hidden"
                    );
                }
                Some(any && all_hidden)
            }
        };
        out.push(Marked { class, masking });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map() -> ClassMap {
        let mut m = ClassMap::default();
        for (t, c, k) in [
            ("people", "id", DataClass::Normal),
            ("people", "email", DataClass::Personal),
            ("people", "name", DataClass::Personal),
            ("people", "token", DataClass::Secret),
            ("people", "score", DataClass::Normal),
            ("orders", "id", DataClass::Normal),
            ("orders", "person_email", DataClass::Personal),
            ("orders", "label", DataClass::Normal),
            ("orders", "amount", DataClass::Normal),
        ] {
            m.add(t, c, k);
        }
        m
    }

    fn classes(sql: &str, n: usize) -> Vec<DataClass> {
        let m = map();
        Footprint::of(EngineKind::Postgres, sql, &m).column_classes(n, &m)
    }

    use DataClass::{Normal as N, Personal as P, Secret as S};

    #[test]
    fn expressions_take_the_strictest_class_they_read() {
        assert_eq!(
            classes(
                "select lower(email), email || '', coalesce(name, 'x'), upper(token), score + 1, \
                 'constant' from people where id = 1",
                6
            ),
            vec![P, P, P, S, N, N]
        );
        assert_eq!(classes("select 'guess', $1", 2), vec![N, N]);
        assert_eq!(classes("select count(*) from people", 1), vec![N]);
        assert_eq!(classes("select count(token) from people", 1), vec![S]);
        assert_eq!(classes("select current_user", 1), vec![N]);
        assert_eq!(
            classes(
                "select p.email, o.label from orders o join people p on p.email = o.person_email",
                2
            ),
            vec![P, N]
        );
        assert_eq!(classes("select length(pin) from people", 1), vec![P]);
        // An alias of the select list and of a table is no column.
        assert_eq!(
            classes("select score s, id as k from people p order by s", 2),
            vec![N, N]
        );
        // A subquery in the select list.
        assert_eq!(
            classes("select (select max(token) from people) as t", 1),
            vec![S]
        );
        // A cast: the type is no column.
        assert_eq!(classes("select score::text from people", 1), vec![N]);
    }

    #[test]
    fn the_server_falls_back_to_the_whole_statement_when_it_cannot_tell() {
        // A set operation: every column the statement names, at least personal.
        assert_eq!(
            classes("select id from people union select id from orders", 1),
            vec![P]
        );
        assert_eq!(
            classes(
                "select score from people union select length(token) from people",
                1
            ),
            vec![S]
        );
        // A CTE whose column has another name: the name is unknown, the statement names token.
        assert_eq!(
            classes(
                "with x as (select token as t from people) select upper(t) from x",
                1
            ),
            vec![P]
        );
        let m = map();
        let f = Footprint::of(
            EngineKind::Postgres,
            "with x as (select token as t from people) select upper(t) from x",
            &m,
        );
        assert_eq!(f.statement_class(&m), S);
        // One wildcard with expressions: aligned.
        assert_eq!(
            classes("select *, upper(token) from orders, people", 14)[13],
            S
        );
        // Two wildcards: the whole statement.
        assert_eq!(
            classes("select p.*, o.* from people p, orders o", 9),
            vec![S; 9]
        );
        // EXPLAIN: the plan is text.
        assert_eq!(classes("explain select * from people", 1), vec![N]);
    }

    #[test]
    fn writes_read_and_write_every_column_they_name() {
        let m = map();
        let w = |sql: &str| Footprint::of(EngineKind::Postgres, sql, &m).write_class(&m);
        assert_eq!(w("update people set score = score + 1 where id = 1"), N);
        assert_eq!(w("update people set nick = email where id = 1"), P);
        assert_eq!(
            w("update people set score = 1 where id = 1 returning email"),
            P
        );
        assert_eq!(w("update people set score = 1 where token is null"), S);
        assert_eq!(
            w(
                "insert into orders (id, person_email, label) select id + 100, 'x', email from people"
            ),
            P
        );
        assert_eq!(
            w("insert into orders (id, label, amount) values (1, 'a', 2)"),
            N
        );
        // Without a column list every column is written.
        assert_eq!(w("insert into orders values (1, 'x', 'y', 2)"), P);
        assert_eq!(w("insert into orders default values"), P);
        assert_eq!(w("insert into people select * from people"), S);
        assert_eq!(w("delete from orders o where o.id = 3"), N);
        assert_eq!(w("delete from people where email = 'x'"), P);
        assert_eq!(
            w(
                "with x as (select id from people) update orders set amount = 1 where id in (select id from x)"
            ),
            N
        );
        // A name the server cannot resolve counts as personal.
        assert_eq!(w("update orders set label = mystery where id = 1"), P);
        // Review-Q-2-2 P-3: a whole row (`p::text`, `row_to_json(p)`, `returning p`) reads every
        // column of its table; functional notation (`email(p)`) reads that column; an alias
        // with a column list renames columns, so each counts as the strictest of its table.
        assert_eq!(
            w(
                "update orders set label = (select p::text from people p where p.id = 1) where id = 1"
            ),
            S
        );
        assert_eq!(
            w(
                "update orders set label = email(p) from people p where p.id = orders.id and orders.id = 2"
            ),
            P
        );
        assert_eq!(
            w(
                "update orders set label = token(p) from people p where p.id = orders.id and orders.id = 3"
            ),
            S
        );
        assert_eq!(
            w(
                "update orders set label = row_to_json(p)::text from people p where p.id = orders.id"
            ),
            S
        );
        assert_eq!(
            w(
                "update orders set label = upper(score) from people p(id, score, x, y, z) where p.id = orders.id"
            ),
            S
        );
        assert_eq!(
            w("update people p set score = 1 where id = 1 returning p"),
            S
        );
        // Table positions stay tables.
        assert_eq!(
            w(
                "update orders o set amount = 1 from people p, orders q where o.id = p.id and q.id = o.id"
            ),
            N
        );
        assert_eq!(
            w("delete from orders o using people p where o.id = p.id"),
            N
        );
        assert_eq!(w("truncate orders"), N);
    }

    #[test]
    fn whole_rows_and_renamed_columns_label_strictly() {
        // Review-Q-2-2 P-4: never `normal` for a whole row or a renamed personal column.
        assert_eq!(
            classes("select row_to_json(p) from people p where id = 1", 1),
            vec![S]
        );
        assert_eq!(classes("select p::text, p.id from people p", 2), vec![S, N]);
        assert_eq!(
            classes(
                "select upper(score) from people p(id, score, x, y, z, w) where id = 1",
                1
            ),
            vec![S]
        );
        assert_eq!(
            classes("select email(p), score(p) from people p", 2),
            vec![P, N]
        );
        assert_eq!(classes("select to_json(o) from orders o", 1), vec![P]);
        // A table in a FROM list and an alias definition are no whole row.
        assert_eq!(
            classes(
                "select o.label, p.score from orders o, people p where o.id = p.id",
                2
            ),
            vec![N, N]
        );
        let m = map();
        let f = Footprint::of(
            EngineKind::Postgres,
            "select count(*) from orders o, people p where o.id = p.id",
            &m,
        );
        assert_eq!(f.column_classes(1, &m), vec![N]);
    }

    #[test]
    fn destructive_statements_are_found() {
        let m = map();
        let d = |sql: &str| Footprint::of(EngineKind::Postgres, sql, &m).destructive();
        assert!(d("delete from orders"));
        assert!(d("update orders set amount = 0"));
        assert!(d("truncate orders"));
        assert!(d(
            "with x as (delete from orders returning id) select count(*) from x"
        ));
        assert!(d(
            "update orders set amount = (select max(score) from people where id = 1)"
        ));
        assert!(!d("delete from orders where id = 1"));
        assert!(!d("update orders set amount = 0 where id = 1"));
        assert!(!d(
            "insert into orders (id) values (1) on conflict (id) do update set amount = 1"
        ));
        assert!(!d("select * from orders for update"));
        assert!(!d("select 1"));
        let w = |sql: &str| Footprint::of(EngineKind::Postgres, sql, &m).writes();
        assert!(w(
            "with x as (select 1) update orders set amount = 1 where id = 1"
        ));
        assert!(w(
            "with d as (delete from orders where id = 1 returning id) select * from d"
        ));
        assert!(w("insert into orders (id) values (1)"));
        assert!(!w("select * from orders for update"));
        assert!(!w("select * from orders for no key update"));
        assert!(!w("select 1"));
    }

    #[test]
    fn class_keys_parse_with_quotes() {
        assert_eq!(
            parse_class_key("public.people.email"),
            Some(("public".into(), "people".into(), "email".into()))
        );
        assert_eq!(
            parse_class_key(r#"public."we.ird"."a""b""#),
            Some(("public".into(), "we.ird".into(), "a\"b".into()))
        );
        assert_eq!(parse_class_key("public.people"), None);
        assert_eq!(parse_class_key("a.b.c.d"), None);
        for key in ["public.t.c", r#"public."x.y".z"#, r#""p""q".t."c.d""#] {
            let (s, t, c) = parse_class_key(key).unwrap();
            assert_eq!(super::super::class_key(&s, &t, &c), key);
        }
    }

    #[test]
    fn the_class_map_takes_the_stricter_of_store_and_manifest() {
        let store = vec![
            ("public".to_string(), "t".to_string(), "a".to_string(), N),
            ("public".to_string(), "t".to_string(), "b".to_string(), N),
            ("public".to_string(), "t".to_string(), "c".to_string(), S),
        ];
        let manifest = HashMap::from([
            ("public.t.a".to_string(), N),
            ("public.t.c".to_string(), N),
            ("public.t.d".to_string(), S),
            ("public.t.e".to_string(), N),
        ]);
        let m = class_map(EngineKind::Postgres, &store, &manifest);
        // b is not in the manifest (a person took it back): personal.
        assert_eq!(m.class("t", "a"), Some(N));
        assert_eq!(m.class("t", "b"), Some(P));
        assert_eq!(m.class("t", "c"), Some(S));
        assert_eq!(m.class("t", "d"), Some(S));
        assert_eq!(m.class("t", "e"), Some(P));
    }

    #[test]
    fn markers_follow_the_lens() {
        let cols = [
            ColumnFacts {
                class: N,
                has_source: true,
            },
            ColumnFacts {
                class: P,
                has_source: true,
            },
            ColumnFacts {
                class: S,
                has_source: true,
            },
            ColumnFacts {
                class: P,
                has_source: true,
            },
            ColumnFacts {
                class: P,
                has_source: false,
            },
            ColumnFacts {
                class: N,
                has_source: true,
            },
            ColumnFacts {
                class: N,
                has_source: false,
            },
        ];
        let row = || {
            vec![
                json!(1),
                json!("p_0123456789ab"),
                json!(SECRET_CONSTANT),
                json!(SECRET_CONSTANT),
                json!(14),
                json!(SECRET_CONSTANT),
                json!(SECRET_CONSTANT),
            ]
        };
        let mut rows = vec![
            row(),
            vec![
                json!(2),
                json!(null),
                json!(null),
                json!(null),
                json!(null),
                json!(null),
                json!("x"),
            ],
        ];
        let m = mark_result(Lens::Masked, &cols, &mut rows);
        assert_eq!(rows[0][0], json!(1));
        assert_eq!(
            rows[0][1],
            json!({ "$ruum": "pseudonym", "id": "p_0123456789ab" })
        );
        assert_eq!(rows[0][2], json!({ "$ruum": "hidden", "class": "secret" }));
        assert_eq!(
            rows[0][3],
            json!({ "$ruum": "hidden", "class": "personal" })
        );
        assert_eq!(rows[0][4], json!(14));
        // A view column that the lens masked like secret.
        assert_eq!(rows[0][5], json!({ "$ruum": "hidden", "class": "secret" }));
        // A constant of the statement itself is never masked.
        assert_eq!(rows[0][6], json!(SECRET_CONSTANT));
        assert_eq!(rows[1][1], json!(null));
        assert_eq!(
            m[1],
            Marked {
                class: P,
                masking: Some(false)
            }
        );
        assert_eq!(
            m[2],
            Marked {
                class: S,
                masking: Some(true)
            }
        );
        assert_eq!(
            m[3],
            Marked {
                class: P,
                masking: Some(true)
            }
        );
        assert_eq!(
            m[5],
            Marked {
                class: S,
                masking: Some(true)
            }
        );
        assert_eq!(
            m[6],
            Marked {
                class: N,
                masking: None
            }
        );

        // nosecret: personal values are shown, secrets hidden.
        let mut rows = vec![vec![json!("a@b"), json!("RUUM:HIDDEN")]];
        let m = mark_result(
            Lens::NoSecret,
            &[
                ColumnFacts {
                    class: P,
                    has_source: true,
                },
                ColumnFacts {
                    class: S,
                    has_source: false,
                },
            ],
            &mut rows,
        );
        assert_eq!(rows[0][0], json!("a@b"));
        assert_eq!(rows[0][1], json!({ "$ruum": "hidden", "class": "secret" }));
        assert_eq!(m[0].masking, None);
        // Review-Q-2-1 F-6: a raw value in a personal source column (a lens behind the classes)
        // is hidden, never labelled a pseudonym; an expression keeps its value.
        let mut rows = vec![
            vec![json!(10), json!(10)],
            vec![json!("p_0123456789ab"), json!(11)],
        ];
        let m = mark_result(
            Lens::Masked,
            &[
                ColumnFacts {
                    class: P,
                    has_source: true,
                },
                ColumnFacts {
                    class: P,
                    has_source: false,
                },
            ],
            &mut rows,
        );
        assert_eq!(
            rows[0][0],
            json!({ "$ruum": "hidden", "class": "personal" })
        );
        assert_eq!(
            rows[1][0],
            json!({ "$ruum": "pseudonym", "id": "p_0123456789ab" })
        );
        assert_eq!(rows[0][1], json!(10));
        assert_eq!(m[0].class, P);
        let mut rows = vec![vec![json!("nick-1")]];
        let m = mark_result(
            Lens::Masked,
            &[ColumnFacts {
                class: P,
                has_source: true,
            }],
            &mut rows,
        );
        assert_eq!(
            rows[0][0],
            json!({ "$ruum": "hidden", "class": "personal" })
        );
        assert_eq!(m[0].masking, Some(true));
        // Base: nothing changes.
        let mut rows = vec![vec![json!("tok")]];
        mark_result(
            Lens::Base,
            &[ColumnFacts {
                class: S,
                has_source: true,
            }],
            &mut rows,
        );
        assert_eq!(rows[0][0], json!("tok"));
    }

    #[test]
    fn pseudonym_shapes() {
        assert!(is_pseudonym("p_0123456789ab"));
        assert!(!is_pseudonym("p_0123456789AB"));
        assert!(!is_pseudonym("p_0123456789a"));
        assert!(!is_pseudonym("x_0123456789ab"));
    }
}

#[cfg(test)]
#[path = "postgres_mask_tests.rs"]
mod engine_tests;
