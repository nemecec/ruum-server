//! `ruum db ...`: the database operations that are built (design sections 8.1, 8.4 and 8.5).
//!
//! A database is addressed by its path, like a file: `/Projects/garden`. Each command prints
//! text for people, or the server's JSON answer with `--json`.
//!
//! Query results can hold markers instead of values (an agent, or a person without the right,
//! does not see a `personal` or `secret` value). A marker prints as text in angle brackets that
//! cannot be taken for a value: `<pseudonym p_3f9c0a1b2c3d>`, `<hidden personal>`,
//! `<hidden secret>`. A string value that could be taken for a marker or for `NULL` prints in
//! JSON quotes. A literal (a JSON value that has the key `$ruum` itself) prints as its value.
//!
//! There are no commands for the planned operations (migrations, branches, restore points,
//! approvals): they come with their cards.

use crate::client::encode_segment;
use crate::{Ctx, s};
use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EngineArg {
    Postgres,
    Sqlite,
}

impl EngineArg {
    fn manifest(self) -> Value {
        match self {
            EngineArg::Postgres => {
                json!({"engine": "postgres", "version": "17", "plan": "shared-small"})
            }
            EngineArg::Sqlite => {
                json!({"engine": "sqlite", "version": "3", "plan": "shared-small"})
            }
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum GrantKindArg {
    /// Writes to the live database (at most 24 hours).
    WriteLive,
    /// Real `personal` values (at most 8 hours).
    ReadPersonal,
}

impl GrantKindArg {
    fn as_str(self) -> &'static str {
        match self {
            GrantKindArg::WriteLive => "write_live",
            GrantKindArg::ReadPersonal => "read_personal",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum DbCommand {
    /// Create a database in a folder. Provisioning starts; `ruum db show` gives the status.
    Create {
        /// The folder, for example /Projects.
        folder: String,
        /// The name of the database node.
        name: String,
        #[arg(long, value_enum)]
        engine: EngineArg,
    },
    /// Show a database: status, size and limits.
    Show { path: String },
    /// Print the manifest, or write a new one from a file (needs manage).
    Manifest {
        path: String,
        /// A JSON file with the new manifest.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// A name for the new manifest version (with --file).
        #[arg(long, requires = "file")]
        label: Option<String>,
    },
    /// List the tables, views and columns (names and types, never values).
    Schema { path: String },
    /// Run one SQL statement. Read-only unless --write.
    Query {
        path: String,
        /// One statement with placeholders ($1, $2 for Postgres; ?1, ?2 for SQLite).
        sql: String,
        /// The value of the next placeholder. A JSON number, true, false, null or a quoted
        /// JSON string is taken as that value; anything else is a string.
        #[arg(long = "param", value_name = "VALUE", allow_hyphen_values = true)]
        params: Vec<String>,
        /// Run in write mode (needs edit).
        #[arg(long)]
        write: bool,
        /// At most this many rows (lower than the plan limit).
        #[arg(long)]
        max_rows: Option<u64>,
    },
    /// The query log, newest first.
    Queries {
        path: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// The data class of every column (effective, confirmed, suggested).
    Columns {
        path: String,
        /// Only the columns that wait for a person.
        #[arg(long)]
        unconfirmed: bool,
    },
    /// Suggest data classes: [schema.]table.column=normal|personal|secret ... (needs edit).
    Suggest {
        path: String,
        #[arg(required = true, value_name = "COLUMN=CLASS")]
        columns: Vec<String>,
        /// Why (for the person who confirms).
        #[arg(long)]
        reason: String,
    },
    /// Confirm data classes: [schema.]table.column=normal|personal|secret ... (people with
    /// manage only).
    Confirm {
        path: String,
        #[arg(required = true, value_name = "COLUMN=CLASS")]
        columns: Vec<String>,
    },
    /// The live and recent grants of agents.
    Grants {
        path: String,
        /// Only the grants in force now.
        #[arg(long)]
        active: bool,
    },
    /// Give an agent a time-limited grant (people only: the user the agent acts for).
    Grant {
        path: String,
        /// The agent's name or ID.
        #[arg(long)]
        agent: String,
        #[arg(long, value_enum)]
        kind: GrantKindArg,
        /// How long, in minutes.
        #[arg(long)]
        minutes: u32,
        #[arg(long)]
        reason: Option<String>,
    },
    /// End a grant now.
    Revoke { path: String, grant_id: String },
}

/// One result cell as text for people (see the module docs).
pub fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let looks_like_marker = s.starts_with('<') && s.ends_with('>');
            if looks_like_marker
                || s == "NULL"
                || s.is_empty()
                || s.trim() != s
                || s.contains(['\n', '\r', '\t'])
            {
                Value::String(s.clone()).to_string()
            } else {
                s.clone()
            }
        }
        Value::Array(_) => v.to_string(),
        Value::Object(o) => match o.get("$ruum").and_then(Value::as_str) {
            Some("pseudonym") => format!("<pseudonym {}>", o["id"].as_str().unwrap_or("?")),
            Some("hidden") => format!("<hidden {}>", o["class"].as_str().unwrap_or("value")),
            Some("literal") => o.get("value").cloned().unwrap_or(Value::Null).to_string(),
            Some(other) => format!("<{other}>"),
            None => v.to_string(),
        },
    }
}

/// A `--param` value: JSON scalars as such, anything else as a string.
pub fn param_value(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(v @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_))) => v,
        _ => Value::String(text.to_string()),
    }
}

/// `[schema.]table.column=class`.
struct ColumnClass {
    schema: Option<String>,
    table: String,
    column: String,
    class: String,
}

fn parse_column_class(arg: &str) -> Result<ColumnClass> {
    let (name, class) = arg
        .split_once('=')
        .ok_or_else(|| anyhow!("{arg}: give table.column=class"))?;
    let class = class.trim();
    if !matches!(class, "normal" | "personal" | "secret") {
        bail!("{arg}: the class is normal, personal or secret");
    }
    let parts: Vec<&str> = name.split('.').collect();
    let (schema, table, column) = match parts.as_slice() {
        [t, c] => (None, *t, *c),
        [sc, t, c] => (Some(sc.to_string()), *t, *c),
        _ => bail!("{arg}: give table.column=class or schema.table.column=class"),
    };
    if table.is_empty() || column.is_empty() {
        bail!("{arg}: give table.column=class");
    }
    Ok(ColumnClass {
        schema,
        table: table.into(),
        column: column.into(),
        class: class.into(),
    })
}

/// Prints rows as aligned columns.
fn table(out: &mut dyn std::io::Write, header: &[String], rows: &[Vec<String>]) -> Result<()> {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(c.chars().count());
            }
        }
    }
    let line = |cells: &[String]| {
        let mut l = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i > 0 {
                l.push_str("  ");
            }
            if i + 1 == cells.len() {
                l.push_str(c);
            } else {
                l.push_str(&format!("{c:<w$}", w = widths[i]));
            }
        }
        l
    };
    writeln!(out, "{}", line(header))?;
    for r in rows {
        writeln!(out, "{}", line(r))?;
    }
    Ok(())
}

fn human_ms(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

fn actor_text(by: &Value) -> String {
    let a = &by["actor"];
    let name = a["display_name"]
        .as_str()
        .or(a["id"].as_str())
        .unwrap_or("?");
    format!("{}:{name}", a["kind"].as_str().unwrap_or("?"))
}

impl Ctx<'_> {
    pub(crate) async fn db(&mut self, cmd: DbCommand) -> Result<()> {
        match cmd {
            DbCommand::Create {
                folder,
                name,
                engine,
            } => self.db_create(&folder, &name, engine).await,
            DbCommand::Show { path } => self.db_show(&path).await,
            DbCommand::Manifest { path, file, label } => {
                self.db_manifest(&path, file.as_deref(), label).await
            }
            DbCommand::Schema { path } => self.db_schema(&path).await,
            DbCommand::Query {
                path,
                sql,
                params,
                write,
                max_rows,
            } => self.db_query(&path, sql, &params, write, max_rows).await,
            DbCommand::Queries { path, limit } => self.db_queries(&path, limit).await,
            DbCommand::Columns { path, unconfirmed } => self.db_columns(&path, unconfirmed).await,
            DbCommand::Suggest {
                path,
                columns,
                reason,
            } => self.db_suggest(&path, &columns, &reason).await,
            DbCommand::Confirm { path, columns } => self.db_confirm(&path, &columns).await,
            DbCommand::Grants { path, active } => self.db_grants(&path, active).await,
            DbCommand::Grant {
                path,
                agent,
                kind,
                minutes,
                reason,
            } => self.db_grant(&path, &agent, kind, minutes, reason).await,
            DbCommand::Revoke { path, grant_id } => self.db_revoke(&path, &grant_id).await,
        }
    }

    /// The node ID of the database at `path`.
    async fn database_id(&mut self, path: &str) -> Result<String> {
        let node = self.resolve(path).await?;
        if node["type"] != "database" {
            bail!("{path}: not a database");
        }
        Ok(s(&node, "id"))
    }

    async fn db_create(&mut self, folder: &str, name: &str, engine: EngineArg) -> Result<()> {
        let parent = self.resolve(folder).await?;
        if parent["kind"] != "folder" {
            bail!("{folder}: not a folder");
        }
        let w = self
            .c
            .post(
                "/v1/databases",
                json!({"parent_id": parent["id"], "name": name, "manifest": engine.manifest()}),
            )
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        let db = &w["database"];
        writeln!(
            self.out,
            "created {}/{} ({}), state {}",
            folder.trim_end_matches('/'),
            name,
            s(&db["node"], "id"),
            s(&db["status"], "state")
        )?;
        writeln!(
            self.out,
            "hint: `ruum db show` shows when it is ready for queries"
        )?;
        Ok(())
    }

    async fn db_show(&mut self, path: &str) -> Result<()> {
        let id = self.database_id(path).await?;
        let db = self.c.get(&format!("/v1/databases/{id}"), &[]).await?;
        if self.json {
            return self.print_json(&db);
        }
        let m = &db["manifest"];
        let st = &db["status"];
        let l = &db["limits"];
        writeln!(self.out, "database {path} ({id})")?;
        writeln!(
            self.out,
            "engine: {} {}, plan {}",
            s(m, "engine"),
            s(m, "version"),
            s(m, "plan")
        )?;
        let mut state = s(st, "state");
        if let Some(r) = st["reason"].as_str() {
            state.push_str(&format!(" ({r})"));
        }
        if let Some(msg) = st["message"].as_str() {
            state.push_str(&format!(": {msg}"));
        }
        writeln!(self.out, "status: {state}")?;
        writeln!(
            self.out,
            "size: {} of {}",
            crate::human_size(db["size_bytes"].as_u64().unwrap_or(0)),
            crate::human_size(l["max_size_bytes"].as_u64().unwrap_or(0))
        )?;
        writeln!(
            self.out,
            "limits: statement timeout {}, {} rows, {} per result",
            human_ms(l["statement_timeout_ms"].as_u64().unwrap_or(0)),
            s(l, "max_rows"),
            crate::human_size(l["max_result_bytes"].as_u64().unwrap_or(0))
        )?;
        writeln!(
            self.out,
            "unconfirmed columns: {}",
            s(&db, "unconfirmed_columns")
        )?;
        if let Some(p) = db["my_permissions"].as_object() {
            let yes: Vec<&str> = p
                .iter()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(k, _)| k.as_str())
                .collect();
            writeln!(
                self.out,
                "you may: {}",
                if yes.is_empty() {
                    "-".into()
                } else {
                    yes.join(", ")
                }
            )?;
        }
        Ok(())
    }

    async fn db_manifest(
        &mut self,
        path: &str,
        file: Option<&std::path::Path>,
        label: Option<String>,
    ) -> Result<()> {
        let id = self.database_id(path).await?;
        let Some(file) = file else {
            let db = self.c.get(&format!("/v1/databases/{id}"), &[]).await?;
            // The manifest is JSON in both forms.
            return self.print_json(&db["manifest"]);
        };
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let manifest: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", file.display()))?;
        let mut body = json!({ "manifest": manifest });
        if let Some(l) = label {
            body["label"] = Value::String(l);
        }
        let w = self
            .c
            .json(
                Method::PUT,
                &format!("/v1/databases/{id}/manifest"),
                &[],
                crate::client::Body::Json(body),
            )
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        writeln!(
            self.out,
            "new manifest version {} for {path}, state {}",
            s(&w["database"], "manifest_version_id"),
            s(&w["database"]["status"], "state")
        )?;
        Ok(())
    }

    async fn db_schema(&mut self, path: &str) -> Result<()> {
        let id = self.database_id(path).await?;
        let schema = self
            .c
            .get(&format!("/v1/databases/{id}/schema"), &[])
            .await?;
        if self.json {
            return self.print_json(&schema);
        }
        writeln!(self.out, "engine: {}", s(&schema, "engine"))?;
        for t in schema["tables"].as_array().into_iter().flatten() {
            let pk: Vec<&str> = t["primary_key"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            writeln!(
                self.out,
                "\n{} {}.{}",
                s(t, "kind"),
                s(t, "schema"),
                s(t, "name")
            )?;
            let rows: Vec<Vec<String>> = t["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    let name = s(c, "name");
                    let mut notes = Vec::new();
                    if pk.contains(&name.as_str()) {
                        notes.push("primary key");
                    }
                    if c["confirmed"] != true {
                        notes.push("unconfirmed");
                    }
                    vec![
                        format!("  {name}"),
                        s(c, "type"),
                        if c["nullable"] == true {
                            "null".into()
                        } else {
                            "not null".into()
                        },
                        s(c, "data_class"),
                        notes.join(", "),
                    ]
                })
                .collect();
            table(
                self.out,
                &[
                    "  column".into(),
                    "type".into(),
                    "nullable".into(),
                    "class".into(),
                    "notes".into(),
                ],
                &rows,
            )?;
            for fk in t["foreign_keys"].as_array().into_iter().flatten() {
                writeln!(
                    self.out,
                    "  foreign key ({}) references {}.{} ({})",
                    join_strings(&fk["columns"]),
                    s(fk, "references_schema"),
                    s(fk, "references_table"),
                    join_strings(&fk["references_columns"])
                )?;
            }
        }
        Ok(())
    }

    async fn db_query(
        &mut self,
        path: &str,
        sql: String,
        params: &[String],
        write: bool,
        max_rows: Option<u64>,
    ) -> Result<()> {
        let id = self.database_id(path).await?;
        let mut body = json!({
            "sql": sql,
            "params": params.iter().map(|p| param_value(p)).collect::<Vec<_>>(),
            "mode": if write { "write" } else { "read" },
        });
        if let Some(n) = max_rows {
            body["max_rows"] = json!(n);
        }
        let r = self
            .c
            .post(&format!("/v1/databases/{id}/query"), body)
            .await?;
        if self.json {
            return self.print_json(&r);
        }
        if r["approval"].is_object() {
            let a = &r["approval"];
            writeln!(
                self.out,
                "nothing ran: a person must approve this statement first (approval {}, status {})",
                s(a, "id"),
                s(a, "status")
            )?;
            if let Some(u) = a["review_url"].as_str() {
                writeln!(self.out, "review: {u}")?;
            }
            return Ok(());
        }
        let header: Vec<String> = r["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| s(c, "name"))
            .collect();
        let rows: Vec<Vec<String>> = r["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                row.as_array()
                    .into_iter()
                    .flatten()
                    .map(cell_text)
                    .collect()
            })
            .collect();
        if !header.is_empty() {
            table(self.out, &header, &rows)?;
        }
        let mut summary = format!(
            "{} row{}",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        );
        if let Some(n) = r["rows_affected"].as_i64() {
            summary.push_str(&format!(", {n} affected"));
        }
        if r["truncated"] == true {
            summary.push_str(&format!(
                ", cut at {}",
                r["truncated_reason"].as_str().unwrap_or("a limit")
            ));
        }
        summary.push_str(&format!(
            " ({})",
            human_ms(r["elapsed_ms"].as_u64().unwrap_or(0))
        ));
        writeln!(self.out, "{summary}")?;
        let masked: Vec<String> = r["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["masking"].as_str().is_some_and(|m| m != "none"))
            .map(|c| format!("{} ({})", s(c, "name"), s(c, "masking")))
            .collect();
        if !masked.is_empty() {
            writeln!(self.out, "masked columns: {}", masked.join(", "))?;
        }
        Ok(())
    }

    async fn db_queries(&mut self, path: &str, limit: u32) -> Result<()> {
        let id = self.database_id(path).await?;
        let page = self
            .c
            .get(
                &format!("/v1/databases/{id}/queries"),
                &[("limit", limit.to_string())],
            )
            .await?;
        if self.json {
            return self.print_json(&page);
        }
        let rows: Vec<Vec<String>> = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| {
                vec![
                    s(e, "at"),
                    actor_text(&e["by"]),
                    s(e, "mode"),
                    match e["error_code"].as_str() {
                        Some(c) => format!("{} ({c})", s(e, "outcome")),
                        None => s(e, "outcome"),
                    },
                    e["statement"].as_str().unwrap_or("-").replace('\n', " "),
                ]
            })
            .collect();
        table(
            self.out,
            &[
                "at".into(),
                "by".into(),
                "mode".into(),
                "outcome".into(),
                "statement".into(),
            ],
            &rows,
        )?;
        if let Some(c) = page["next_cursor"].as_str() {
            writeln!(self.out, "more entries (next cursor {c})")?;
        }
        Ok(())
    }

    async fn columns(&mut self, id: &str, unconfirmed: bool) -> Result<Vec<Value>> {
        let mut q = vec![("limit", "500".to_string())];
        if unconfirmed {
            q.push(("unconfirmed", "true".into()));
        }
        self.c
            .all_pages(&format!("/v1/databases/{id}/columns"), &q)
            .await
    }

    async fn db_columns(&mut self, path: &str, unconfirmed: bool) -> Result<()> {
        let id = self.database_id(path).await?;
        let items = self.columns(&id, unconfirmed).await?;
        if self.json {
            return self.print_json(&Value::Array(items));
        }
        let rows: Vec<Vec<String>> = items
            .iter()
            .map(|c| {
                let suggestion = match c["suggestion"].as_object() {
                    Some(sg) => format!(
                        "{} ({})",
                        sg.get("class").and_then(Value::as_str).unwrap_or("?"),
                        sg.get("by").map(actor_text).unwrap_or_default()
                    ),
                    None => "-".into(),
                };
                vec![
                    format!("{}.{}.{}", s(c, "schema"), s(c, "table"), s(c, "column")),
                    s(c, "type"),
                    s(c, "data_class"),
                    s(c, "confirmed_class"),
                    suggestion,
                ]
            })
            .collect();
        table(
            self.out,
            &[
                "column".into(),
                "type".into(),
                "class".into(),
                "confirmed".into(),
                "suggestion".into(),
            ],
            &rows,
        )
    }

    /// The column references of `args`, with the schema found in the column list when the
    /// argument leaves it out.
    async fn column_classes(&mut self, id: &str, args: &[String]) -> Result<Vec<Value>> {
        let parsed: Vec<ColumnClass> = args
            .iter()
            .map(|a| parse_column_class(a))
            .collect::<Result<_>>()?;
        let known = if parsed.iter().any(|p| p.schema.is_none()) {
            self.columns(id, false).await?
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        for p in parsed {
            let schema = match p.schema {
                Some(sc) => sc,
                None => {
                    let schemas: Vec<String> = known
                        .iter()
                        .filter(|c| {
                            c["table"] == p.table.as_str() && c["column"] == p.column.as_str()
                        })
                        .map(|c| s(c, "schema"))
                        .collect();
                    match schemas.as_slice() {
                        [one] => one.clone(),
                        [] => bail!(
                            "{}.{}: no such column\nhint: `ruum db columns` lists them",
                            p.table,
                            p.column
                        ),
                        _ => bail!(
                            "{}.{} is in more than one schema\nhint: give schema.table.column",
                            p.table,
                            p.column
                        ),
                    }
                }
            };
            out.push(json!({
                "schema": schema, "table": p.table, "column": p.column, "class": p.class,
            }));
        }
        Ok(out)
    }

    async fn db_suggest(&mut self, path: &str, args: &[String], reason: &str) -> Result<()> {
        let id = self.database_id(path).await?;
        let mut cols = self.column_classes(&id, args).await?;
        for c in &mut cols {
            c["reason"] = Value::String(reason.to_string());
        }
        let r = self
            .c
            .post(
                &format!("/v1/databases/{id}/columns/suggestions"),
                json!({ "suggestions": cols }),
            )
            .await?;
        if self.json {
            return self.print_json(&r);
        }
        writeln!(
            self.out,
            "suggested {} column class{} for {path}; a person confirms them (`ruum db confirm`)",
            cols.len(),
            if cols.len() == 1 { "" } else { "es" }
        )?;
        Ok(())
    }

    async fn db_confirm(&mut self, path: &str, args: &[String]) -> Result<()> {
        let id = self.database_id(path).await?;
        let cols = self.column_classes(&id, args).await?;
        let w = self
            .c
            .post(
                &format!("/v1/databases/{id}/columns/confirm"),
                json!({ "columns": cols }),
            )
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        writeln!(
            self.out,
            "confirmed {} column class{} for {path} (manifest version {})",
            cols.len(),
            if cols.len() == 1 { "" } else { "es" },
            s(&w["database"], "manifest_version_id")
        )?;
        Ok(())
    }

    async fn db_grants(&mut self, path: &str, active: bool) -> Result<()> {
        let id = self.database_id(path).await?;
        let mut q = vec![("limit", "200".to_string())];
        if active {
            q.push(("active", "true".into()));
        }
        let items = self
            .c
            .all_pages(&format!("/v1/databases/{id}/access-grants"), &q)
            .await?;
        if self.json {
            return self.print_json(&Value::Array(items));
        }
        let rows: Vec<Vec<String>> = items
            .iter()
            .map(|g| {
                let state = if !g["revoked_at"].is_null() {
                    "revoked"
                } else {
                    "-"
                };
                vec![
                    s(g, "id"),
                    s(g, "kind"),
                    g["agent_display_name"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| s(g, "agent_id")),
                    s(g, "expires_at"),
                    state.into(),
                ]
            })
            .collect();
        table(
            self.out,
            &[
                "id".into(),
                "kind".into(),
                "agent".into(),
                "expires".into(),
                "revoked".into(),
            ],
            &rows,
        )
    }

    /// The ID of the agent called `agent` (a name or an ID) among the caller's agents.
    async fn agent_id(&mut self, agent: &str) -> Result<String> {
        if uuid::Uuid::parse_str(agent).is_ok() {
            return Ok(agent.to_string());
        }
        let items = self
            .c
            .all_pages("/v1/agents", &[("limit", "1000".into())])
            .await?;
        let found: Vec<String> = items
            .iter()
            .filter(|a| a["name"] == agent)
            .map(|a| s(a, "id"))
            .collect();
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => bail!("no agent called \"{agent}\"\nhint: `ruum agents` lists them"),
            _ => bail!("more than one agent is called \"{agent}\"\nhint: give its ID"),
        }
    }

    async fn db_grant(
        &mut self,
        path: &str,
        agent: &str,
        kind: GrantKindArg,
        minutes: u32,
        reason: Option<String>,
    ) -> Result<()> {
        let id = self.database_id(path).await?;
        let agent_id = self.agent_id(agent).await?;
        let mut body = json!({
            "kind": kind.as_str(), "agent_id": agent_id, "duration_minutes": minutes,
        });
        if let Some(r) = reason {
            body["reason"] = Value::String(r);
        }
        let w = self
            .c
            .post(&format!("/v1/databases/{id}/access-grants"), body)
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        let g = &w["grant"];
        writeln!(
            self.out,
            "granted {} on {path} to {} until {} (grant {})",
            s(g, "kind"),
            agent,
            s(g, "expires_at"),
            s(g, "id")
        )?;
        Ok(())
    }

    async fn db_revoke(&mut self, path: &str, grant_id: &str) -> Result<()> {
        let id = self.database_id(path).await?;
        let w = self
            .c
            .json(
                Method::DELETE,
                &format!(
                    "/v1/databases/{id}/access-grants/{}",
                    encode_segment(grant_id)
                ),
                &[],
                crate::client::Body::None,
            )
            .await?;
        if self.json {
            return self.print_json(&w);
        }
        writeln!(self.out, "revoked grant {grant_id} on {path}")?;
        Ok(())
    }
}

fn join_strings(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_cannot_be_taken_for_values() {
        assert_eq!(
            cell_text(&json!({"$ruum": "pseudonym", "id": "p_3f9c0a1b2c3d"})),
            "<pseudonym p_3f9c0a1b2c3d>"
        );
        assert_eq!(
            cell_text(&json!({"$ruum": "hidden", "class": "personal"})),
            "<hidden personal>"
        );
        assert_eq!(
            cell_text(&json!({"$ruum": "hidden", "class": "secret"})),
            "<hidden secret>"
        );
        // A literal prints as its value (JSON), never as a marker.
        assert_eq!(
            cell_text(&json!({"$ruum": "literal", "value": {"$ruum": "hidden"}})),
            r#"{"$ruum":"hidden"}"#
        );
        // Strings that look like a marker or NULL are quoted.
        assert_eq!(cell_text(&json!("<hidden secret>")), r#""<hidden secret>""#);
        assert_eq!(cell_text(&json!("NULL")), r#""NULL""#);
        assert_eq!(cell_text(&json!("")), r#""""#);
        assert_eq!(cell_text(&json!("a\tb")), r#""a\tb""#);
        assert_eq!(cell_text(&Value::Null), "NULL");
        assert_eq!(cell_text(&json!("Rosa canina")), "Rosa canina");
        assert_eq!(cell_text(&json!(42)), "42");
        assert_eq!(cell_text(&json!([1, 2])), "[1,2]");
    }

    #[test]
    fn params_are_json_scalars_or_strings() {
        assert_eq!(param_value("42"), json!(42));
        assert_eq!(param_value("true"), json!(true));
        assert_eq!(param_value("null"), Value::Null);
        assert_eq!(param_value("\"42\""), json!("42"));
        assert_eq!(param_value("Rosa canina"), json!("Rosa canina"));
        assert_eq!(param_value("[1]"), json!("[1]"));
    }

    #[test]
    fn column_classes_parse() {
        let c = parse_column_class("plants.latin_name=normal").unwrap();
        assert_eq!(
            (c.schema, c.table.as_str(), c.column.as_str()),
            (None, "plants", "latin_name")
        );
        let c = parse_column_class("public.plants.id=normal").unwrap();
        assert_eq!(c.schema.as_deref(), Some("public"));
        assert!(parse_column_class("plants.id=public").is_err());
        assert!(parse_column_class("plants=normal").is_err());
        assert!(parse_column_class("plants.id").is_err());
    }
}
