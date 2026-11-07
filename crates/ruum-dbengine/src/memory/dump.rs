//! SQL dumps of a memory database (`.dump` style): the export, restore points and copies.
//!
//! A dump holds the user's schema and (optionally) rows of the schema `main`: no `ruum_*`
//! object (the memory engine's stand-in for the Ruum schemas and the roles), no TEMP object
//! (the lens views of card Q-3), no grant, and never a key (decision 0004-29).

use rusqlite::Connection;
use rusqlite::types::ValueRef;
use std::fmt::Write as _;

/// Whether a name belongs to the Ruum objects of a database.
pub(crate) fn is_ruum_name(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("ruum_")
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            if f.is_finite() {
                // `{:?}` is the shortest form that reads back exactly.
                format!("{f:?}")
            } else if f > 0.0 {
                "9e999".to_string()
            } else if f < 0.0 {
                "-9e999".to_string()
            } else {
                "NULL".to_string()
            }
        }
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => format!("'{}'", text.replace('\'', "''")),
            _ => format!("CAST(X'{}' AS TEXT)", hex::encode(bytes)),
        },
        ValueRef::Blob(bytes) => format!("X'{}'", hex::encode(bytes)),
    }
}

/// The objects of `main` in creation order: (type, name, table name, sql).
fn objects(conn: &Connection) -> rusqlite::Result<Vec<(String, String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT type, name, tbl_name, sql FROM main.sqlite_schema \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, name, table, sql): (String, String, String, String) = row?;
        if is_ruum_name(&name) || is_ruum_name(&table) {
            continue;
        }
        out.push((kind, name, table, sql));
    }
    Ok(out)
}

/// The insertable columns of a table (no hidden or generated column).
fn insert_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_xinfo(?1, 'main') WHERE hidden = 0 ORDER BY cid")?;
    let rows = stmt.query_map([table], |r| r.get::<_, String>(0))?;
    rows.collect()
}

fn dump_rows(conn: &Connection, table: &str, out: &mut String) -> rusqlite::Result<()> {
    let columns = insert_columns(conn, table)?;
    if columns.is_empty() {
        return Ok(());
    }
    let list = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!("SELECT {list} FROM main.{}", quote_ident(table)))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let mut values = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            values.push(literal(row.get_ref(i)?));
        }
        let _ = writeln!(
            out,
            "INSERT INTO {}({list}) VALUES({});",
            quote_ident(table),
            values.join(",")
        );
    }
    Ok(())
}

/// A dump of the schema `main`, with rows when `with_rows`.
pub(crate) fn dump(conn: &Connection, with_rows: bool) -> rusqlite::Result<String> {
    let objects = objects(conn)?;
    let mut out = String::from("PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n");
    for (_, name, _, sql) in objects.iter().filter(|o| o.0 == "table") {
        let _ = writeln!(out, "{sql};");
        if with_rows {
            dump_rows(conn, name, &mut out)?;
        }
    }
    if with_rows {
        let has_sequence: bool = conn.query_row(
            "SELECT count(*) > 0 FROM main.sqlite_schema WHERE name = 'sqlite_sequence'",
            [],
            |r| r.get(0),
        )?;
        if has_sequence {
            let mut stmt = conn.prepare("SELECT name, seq FROM main.sqlite_sequence")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            let mut lines = String::new();
            for row in rows {
                let (name, seq) = row?;
                if !is_ruum_name(&name) {
                    let _ = writeln!(
                        lines,
                        "INSERT INTO sqlite_sequence(name,seq) VALUES({},{seq});",
                        literal(ValueRef::Text(name.as_bytes()))
                    );
                }
            }
            if !lines.is_empty() {
                out.push_str("DELETE FROM sqlite_sequence;\n");
                out.push_str(&lines);
            }
        }
    }
    for (_, _, _, sql) in objects.iter().filter(|o| o.0 != "table") {
        let _ = writeln!(out, "{sql};");
    }
    out.push_str("COMMIT;\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_read_back_exactly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t(a, b, c, d, e);
             INSERT INTO t VALUES(NULL, -9223372036854775808, 0.1, 'it''s', X'00ff');
             INSERT INTO t VALUES(1e300, -0.0, 9e999, CAST(X'610062' AS TEXT), '');",
        )
        .unwrap();
        let text = dump(&conn, true).unwrap();
        let copy = Connection::open_in_memory().unwrap();
        copy.execute_batch(&text).unwrap();
        let read = |c: &Connection| -> Vec<String> {
            let mut stmt = c
                .prepare(
                    "SELECT quote(a), quote(b), quote(c), quote(d), quote(e) FROM t ORDER BY rowid",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((0..5)
                        .map(|i| r.get::<_, String>(i).unwrap())
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(read(&conn), read(&copy));
    }

    #[test]
    fn ruum_objects_stay_out() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE ruum_roles(kind, role_name);
             INSERT INTO ruum_roles VALUES('owner', 'r_0011223344556677');
             CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO users(name) VALUES('Ann');",
        )
        .unwrap();
        let text = dump(&conn, true).unwrap();
        assert!(text.contains("CREATE TABLE users"));
        assert!(text.contains("'Ann'"));
        assert!(!text.to_ascii_lowercase().contains("ruum_"));
        assert!(!text.contains("r_0011223344556677"));
        let schema = dump(&conn, false).unwrap();
        assert!(schema.contains("CREATE TABLE users"));
        assert!(!schema.contains("'Ann'"));
    }
}
