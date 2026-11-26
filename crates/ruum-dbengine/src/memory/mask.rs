//! Masked copies of the memory engine (card D-3b; decisions 0004-17 and 0004-25).
//!
//! A masked copy loads the source's dump with its rows into the new database and then changes
//! the columns of the plan in place, as the copy's platform step (guard `Trusted`):
//!
//! - `personal` ([`MaskAs::Pseudonym`]): each non-`NULL` value becomes its pseudonym under the
//!   SOURCE key, `p_` and 12 hex digits of HMAC-SHA-256 over the value's text form (the form of
//!   `ruum_mask.pseudonym` of the Postgres engine). SQLite keeps the declared type; the value is
//!   text. An `INTEGER PRIMARY KEY` (the row ID) cannot hold text: there the value becomes the
//!   48-bit number of the same 12 hex digits.
//! - `secret` ([`MaskAs::Null`]): each value that is not `NULL` or the empty string becomes
//!   `NULL`.
//!
//! The tenant's triggers are dropped before the change and made again after it (SQLite cannot
//! disable a trigger), and foreign keys are not enforced while the columns change, so both sides
//! of a foreign key can take their pseudonyms one after the other. The memory engine is a fake
//! for unit tests: it does not drop and rebuild keys. A change that a constraint refuses (a `NOT
//! NULL` secret column, a pseudonym collision in a key, a `CHECK`) fails the copy with
//! [`EngineError::Unsupported`], and the memory engine reports no left-out constraint.
//!
//! The memory engine still has no lens (`mod.rs`): a masked copy masks the stored rows only.

use super::conn::internal;
use crate::error::EngineError;
use crate::key::{KEY_LEN, PseudonymKey};
use crate::model::{Catalog, MaskAs, MaskPlan};
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use sha2::{Digest, Sha256};

/// The schema of a memory database (the catalog's name for it).
const SCHEMA: &str = "main";
/// The functions of the copy step. Registered for the step only, removed after it.
const FUNCTION: &str = "ruum_copy_pseudonym";
const FUNCTION_INT: &str = "ruum_copy_pseudonym_int";

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `p_` and the first 12 hex digits of HMAC-SHA-256(`key`, `text`).
pub(crate) fn pseudonym(key: &[u8; KEY_LEN], text: &[u8]) -> String {
    let mut block = [0u8; 64];
    block[..KEY_LEN].copy_from_slice(key);
    let ipad: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(text)
        .finalize();
    let outer = Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    format!("p_{}", &hex::encode(outer)[..12])
}

/// The pseudonym as a number: its 12 hex digits (48 bits), for a row ID column.
fn pseudonym_number(key: &[u8; KEY_LEN], text: &[u8]) -> i64 {
    i64::from_str_radix(&pseudonym(key, text)[2..], 16).unwrap_or(0)
}

/// The text form of a stored value (`None` for `NULL`).
fn text_form(value: ValueRef<'_>) -> Option<Vec<u8>> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string().into_bytes()),
        ValueRef::Real(f) => Some(format!("{f:?}").into_bytes()),
        ValueRef::Text(t) => Some(t.to_vec()),
        ValueRef::Blob(b) => Some(format!("\\x{}", hex::encode(b)).into_bytes()),
    }
}

/// Refuses the copy with [`EngineError::NotReady`] when the columns of the copy (the source as
/// it was dumped) are not the columns the plan was made from (`MaskPlan::planned_from`): a
/// migration changed the source in between, so the plan may miss a column. The memory engine
/// has no column identity, so names are compared.
fn check_planned_from(catalog: &Catalog, plan: &MaskPlan) -> Result<(), EngineError> {
    let Some(planned) = &plan.planned_from else {
        return Ok(());
    };
    let mut now: Vec<(String, String)> = catalog
        .tables
        .iter()
        .filter(|t| t.schema == SCHEMA && t.kind == crate::model::RelationKind::Table)
        .flat_map(|t| t.columns.iter().map(|c| (t.name.clone(), c.name.clone())))
        .collect();
    let mut then: Vec<(String, String)> = planned
        .iter()
        .filter(|p| p.column.schema == SCHEMA)
        .map(|p| (p.column.table.clone(), p.column.column.clone()))
        .collect();
    now.sort();
    then.sort();
    if now != then {
        return Err(EngineError::NotReady);
    }
    Ok(())
}

/// Masks the columns of `plan` in the freshly loaded copy `conn` with `source_key`. Returns
/// the constraints left out (always none here, see the module docs).
pub(crate) fn mask(
    conn: &Connection,
    plan: &MaskPlan,
    source_key: &PseudonymKey,
) -> Result<Vec<String>, EngineError> {
    let catalog = super::conn::catalog(conn)?;
    check_planned_from(&catalog, plan)?;
    let result = mask_columns(conn, &catalog, plan, source_key);
    // After the masking: a masked row ID is a pseudonym number then.
    result.and_then(|left_out| {
        reset_sequences(conn)?;
        Ok(left_out)
    })
}

/// The `AUTOINCREMENT` counters of a masked copy (Kimi review of D-3b on `main`, finding 4).
/// The dump carries the source's `sqlite_sequence`, the highest row ID a table ever had,
/// which tells values that the masking removed (a masked row ID, the IDs of deleted rows).
/// Each counter becomes the highest row ID of the copy's table (0 when it is empty).
fn reset_sequences(conn: &Connection) -> Result<(), EngineError> {
    let has: bool = conn
        .query_row(
            "SELECT count(*) > 0 FROM main.sqlite_schema WHERE name = 'sqlite_sequence'",
            [],
            |r| r.get(0),
        )
        .map_err(internal)?;
    if !has {
        return Ok(());
    }
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM main.sqlite_sequence")
            .map_err(internal)?;
        stmt.query_map([], |r| r.get(0))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?
    };
    for name in names {
        if super::dump::is_ruum_name(&name) {
            continue;
        }
        conn.execute(
            &format!(
                "UPDATE main.sqlite_sequence SET seq = coalesce((SELECT max(rowid) FROM main.{}), 0) \
                 WHERE name = ?1",
                quote(&name)
            ),
            [&name],
        )
        .map_err(internal)?;
    }
    Ok(())
}

/// The masking of the columns of `plan` (see [`mask`]).
fn mask_columns(
    conn: &Connection,
    catalog: &Catalog,
    plan: &MaskPlan,
    source_key: &PseudonymKey,
) -> Result<Vec<String>, EngineError> {
    let columns: Vec<_> = plan
        .columns
        .iter()
        .filter(|(c, _)| {
            c.schema == SCHEMA
                && catalog.tables.iter().any(|t| {
                    t.schema == SCHEMA
                        && t.kind == crate::model::RelationKind::Table
                        && t.name == c.table
                        && t.columns.iter().any(|x| x.name == c.column)
                })
        })
        .collect();
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let key = *source_key.expose();
    conn.create_scalar_function(
        FUNCTION,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| Ok(text_form(ctx.get_raw(0)).map(|t| pseudonym(&key, &t))),
    )
    .map_err(internal)?;
    conn.create_scalar_function(
        FUNCTION_INT,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| Ok(text_form(ctx.get_raw(0)).map(|t| pseudonym_number(&key, &t))),
    )
    .map_err(internal)?;
    let triggers: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT name, sql FROM main.sqlite_schema WHERE type = 'trigger' \
                 AND sql IS NOT NULL ORDER BY rowid",
            )
            .map_err(internal)?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?
    };
    let result = (|| -> Result<(), EngineError> {
        conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN")
            .map_err(internal)?;
        let changed = (|| -> Result<(), EngineError> {
            for (name, _) in &triggers {
                conn.execute_batch(&format!("DROP TRIGGER main.{}", quote(name)))
                    .map_err(internal)?;
            }
            for (column, how) in &columns {
                let (t, c) = (quote(&column.table), quote(&column.column));
                let update =
                    |f: &str| format!("UPDATE main.{t} SET {c} = {f}({c}) WHERE {c} IS NOT NULL");
                let done = match how {
                    MaskAs::Pseudonym => match conn.execute_batch(&update(FUNCTION)) {
                        // A row ID column holds numbers only.
                        Err(e)
                            if e.sqlite_error_code() == Some(rusqlite::ErrorCode::TypeMismatch) =>
                        {
                            conn.execute_batch(&update(FUNCTION_INT))
                        }
                        other => other,
                    },
                    MaskAs::Null => conn.execute_batch(&format!(
                        "UPDATE main.{t} SET {c} = NULL WHERE {c} IS NOT NULL \
                         AND NOT (typeof({c}) = 'text' AND {c} = '')"
                    )),
                };
                done.map_err(|e| {
                    EngineError::Unsupported(format!(
                        "the memory engine cannot mask {}.{}: {e}",
                        column.table, column.column
                    ))
                })?;
            }
            for (_, sql) in &triggers {
                conn.execute_batch(sql).map_err(internal)?;
            }
            Ok(())
        })();
        match changed {
            Ok(()) => conn.execute_batch("COMMIT").map_err(internal)?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        Ok(())
    })();
    let on = conn
        .execute_batch("PRAGMA foreign_keys = ON")
        .map_err(internal);
    let removed = conn
        .remove_function(FUNCTION, 1)
        .and_then(|()| conn.remove_function(FUNCTION_INT, 1))
        .map_err(internal);
    result?;
    on?;
    removed?;
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pseudonym_is_an_hmac_of_the_text_form() {
        let key = [7u8; KEY_LEN];
        let a = pseudonym(&key, b"a@example.test");
        assert!(a.starts_with("p_") && a.len() == 14, "{a}");
        assert_eq!(a, pseudonym(&key, b"a@example.test"), "stable");
        assert_ne!(a, pseudonym(&[8u8; KEY_LEN], b"a@example.test"), "keyed");
        assert_ne!(a, pseudonym(&key, b"b@example.test"));
        let n = pseudonym_number(&key, b"1");
        assert_eq!(format!("p_{n:012x}"), pseudonym(&key, b"1"));
    }
}
