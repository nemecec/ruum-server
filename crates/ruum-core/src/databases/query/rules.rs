//! The statement rules of the query API (card Q-1; `queryDatabase` "Statements that never run"
//! and "Table names" in `contracts/openapi.yaml`; decisions 0004-34 d and 0004-35 a, b) and the
//! normalized statement of the query log (decisions 0004-9 and 0004-24).
//!
//! These checks are a first line only: the engine sessions hold every rule even when a statement
//! gets past them (`docs/STEP2.md` "The engine security model"). They run before the engine, in
//! this order:
//!
//! 1. The statement must tokenize in the dialect of the database's engine (`sqlparser`,
//!    `PostgreSqlDialect` or `SQLiteDialect`). A statement that does not is refused (fail
//!    closed).
//! 2. Exactly one statement: an empty request or a second statement is `validation_failed`.
//! 3. Transaction control (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`,
//!    `SAVEPOINT`, `RELEASE`, `PREPARE TRANSACTION`, ...) is `statement_not_allowed` with the
//!    hint `remove_transaction_control`.
//! 4. `SET` (also `SET LOCAL`, `SET ROLE`, `SET SESSION AUTHORIZATION`), `RESET` and `DISCARD`
//!    are refused; so is `set_config` anywhere in the statement.
//! 5. Any name that starts with `ruum_` (the platform's schemas and objects) is refused. On
//!    Postgres, a Unicode-escaped name (`U&"..."`, also with `UESCAPE`) is refused first: the
//!    name rules compare the text of a word, and Postgres reads `U&"set\005fconfig"` as
//!    `set_config` (review Q-1, R-1).
//! 6. `public.` (Postgres) or `main.` (SQLite) before a name is refused with the hint
//!    `unqualified_names`. The server never rewrites a name. `pg_catalog`, `information_schema`
//!    and `extensions` may be named.
//! 7. On SQLite: `load_extension`, `readfile` and `writefile` anywhere.
//! 8. The statement must parse; one that does not is refused (fail closed).
//! 9. Only these statement kinds run: queries (`SELECT`, `WITH`, `VALUES`), `INSERT`, `UPDATE`,
//!    `DELETE`, `MERGE`, `TRUNCATE`, `EXPLAIN` of one of them, and schema changes (`CREATE` and
//!    `ALTER` of tables, views, indexes, sequences, types, domains, functions and triggers; their
//!    `DROP`; `COMMENT ON`; `ALTER TABLE ... RENAME`). On SQLite also `PRAGMA` of the read-only
//!    allow-list. Every other kind (roles, `GRANT`, `REVOKE`, schemas, databases, extensions,
//!    `COPY`, `CALL`, `DO`, `ATTACH`, `DETACH`, `VACUUM`, `LISTEN`, ...) is refused.
//!
//! `set_config` inside a function body or a `DO` block is out of the parser's reach: the engine
//! holds that line (card E-2). A `DO` block does not parse, so it is refused here anyway.
//!
//! The parse runs on a thread with a large stack: a hostile statement of 100 000 characters can
//! nest deeply (a long chain of operators), and both the parse and the drop of its syntax tree
//! recurse.

use ruum_dbengine::EngineKind;
use sqlparser::ast::{ObjectType, Query, SetExpr, Statement};
use sqlparser::dialect::{Dialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

/// The text the query log stores for a statement that does not parse (decision 0004-9).
pub const UNPARSED: &str = "<unparsed statement>";

/// The hint of a refused transaction-control statement (decision 0004-35).
pub const HINT_TRANSACTION_CONTROL: &str = "remove_transaction_control";
/// The hint of a refused `public.` or `main.` before a name (decision 0004-35 b).
pub const HINT_UNQUALIFIED_NAMES: &str = "unqualified_names";

/// The stack of the parse thread (see the module docs).
const PARSE_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The read-only `PRAGMA`s of SQLite that a statement may run (decision 0004-2).
pub const SQLITE_PRAGMA_ALLOW_LIST: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
];

/// What a statement that may run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementKind {
    /// Reads only (as far as the parser can tell; the engine has the last word).
    Read,
    /// Changes rows.
    Write,
    /// Changes the schema (DDL). A person with `manage` runs it through the engine's migration
    /// path (wave decision 3).
    SchemaChange,
}

/// The error code of a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalCode {
    /// `422 validation_failed`: no statement, or more than one.
    ValidationFailed,
    /// `422 statement_not_allowed`.
    StatementNotAllowed,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalCode::ValidationFailed => "validation_failed",
            RefusalCode::StatementNotAllowed => "statement_not_allowed",
        }
    }
}

/// Why a statement does not run. The texts name the rule, never a value of the statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: RefusalCode,
    pub message: String,
    pub hint: Option<String>,
}

impl Refusal {
    fn not_allowed(message: impl Into<String>, hint: Option<&str>) -> Refusal {
        Refusal {
            code: RefusalCode::StatementNotAllowed,
            message: message.into(),
            hint: hint.map(str::to_string),
        }
    }

    fn validation(message: impl Into<String>, hint: &str) -> Refusal {
        Refusal {
            code: RefusalCode::ValidationFailed,
            message: message.into(),
            hint: Some(hint.to_string()),
        }
    }
}

/// The outcome of [`analyze`]: the verdict and the normalized statement for the query log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub verdict: Result<StatementKind, Refusal>,
    /// Every literal replaced by `$L1`, `$L2`, ... and every comment removed; [`UNPARSED`] when
    /// the statement does not parse.
    pub normalized: String,
}

/// Checks one statement against the rules (see the module docs) and normalizes it, in the
/// dialect of `engine`.
pub fn analyze(engine: EngineKind, sql: &str) -> Analysis {
    let sql = sql.to_string();
    let run = move || analyze_here(engine, &sql);
    match std::thread::Builder::new()
        .name("ruum-sql-parse".into())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(run)
        .map(|h| h.join())
    {
        Ok(Ok(analysis)) => analysis,
        // The thread could not start or the parser panicked: refuse (fail closed).
        _ => Analysis {
            verdict: Err(unparsed()),
            normalized: UNPARSED.to_string(),
        },
    }
}

fn dialect(engine: EngineKind) -> Box<dyn Dialect> {
    match engine {
        EngineKind::Postgres => Box::new(PostgreSqlDialect {}),
        EngineKind::Sqlite => Box::new(SQLiteDialect {}),
    }
}

fn unparsed() -> Refusal {
    Refusal::not_allowed(
        "the server could not parse the statement, and it runs only statements that it can check",
        Some(
            "check the syntax for this database's engine; DO blocks do not run; on SQLite, quote \
             the table name of a PRAGMA, as in pragma table_info('plants')",
        ),
    )
}

/// [`analyze`] on the current thread.
fn analyze_here(engine: EngineKind, sql: &str) -> Analysis {
    let dialect = dialect(engine);
    let tokens = match Tokenizer::new(&*dialect, sql)
        .with_unescape(false)
        .tokenize()
    {
        Ok(t) => t,
        Err(_) => {
            return Analysis {
                verdict: Err(unparsed()),
                normalized: UNPARSED.to_string(),
            };
        }
    };
    let parsed = Parser::parse_sql(&*dialect, sql).ok();
    let normalized = match &parsed {
        Some(_) => normalize_tokens(engine, &tokens),
        None => UNPARSED.to_string(),
    };
    let verdict = check(engine, &tokens, parsed.as_deref());
    Analysis {
        verdict,
        normalized,
    }
}

/// The tokens that are not whitespace or comments.
fn significant(tokens: &[Token]) -> Vec<&Token> {
    tokens
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace(_) | Token::EOF))
        .collect()
}

/// The upper-case text of an unquoted word.
fn keyword(t: Option<&&Token>) -> Option<String> {
    match t {
        Some(Token::Word(w)) if w.quote_style.is_none() => Some(w.value.to_ascii_uppercase()),
        _ => None,
    }
}

/// The number of statements by the semicolons of the tokens (when the parse fails).
fn token_statements(sig: &[&Token]) -> usize {
    sig.split(|t| matches!(t, Token::SemiColon))
        .filter(|s| !s.is_empty())
        .count()
}

fn check(
    engine: EngineKind,
    tokens: &[Token],
    parsed: Option<&[Statement]>,
) -> Result<StatementKind, Refusal> {
    let sig = significant(tokens);
    // 2. Exactly one statement.
    let count = parsed.map_or_else(|| token_statements(&sig), <[Statement]>::len);
    if count == 0 {
        return Err(Refusal::validation(
            "the request holds no statement",
            "/sql: send one SQL statement",
        ));
    }
    if count > 1 {
        return Err(Refusal::validation(
            format!("send one statement per call; this request holds {count}"),
            "/sql: send each statement in its own call",
        ));
    }
    // 3. Transaction control (by the first words, so it holds also when the parse fails).
    let first = keyword(sig.first());
    let second = keyword(sig.get(1));
    let transaction = match first.as_deref() {
        Some("BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" | "RELEASE") => true,
        Some("START" | "PREPARE") => second.as_deref() == Some("TRANSACTION"),
        _ => false,
    };
    if transaction {
        return Err(transaction_control());
    }
    // 4. Session statements.
    if matches!(first.as_deref(), Some("SET" | "RESET" | "DISCARD")) {
        return Err(session_statement());
    }
    // 4 to 7. Names anywhere in the statement.
    if engine == EngineKind::Postgres {
        check_unicode_names(tokens)?;
    }
    check_names(engine, &sig)?;
    // 8. The parse.
    let Some([statement]) = parsed else {
        return Err(unparsed());
    };
    // 9. The statement kinds that run.
    classify(engine, statement)
}

fn transaction_control() -> Refusal {
    Refusal::not_allowed(
        "transaction control (BEGIN, COMMIT, ROLLBACK, SAVEPOINT, ...) never runs: each call runs \
         in its own transaction",
        Some(HINT_TRANSACTION_CONTROL),
    )
}

fn session_statement() -> Refusal {
    Refusal::not_allowed(
        "SET (also SET LOCAL and SET ROLE), RESET, DISCARD and set_config never run: the \
         platform's options hold the limits of the call",
        Some(
            "leave the option out; the limits of one call go in the request (timeout_ms, max_rows)",
        ),
    )
}

/// Postgres reads `U&"..."` (optionally with `UESCAPE '<char>'`) as an identifier with Unicode
/// escapes, so `U&"set\005fconfig"` is `set_config`. The parser keeps such a name as the word
/// `U`, the operator `&` and a quoted word with the escapes still in it, so the word rules below
/// would compare the escaped text. Refuse every such name (fail closed). The check runs on the
/// full token list: with whitespace or a comment between `U`, `&` and the quoted word, Postgres
/// reads a column `U` and the operator `&`, not an escaped name.
fn check_unicode_names(tokens: &[Token]) -> Result<(), Refusal> {
    for w in tokens.windows(3) {
        let [Token::Word(u), Token::Ampersand, name] = w else {
            continue;
        };
        let quoted = match name {
            Token::Word(n) => n.quote_style == Some('"'),
            Token::DoubleQuotedString(_) => true,
            _ => false,
        };
        if u.quote_style.is_none() && u.value.eq_ignore_ascii_case("u") && quoted {
            return Err(Refusal::not_allowed(
                "names with Unicode escapes (U&\"...\") never run: the server checks names by \
                 their text",
                Some("write the name without Unicode escapes"),
            ));
        }
    }
    Ok(())
}

/// The rules on words: `set_config`, `ruum_*`, `public.` / `main.`, and the SQLite file
/// functions.
fn check_names(engine: EngineKind, sig: &[&Token]) -> Result<(), Refusal> {
    let own_schema = match engine {
        EngineKind::Postgres => "public",
        EngineKind::Sqlite => "main",
    };
    for (i, t) in sig.iter().enumerate() {
        let Token::Word(w) = t else { continue };
        let lower = w.value.to_ascii_lowercase();
        if lower == "set_config" {
            return Err(session_statement());
        }
        if lower.starts_with("ruum_") {
            return Err(Refusal::not_allowed(
                "names that start with ruum_ belong to the platform and never run in a statement",
                Some("use the names of the database's own tables, views and columns"),
            ));
        }
        if engine == EngineKind::Sqlite
            && matches!(lower.as_str(), "load_extension" | "readfile" | "writefile")
        {
            return Err(Refusal::not_allowed(
                format!("{lower} never runs"),
                Some("leave the function out"),
            ));
        }
        if lower == own_schema && matches!(sig.get(i + 1), Some(Token::Period)) {
            return Err(Refusal::not_allowed(
                format!(
                    "write the names of the database's tables without the schema ({own_schema}. \
                     is not allowed; the server never rewrites a name)"
                ),
                Some(HINT_UNQUALIFIED_NAMES),
            ));
        }
    }
    Ok(())
}

fn not_allowed_kind() -> Refusal {
    Refusal::not_allowed(
        "this kind of statement never runs through the query API (roles, grants, schemas, \
         databases, extensions, session and file statements)",
        Some("run queries, INSERT, UPDATE, DELETE or schema changes of tables, views and indexes"),
    )
}

/// The statement kinds that run (rule 9).
fn classify(engine: EngineKind, statement: &Statement) -> Result<StatementKind, Refusal> {
    use StatementKind::*;
    Ok(match statement {
        // `SELECT ... INTO` makes a table: a schema change (review Q-1, N-2).
        Statement::Query(q) if query_has_into(q) => SchemaChange,
        Statement::Query(_) => Read,
        Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Truncate(_) => Write,
        Statement::Explain {
            statement,
            analyze,
            options,
            ..
        } => {
            // `EXPLAIN (ANALYZE)` and `EXPLAIN (ANALYZE <any value>)` count as ANALYZE (fail
            // closed; review Q-1, N-1).
            let analyze = *analyze
                || options.as_ref().is_some_and(|o| {
                    o.iter()
                        .any(|opt| opt.name.value.eq_ignore_ascii_case("analyze"))
                });
            match classify(engine, statement)? {
                // EXPLAIN without ANALYZE plans the statement but does not run it.
                _ if !analyze => Read,
                kind => kind,
            }
        }
        Statement::CreateTable(_)
        | Statement::CreateView(_)
        | Statement::CreateIndex(_)
        | Statement::CreateVirtualTable { .. }
        | Statement::AlterTable(_)
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterType(_)
        | Statement::CreateFunction(_)
        | Statement::DropFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::DropTrigger(_)
        | Statement::CreateSequence { .. }
        | Statement::CreateType { .. }
        | Statement::CreateDomain(_)
        | Statement::DropDomain(_)
        | Statement::Comment { .. }
        | Statement::RenameTable(_) => SchemaChange,
        Statement::Drop {
            object_type:
                ObjectType::Table
                | ObjectType::View
                | ObjectType::MaterializedView
                | ObjectType::Index
                | ObjectType::Sequence
                | ObjectType::Type,
            ..
        } => SchemaChange,
        Statement::Pragma { name, is_eq, .. } if engine == EngineKind::Sqlite => {
            let allowed = !is_eq
                && name.0.len() == 1
                && name.0[0].as_ident().is_some_and(|i| {
                    SQLITE_PRAGMA_ALLOW_LIST.contains(&i.value.to_ascii_lowercase().as_str())
                });
            if !allowed {
                return Err(Refusal::not_allowed(
                    format!(
                        "only the read-only PRAGMAs run: {}",
                        SQLITE_PRAGMA_ALLOW_LIST.join(", ")
                    ),
                    Some("use one of the read-only PRAGMAs, without a value"),
                ));
            }
            Read
        }
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::ReleaseSavepoint { .. } => return Err(transaction_control()),
        Statement::Set(_) => return Err(session_statement()),
        Statement::Reset(_) | Statement::Discard { .. } => return Err(session_statement()),
        _ => return Err(not_allowed_kind()),
    })
}

/// Whether a query (or one of its set operations) has `INTO`.
fn query_has_into(q: &Query) -> bool {
    fn set_has_into(s: &SetExpr) -> bool {
        match s {
            SetExpr::Select(select) => select.into.is_some(),
            SetExpr::Query(q) => query_has_into(q),
            SetExpr::SetOperation { left, right, .. } => set_has_into(left) || set_has_into(right),
            _ => false,
        }
    }
    set_has_into(&q.body)
}

/// Whether a token is a literal (a value written into the statement).
///
/// On SQLite a double-quoted word is a literal too: SQLite reads `"Alice"` as a string when no
/// column has that name (the DQS quirk), so the log cannot keep it (review Q-1, R-3). The log
/// then shows a double-quoted name as a marker as well; that loses a name, never a value.
fn is_literal(engine: EngineKind, t: &Token) -> bool {
    if engine == EngineKind::Sqlite
        && let Token::Word(w) = t
        && w.quote_style == Some('"')
    {
        return true;
    }
    matches!(
        t,
        Token::Number(..)
            | Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::TripleSingleQuotedString(_)
            | Token::TripleDoubleQuotedString(_)
            | Token::DollarQuotedString(_)
            | Token::SingleQuotedByteStringLiteral(_)
            | Token::DoubleQuotedByteStringLiteral(_)
            | Token::TripleSingleQuotedByteStringLiteral(_)
            | Token::TripleDoubleQuotedByteStringLiteral(_)
            | Token::SingleQuotedRawStringLiteral(_)
            | Token::DoubleQuotedRawStringLiteral(_)
            | Token::TripleSingleQuotedRawStringLiteral(_)
            | Token::TripleDoubleQuotedRawStringLiteral(_)
            | Token::NationalStringLiteral(_)
            | Token::QuoteDelimitedStringLiteral(_)
            | Token::NationalQuoteDelimitedStringLiteral(_)
            | Token::EscapedStringLiteral(_)
            | Token::UnicodeStringLiteral(_)
            | Token::HexStringLiteral(_)
    )
}

/// The normalized statement: every literal token becomes `$L1`, `$L2`, ... in order; comments
/// and runs of whitespace become one space.
///
/// A number directly followed by a word (no whitespace) is one literal: Postgres 16+ reads
/// `0o17` and `0b101` as numbers, while the tokenizer splits them into the number `0` and the
/// words `o17` and `b101` (review Q-1, R-2). Postgres refuses every other such case ("trailing
/// junk after numeric literal"), so the merge hides no name.
fn normalize_tokens(engine: EngineKind, tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut space = false;
    let mut iter = tokens.iter().peekable();
    while let Some(t) = iter.next() {
        match t {
            Token::EOF => {}
            Token::Whitespace(_) => space = true,
            t => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                if is_literal(engine, t) {
                    n += 1;
                    out.push_str(&format!("$L{n}"));
                    if matches!(t, Token::Number(..)) {
                        while matches!(iter.peek(), Some(Token::Word(_) | Token::Number(..))) {
                            iter.next();
                        }
                    }
                } else {
                    out.push_str(&t.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: EngineKind = EngineKind::Postgres;
    const LITE: EngineKind = EngineKind::Sqlite;
    const BOTH: [EngineKind; 2] = [PG, LITE];

    fn kind(engine: EngineKind, sql: &str) -> StatementKind {
        match analyze(engine, sql).verdict {
            Ok(k) => k,
            Err(r) => panic!("{engine:?}: {sql:?} was refused: {r:?}"),
        }
    }

    fn refused(engine: EngineKind, sql: &str) -> Refusal {
        match analyze(engine, sql).verdict {
            Ok(k) => panic!("{engine:?}: {sql:?} was allowed as {k:?}"),
            Err(r) => r,
        }
    }

    fn not_allowed(engine: EngineKind, sql: &str) -> Refusal {
        let r = refused(engine, sql);
        assert_eq!(
            r.code,
            RefusalCode::StatementNotAllowed,
            "{engine:?}: {sql:?}: {r:?}"
        );
        r
    }

    #[test]
    fn plain_statements_run() {
        for e in BOTH {
            assert_eq!(kind(e, "select count(*) from plants"), StatementKind::Read);
            assert_eq!(kind(e, "select 1;"), StatementKind::Read);
            assert_eq!(
                kind(e, "with x as (select id from plants) select * from x"),
                StatementKind::Read
            );
            assert_eq!(
                kind(e, "insert into plants (id) values (1)"),
                StatementKind::Write
            );
            assert_eq!(
                kind(e, "update plants set name = 'x' where id = 1"),
                StatementKind::Write
            );
            assert_eq!(kind(e, "delete from plants"), StatementKind::Write);
            assert_eq!(
                kind(e, "create table beds (id integer primary key)"),
                StatementKind::SchemaChange
            );
            assert_eq!(
                kind(e, "alter table plants add column notes text"),
                StatementKind::SchemaChange
            );
            assert_eq!(kind(e, "drop table beds"), StatementKind::SchemaChange);
            assert_eq!(
                kind(e, "create index plants_name on plants (name)"),
                StatementKind::SchemaChange
            );
            assert_eq!(kind(e, "explain select * from plants"), StatementKind::Read);
        }
        assert_eq!(
            kind(PG, "alter table plants alter column id type bigint"),
            StatementKind::SchemaChange
        );
        assert_eq!(kind(PG, "truncate plants"), StatementKind::Write);
        assert_eq!(
            kind(PG, "select * from pg_catalog.pg_tables"),
            StatementKind::Read
        );
        assert_eq!(
            kind(PG, "select table_name from information_schema.tables"),
            StatementKind::Read
        );
        assert_eq!(
            kind(PG, "select extensions.st_astext(geom) from places"),
            StatementKind::Read
        );
        assert_eq!(
            kind(PG, "select * from plants where id = $1"),
            StatementKind::Read
        );
        assert_eq!(
            kind(LITE, "select * from plants where id = ?1"),
            StatementKind::Read
        );
    }

    #[test]
    fn exactly_one_statement() {
        for e in BOTH {
            let r = refused(e, "select 1; select 2");
            assert_eq!(r.code, RefusalCode::ValidationFailed);
            assert!(r.message.contains('2'), "{r:?}");
            let r = refused(e, "select 1; delete from plants");
            assert_eq!(r.code, RefusalCode::ValidationFailed);
            for empty in ["", "   ", ";", "-- nothing"] {
                assert_eq!(refused(e, empty).code, RefusalCode::ValidationFailed);
            }
        }
        // Also when the parse fails: the semicolons count.
        let r = refused(PG, "do $$ begin end $$; select 1");
        assert_eq!(r.code, RefusalCode::ValidationFailed);
    }

    #[test]
    fn transaction_control_never_runs() {
        for e in BOTH {
            for sql in [
                "begin",
                "BEGIN TRANSACTION",
                "start transaction",
                "commit",
                "end",
                "rollback",
                "abort",
                "savepoint a",
                "release savepoint a",
                "rollback to savepoint a",
                "prepare transaction 'x'",
                "commit prepared 'x'",
                "rollback prepared 'x'",
            ] {
                let r = not_allowed(e, sql);
                assert_eq!(
                    r.hint.as_deref(),
                    Some(HINT_TRANSACTION_CONTROL),
                    "{e:?}: {sql}"
                );
            }
        }
    }

    #[test]
    fn session_statements_never_run() {
        for e in BOTH {
            for sql in [
                "set statement_timeout = 0",
                "set local statement_timeout = 0",
                "SET LOCAL work_mem = '1GB'",
                "set role r_0123456789abcdef",
                "set session authorization r_0123456789abcdef",
                "reset role",
                "reset all",
                "reset statement_timeout",
                "discard all",
                "select set_config('role', 'x', false)",
                "select set_config('statement_timeout', '0', true)",
                "select pg_catalog.set_config('search_path', 'public', true)",
                "select * from plants where set_config('x', 'y', true) is not null",
            ] {
                let r = not_allowed(e, sql);
                assert_ne!(r.hint.as_deref(), Some(HINT_TRANSACTION_CONTROL));
            }
        }
    }

    #[test]
    fn role_and_platform_statements_never_run() {
        for e in BOTH {
            for sql in [
                "create role x",
                "alter role current_user set search_path = public",
                "drop role x",
                "create user x",
                "grant r_0123456789abcdef to current_user",
                "grant select on plants to x",
                "revoke select on plants from x",
                "create schema x",
                "drop schema x",
                "create database x",
                "drop database x",
                "create extension pgcrypto",
                "copy plants to stdout",
                "call p()",
                "listen x",
                "vacuum",
            ] {
                not_allowed(e, sql);
            }
        }
    }

    #[test]
    fn do_blocks_never_run() {
        // Postgres: a DO block does not parse, so it is refused (fail closed).
        not_allowed(PG, "do $$ begin perform 1; end $$");
        not_allowed(
            PG,
            "do $$ begin perform set_config('role', 'x', false); end $$",
        );
    }

    #[test]
    fn ruum_names_never_run() {
        for e in BOTH {
            for sql in [
                "select * from ruum_mask.key",
                "select * from ruum_masked.plants",
                "select * from ruum_nosecret.plants",
                "select ruum_pseudonym(name) from plants",
                "select * from ruum_roles",
                "select * from \"ruum_meta\".databases",
            ] {
                not_allowed(e, sql);
            }
        }
    }

    /// Review Q-1, R-1: Postgres reads `U&"set\005fconfig"` as `set_config`.
    #[test]
    fn unicode_escaped_names_never_run() {
        for sql in [
            r#"select U&"set\005fconfig"('statement_timeout', '0', false)"#,
            r#"select u&"set\005fconfig"('work_mem', '7MB', true)"#,
            r#"select U&"set!005fconfig" UESCAPE '!' ('work_mem', '7MB', true)"#,
            r#"select * from U&"\0070ublic".plants"#,
            r#"select * from U&"\0072uum_meta".databases"#,
            r#"select * from U&"plants""#,
            r#"select U&"name" from plants"#,
        ] {
            let r = not_allowed(PG, sql);
            assert_eq!(
                r.hint.as_deref(),
                Some("write the name without Unicode escapes"),
                "{sql}"
            );
        }
        // A Unicode string literal is a value, not a name.
        assert_eq!(kind(PG, r"select U&'d\0061t\+000061'"), StatementKind::Read);
        // With whitespace, `u` is a column and `&` the bitwise and.
        assert_eq!(
            kind(PG, r#"select u & "id" from plants"#),
            StatementKind::Read
        );
    }

    /// Review Q-1, N-1 and N-2.
    #[test]
    fn explain_options_and_select_into_are_classified_by_what_they_do() {
        assert_eq!(
            kind(PG, "explain (analyze) delete from plants"),
            StatementKind::Write
        );
        assert_eq!(
            kind(PG, "explain (analyze true) delete from plants"),
            StatementKind::Write
        );
        assert_eq!(
            kind(PG, "explain (verbose) delete from plants"),
            StatementKind::Read
        );
        assert_eq!(
            kind(PG, "select * into plants2 from plants"),
            StatementKind::SchemaChange
        );
        assert_eq!(
            kind(
                PG,
                "select id into plants3 from plants union select id from plants"
            ),
            StatementKind::SchemaChange
        );
    }

    #[test]
    fn qualified_names_are_refused_and_never_rewritten() {
        for sql in [
            "select count(*) from public.plants",
            "select count(*) from PUBLIC.plants",
            "select count(*) from \"public\".plants",
            "select count(*) from public . plants",
            "insert into public.plants (id) values (1)",
            "create table public.beds (id integer)",
            "select public.plants.id from plants",
        ] {
            let r = not_allowed(PG, sql);
            assert_eq!(r.hint.as_deref(), Some(HINT_UNQUALIFIED_NAMES), "{sql}");
        }
        for sql in [
            "select count(*) from main.plants",
            "select count(*) from MAIN.plants",
            "delete from main.plants",
            "create table main.beds (id integer)",
        ] {
            let r = not_allowed(LITE, sql);
            assert_eq!(r.hint.as_deref(), Some(HINT_UNQUALIFIED_NAMES), "{sql}");
        }
        // A word or a string `public` that is not a schema is fine.
        assert_eq!(
            kind(PG, "select 'public.plants' as public from plants"),
            StatementKind::Read
        );
        assert_eq!(kind(LITE, "select 'main.plants'"), StatementKind::Read);
    }

    #[test]
    fn sqlite_file_statements_never_run() {
        for sql in [
            "attach database 'other.db' as other",
            "detach database other",
            "vacuum",
            "vacuum into 'x.db'",
            "select load_extension('x')",
            "select readfile('/etc/passwd')",
            "select writefile('x', 'y')",
            "pragma writable_schema = 1",
            "pragma journal_mode = delete",
            "pragma table_info = 1",
            "pragma key = 'x'",
        ] {
            not_allowed(LITE, sql);
        }
        for sql in [
            "pragma table_info('plants')",
            "PRAGMA index_list('plants')",
            "pragma foreign_key_list('plants')",
        ] {
            assert_eq!(kind(LITE, sql), StatementKind::Read, "{sql}");
        }
        // The parser takes a PRAGMA value only as a string or a number; a bare name does not
        // parse, so it is refused (fail closed).
        not_allowed(LITE, "pragma table_info(plants)");
        // PRAGMA is a SQLite statement only.
        not_allowed(PG, "pragma table_info('plants')");
    }

    #[test]
    fn unparsed_statements_are_refused() {
        for e in BOTH {
            for sql in ["selec 1", "select * from (", "select 'unterminated"] {
                let a = analyze(e, sql);
                assert_eq!(a.normalized, UNPARSED, "{e:?}: {sql}");
                assert_eq!(
                    a.verdict.unwrap_err().code,
                    RefusalCode::StatementNotAllowed
                );
            }
        }
    }

    #[test]
    fn deep_statements_neither_crash_nor_run() {
        // A long chain of operators: the parse and the drop recurse.
        let chain = format!("select {}", vec!["1"; 40_000].join("+"));
        let a = analyze(PG, &chain);
        assert!(!a.normalized.contains("+1"));
        let nested = format!("select {}1{}", "(".repeat(5_000), ")".repeat(5_000));
        assert!(analyze(PG, &nested).verdict.is_err());
    }

    /// The normalized statement holds no literal of the statement as sent.
    #[test]
    fn the_normalizer_leaves_no_literal() {
        let cases: &[(EngineKind, &str, &str)] = &[
            (
                PG,
                "select * from people where email = 'alice@example.test' and age > 42",
                "select * from people where email = $L1 and age > $L2",
            ),
            (
                PG,
                "select $tag$secret dollar$tag$, $$another secret$$",
                "select $L1, $L2",
            ),
            (
                PG,
                "select E'escaped\\nsecret', U&'unicode secret', X'deadbeef', B'0101'",
                "",
            ),
            (PG, "select 3.14159, -7, 1e10, .5", ""),
            (
                PG,
                "select array[11, 12], '{13,14}'::int[] from t",
                "select array[$L1, $L2], $L3::int[] from t",
            ),
            (
                PG,
                "select 1 -- comment with secret-in-comment\n/* block secret-in-block */ from t",
                "select $L1 from t",
            ),
            (
                PG,
                "select * from t where name = 'it''s secret' and id = $1",
                "select * from t where name = $L1 and id = $1",
            ),
            (
                LITE,
                "select * from t where name = 'lite secret' and id = ?1 and b = x'cafe'",
                "select * from t where name = $L1 and id = ?1 and b = $L2",
            ),
            (
                PG,
                "insert into t (a, b) values ('v1 secret', 99), ('v2 secret', 100)",
                "insert into t (a, b) values ($L1, $L2), ($L3, $L4)",
            ),
            (
                PG,
                "select interval '3 days', date '2026-01-02', timestamptz '2026-01-02 03:04:05+00'",
                "",
            ),
            // Review Q-1, R-2: Postgres 16+ integer literals with a prefix.
            (
                PG,
                "select 0o17, 0b101, 0x1F, 1_000_000",
                "select $L1, $L2, $L3, $L4",
            ),
            // Review Q-1, R-3: SQLite reads a double-quoted word as a string (DQS).
            (
                LITE,
                "select \"Alice\" from plants where latin_name = \"Alice\"",
                "select $L1 from plants where latin_name = $L2",
            ),
        ];
        let secrets = [
            "alice", "42", "secret", "deadbeef", "0101", "3.14159", "7", "1e10", ".5", "11", "12",
            "13", "14", "99", "100", "3 days", "2026", "cafe", "o17", "b101", "x1F", "000",
            "Alice",
        ];
        for (engine, sql, expected) in cases {
            let a = analyze(*engine, sql);
            assert_ne!(a.normalized, UNPARSED, "{sql}");
            if !expected.is_empty() {
                assert_eq!(&a.normalized, expected, "{sql}");
            }
            for s in secrets {
                assert!(
                    !a.normalized.contains(s),
                    "{sql:?} -> {:?} still holds {s:?}",
                    a.normalized
                );
            }
            assert!(
                a.normalized.contains("$L1"),
                "{sql:?} -> {:?}",
                a.normalized
            );
        }
    }

    /// A prefixed integer where the parser fails or reads an alias still leaves no digits.
    #[test]
    fn prefixed_integers_leave_no_digits() {
        for sql in [
            "select * from plants where pin = 0b1011",
            "select 0o7531 as x",
            "select * from plants where pin = 0o7531 and id = 0x2A",
        ] {
            let a = analyze(PG, sql);
            for s in ["1011", "7531", "2A"] {
                assert!(!a.normalized.contains(s), "{sql:?} -> {:?}", a.normalized);
            }
        }
    }

    #[test]
    fn a_refused_statement_is_normalized_too() {
        let a = analyze(
            PG,
            "select * from public.users where email = 'bob@example.test'",
        );
        assert!(a.verdict.is_err());
        assert_eq!(a.normalized, "select * from public.users where email = $L1");
    }
}
