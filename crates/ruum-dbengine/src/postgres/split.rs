//! A small statement splitter with the lexer rules of PostgreSQL (and of `psql`), and the
//! classification of transaction-control statements.
//!
//! It knows string literals (`'...'` with `''`, `E'...'` with backslash escapes), quoted
//! identifiers (`"..."`), dollar quotes (`$tag$...$tag$`), line comments (`-- ...`) and nested
//! block comments (`/* ... */`). Like `psql`, it does not split inside the body of a
//! `CREATE FUNCTION ... BEGIN ATOMIC ... END` (it counts `BEGIN` / `CASE` and `END` inside a
//! `CREATE` statement).
//!
//! It is the engine's first line against transaction control in a migration's `up` and in a
//! query (decision 0004-34 d, review-E-1 N-2): every statement is classified BEFORE anything
//! runs. The second line is the check after each statement that the transaction is still open.
//! A splitter that misses a semicolon cannot hide a statement: the engine sends each piece with
//! the extended protocol, and the server refuses more than one command in it. Card Q-1 replaces
//! this first line with the parser of the query layer (`sqlparser`, once approved).

/// One statement of a script: its text without the final semicolon, trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Piece<'a> {
    pub text: &'a str,
    /// The byte offset just after the statement's semicolon (or the end of the input).
    pub end: usize,
}

/// The statements of `sql`, without empty ones (only blanks and comments).
pub(crate) fn split(sql: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(piece) = next(sql, pos) {
        pos = piece.end;
        if !piece.text.is_empty() {
            out.push(piece.text);
        }
    }
    out
}

/// The next statement of `sql` from byte offset `pos`, or `None` at the end. The text may be
/// empty (a `;` alone, or only comments).
pub(crate) fn next(sql: &str, pos: usize) -> Option<Piece<'_>> {
    if pos >= sql.len() {
        return None;
    }
    let bytes = sql.as_bytes();
    let mut i = pos;
    let mut words = 0usize;
    let mut is_create = false;
    let mut depth = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b';' if depth == 0 => {
                return Some(Piece {
                    text: strip(&sql[pos..i]),
                    end: i + 1,
                });
            }
            b'\'' => i = skip_quoted(bytes, i, b'\'', false),
            b'"' => i = skip_quoted(bytes, i, b'"', false),
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b'$' => match dollar_tag(bytes, i) {
                Some(tag_len) => i = skip_dollar(bytes, i, tag_len),
                None => i += 1,
            },
            _ if is_ident_start(c) => {
                let start = i;
                while i < bytes.len() && is_ident_char(bytes[i]) {
                    i += 1;
                }
                let word = &sql[start..i];
                // E'...' (also e'...'): a string with backslash escapes.
                if (word == "E" || word == "e") && bytes.get(i) == Some(&b'\'') {
                    i = skip_quoted(bytes, i, b'\'', true);
                    continue;
                }
                if words == 0 {
                    is_create = word.eq_ignore_ascii_case("create");
                }
                words += 1;
                if is_create {
                    if word.eq_ignore_ascii_case("begin")
                        || (depth > 0 && word.eq_ignore_ascii_case("case"))
                    {
                        depth += 1;
                    } else if depth > 0 && word.eq_ignore_ascii_case("end") {
                        depth -= 1;
                    }
                }
                continue;
            }
            _ => i += 1,
        }
    }
    Some(Piece {
        text: strip(&sql[pos..]),
        end: sql.len(),
    })
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == b'$'
}

/// Skips a quoted string or identifier that starts at `i` (the quote). A doubled quote is part
/// of it; with `backslash`, `\x` escapes one character. Returns the offset after the closing
/// quote (or the end of the input).
fn skip_quoted(bytes: &[u8], i: usize, quote: u8, backslash: bool) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        let c = bytes[j];
        if backslash && c == b'\\' {
            j += 2;
            continue;
        }
        if c == quote {
            if bytes.get(j + 1) == Some(&quote) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_block_comment(bytes: &[u8], i: usize) -> usize {
    let mut depth = 0usize;
    let mut j = i;
    while j < bytes.len() {
        if bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// The length of the dollar-quote tag at `i` (`$$` or `$tag$`), if one starts there. A `$`
/// followed by a digit is a parameter (`$1`), not a tag.
fn dollar_tag(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if let Some(&c) = bytes.get(j)
        && c != b'$'
        && !(c.is_ascii_alphabetic() || c == b'_' || c >= 0x80)
    {
        return None;
    }
    while j < bytes.len() {
        let c = bytes[j];
        if c == b'$' {
            return Some(j + 1 - i);
        }
        if !(c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80) {
            return None;
        }
        j += 1;
    }
    None
}

fn skip_dollar(bytes: &[u8], i: usize, tag_len: usize) -> usize {
    let tag = &bytes[i..i + tag_len];
    let mut j = i + tag_len;
    while j + tag_len <= bytes.len() {
        if &bytes[j..j + tag_len] == tag {
            return j + tag_len;
        }
        j += 1;
    }
    bytes.len()
}

/// The offset of the first byte from `pos` that is not a blank or inside a comment.
pub(crate) fn skip_blanks_and_comments(sql: &str, mut pos: usize) -> usize {
    let bytes = sql.as_bytes();
    loop {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if bytes[pos..].starts_with(b"--") {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
        } else if bytes[pos..].starts_with(b"/*") {
            pos = pos + skip_block_comment(&bytes[pos..], 0);
        } else {
            return pos;
        }
    }
}

/// Trims blanks and leading or trailing comments: an empty result means "no statement".
fn strip(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map(|(_, r)| r).unwrap_or("").trim();
        } else if s.starts_with("/*") {
            let end = skip_block_comment(s.as_bytes(), 0);
            s = s[end..].trim();
        } else {
            break;
        }
    }
    s
}

/// The first `n` words of a statement, lowercase, after comments.
fn leading_words(stmt: &str, n: usize) -> Vec<String> {
    stmt.split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .filter(|w| !w.is_empty())
        .take(n)
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Whether a statement is transaction control (decision 0004-34 d): `BEGIN`, `START
/// TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`, `SAVEPOINT`, `RELEASE`, `PREPARE
/// TRANSACTION`, `COMMIT PREPARED`, `ROLLBACK PREPARED`.
pub(crate) fn is_transaction_control(stmt: &str) -> bool {
    let words = leading_words(strip(stmt), 2);
    let first = words.first().map(String::as_str).unwrap_or("");
    let second = words.get(1).map(String::as_str).unwrap_or("");
    match first {
        "begin" | "commit" | "end" | "rollback" | "abort" | "savepoint" | "release" => true,
        "start" => second == "transaction",
        "prepare" => second == "transaction",
        _ => false,
    }
}

/// Whether a statement changes the schema (DDL and grants), by its first words. The engine
/// refuses these for the kinds without schema rights before they run (the database refuses
/// them too: the second line).
pub(crate) fn is_schema_change(stmt: &str) -> bool {
    let words = leading_words(strip(stmt), 2);
    let first = words.first().map(String::as_str).unwrap_or("");
    let second = words.get(1).map(String::as_str).unwrap_or("");
    matches!(
        first,
        "create"
            | "alter"
            | "drop"
            | "grant"
            | "revoke"
            | "comment"
            | "reindex"
            | "cluster"
            | "refresh"
            | "reassign"
            | "vacuum"
            | "analyze"
            | "analyse"
    ) || (first == "security" && second == "label")
        || (first == "import" && second == "foreign")
}

/// Whether a statement writes rows, by its first word (`INSERT`, `UPDATE`, `DELETE`, `MERGE`).
pub(crate) fn is_row_write(stmt: &str) -> bool {
    let words = leading_words(strip(stmt), 1);
    matches!(
        words.first().map(String::as_str),
        Some("insert" | "update" | "delete" | "merge")
    )
}

/// Whether a statement is `COPY ... FROM STDIN` (the data follows the statement, in a dump).
pub(crate) fn is_copy_from_stdin(stmt: &str) -> bool {
    let lower = strip(stmt).to_ascii_lowercase();
    lower.starts_with("copy ") && lower.trim_end().ends_with("from stdin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons_only() {
        let sql = "CREATE TABLE a (x text DEFAULT 'a;b'); -- c;d\n\
                   INSERT INTO \"we;ird\" VALUES (E'it\\'s;', $$x;y$$, $q$z$$;$q$);\n\
                   /* one /* nested; */ comment; */ SELECT $1;;  ";
        assert_eq!(
            split(sql),
            [
                "CREATE TABLE a (x text DEFAULT 'a;b')",
                "INSERT INTO \"we;ird\" VALUES (E'it\\'s;', $$x;y$$, $q$z$$;$q$)",
                "SELECT $1",
            ]
        );
        assert_eq!(
            split("  ; -- only a comment\n /* x */ ;"),
            Vec::<&str>::new()
        );
        assert_eq!(split("SELECT 'it''s; fine'"), ["SELECT 'it''s; fine'"]);
    }

    #[test]
    fn keeps_a_begin_atomic_body_together() {
        let sql = "CREATE FUNCTION f(x int) RETURNS int LANGUAGE sql BEGIN ATOMIC \
                   SELECT CASE WHEN x > 0 THEN 1 ELSE 2 END; SELECT 3; END; COMMIT;";
        let pieces = split(sql);
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        assert!(pieces[0].ends_with("SELECT 3; END"));
        assert!(!is_transaction_control(pieces[0]));
        assert!(is_transaction_control(pieces[1]));
    }

    #[test]
    fn classifies_transaction_control() {
        for stmt in [
            "COMMIT",
            "commit work",
            "BEGIN",
            "begin isolation level serializable",
            "START TRANSACTION",
            "END",
            "ROLLBACK",
            "rollback to savepoint s",
            "ABORT",
            "SAVEPOINT s1",
            "RELEASE s1",
            "PREPARE TRANSACTION 'x'",
            "COMMIT PREPARED 'x'",
            "ROLLBACK PREPARED 'x'",
            "  -- a comment\n  /* and */ COMMIT",
        ] {
            assert!(is_transaction_control(stmt), "{stmt}");
        }
        for stmt in [
            "SELECT 1",
            "CREATE TABLE begin_x (x int)",
            "PREPARE p AS SELECT 1",
            "START",
            "DO $$ BEGIN COMMIT; END $$",
            "UPDATE t SET end_at = now()",
        ] {
            assert!(!is_transaction_control(stmt), "{stmt}");
        }
    }

    #[test]
    fn classifies_schema_changes_and_row_writes() {
        for stmt in [
            "CREATE TABLE t (x int)",
            "drop table users",
            "ALTER TABLE t ADD c int",
            "GRANT SELECT ON t TO r",
            "security label on table t is 'x'",
            "ANALYZE t",
        ] {
            assert!(is_schema_change(stmt), "{stmt}");
        }
        for stmt in [
            "SELECT 1",
            "INSERT INTO t VALUES (1)",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "DO $$ BEGIN END $$",
        ] {
            assert!(!is_schema_change(stmt), "{stmt}");
        }
        assert!(is_row_write("  insert into t values (1)"));
        assert!(is_row_write("DELETE FROM t"));
        assert!(!is_row_write("SELECT 1"));
    }

    #[test]
    fn skips_blanks_and_comments() {
        let sql = "  -- a\n /* b */\n\\restrict x\nSELECT 1;";
        let at = skip_blanks_and_comments(sql, 0);
        assert!(sql[at..].starts_with("\\restrict"));
    }

    #[test]
    fn finds_copy_from_stdin() {
        assert!(is_copy_from_stdin(
            "COPY public.users (id, name) FROM stdin"
        ));
        assert!(!is_copy_from_stdin("COPY users TO stdout"));
        assert!(!is_copy_from_stdin("SELECT 'COPY x FROM stdin'"));
    }

    #[test]
    fn next_reports_offsets() {
        let sql = "SELECT 1;\nCOPY t FROM stdin;\n1\n\\.\nSELECT 2";
        let a = next(sql, 0).unwrap();
        assert_eq!(a.text, "SELECT 1");
        let b = next(sql, a.end).unwrap();
        assert_eq!(b.text, "COPY t FROM stdin");
        assert_eq!(&sql[b.end..b.end + 1], "\n");
    }
}
