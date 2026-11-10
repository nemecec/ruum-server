//! The `MaskingRules` implementation for Postgres (card Q-2): when the column classes of a
//! database change, rebuild its lens views (`ruum_nosecret`, `ruum_masked`) through
//! `DatabaseEngine::sync_lens`, in one engine transaction with the catalog sync of card K-1
//! ([`StrictSync`]).
//!
//! # The classes of the lens
//!
//! The lens takes each column's class from the column store of card K-1 (`database_columns`:
//! the confirmed class, or `personal` while unconfirmed), made stricter by the head manifest's
//! `data_classes` ([`StrictSync`]). The two are kept equal by K-1, but a person can change
//! `data_classes` through `updateDatabaseManifest` or `restoreVersion` without the store
//! (review-K-1 C-1, backlog P-1). Taking the stricter of the two keeps that drift on the safe
//! side: a class that either source has taken back is `personal`, and a `secret` in either
//! source is `secret`.
//!
//! # When it runs
//!
//! - After a confirmation and after a catalog sync that changed classes
//!   (`MaskingRules::classes_changed`, called by K-1).
//! - A person's schema change through `queryDatabase` (wave 4 decision 3) passes a
//!   [`StrictSync`] to `DatabaseEngine::apply_migration`, so the rebuild after the change uses the
//!   same classes.
//!
//! A database of another engine, or an engine without a lens (the memory engine), is left alone.
//! A database that is not ready (a migration runs) is left alone too: the migration's own rebuild
//! reads the store after the change.
//!
//! The rules are built before the service they need ([`crate::databases::build`] makes both),
//! so they hold a weak handle that `build` binds ([`PostgresMaskingRules::bind`]).

use super::MaskingRules;
use crate::databases::DatabaseService;
use crate::databases::classes::ColumnSync;
use crate::databases::query::postgres_mask::{parse_class_key, stricter};
use crate::databases::query::{class_key, manifest_classes, own_schema};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use ruum_dbengine::{
    Catalog, CatalogSync, ColumnRef, DataClass, EngineError, EngineKind, LensSpec,
};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

/// The catalog sync of a lens build: card K-1's [`ColumnSync`] (it records the catalog and
/// answers the store's classes), each class made stricter by the head manifest's
/// `data_classes` (see the module docs).
pub struct StrictSync {
    columns: ColumnSync,
    engine: EngineKind,
    manifest: HashMap<String, DataClass>,
}

impl StrictSync {
    /// The column sync, to call [`ColumnSync::finish`] after the engine call.
    pub fn columns(&self) -> &ColumnSync {
        &self.columns
    }
}

/// `spec` with each class made stricter by `manifest`: a column whose key the manifest lacks
/// counts as `personal` at least; a manifest key of a live column that `spec` lacks keeps its
/// class (never less than `personal`).
pub fn stricter_spec(
    engine: EngineKind,
    catalog: &Catalog,
    spec: LensSpec,
    manifest: &HashMap<String, DataClass>,
) -> LensSpec {
    let mut columns: Vec<(ColumnRef, DataClass)> = spec
        .columns
        .into_iter()
        .map(|(c, class)| {
            let key = class_key(&c.schema, &c.table, &c.column);
            let m = manifest.get(&key).copied().unwrap_or(DataClass::Personal);
            (c, stricter(class, m))
        })
        .collect();
    let own = own_schema(engine);
    for (key, class) in manifest {
        let Some((schema, table, column)) = parse_class_key(key) else {
            continue;
        };
        if schema != own
            || columns
                .iter()
                .any(|(c, _)| c.schema == schema && c.table == table && c.column == column)
        {
            continue;
        }
        let live = catalog
            .table(&schema, &table)
            .is_some_and(|t| t.column(&column).is_some());
        if live {
            columns.push((
                ColumnRef {
                    schema,
                    table,
                    column,
                },
                stricter(*class, DataClass::Personal),
            ));
        }
    }
    LensSpec { columns }
}

#[async_trait]
impl CatalogSync for StrictSync {
    async fn sync(&self, catalog: &Catalog) -> std::result::Result<LensSpec, EngineError> {
        let spec = self.columns.sync(catalog).await?;
        Ok(stricter_spec(self.engine, catalog, spec, &self.manifest))
    }
}

/// The confirmed classes of the head manifest of a database (`data_classes`).
pub async fn head_classes(
    svc: &DatabaseService,
    tenant: TenantId,
    node: NodeId,
) -> Result<HashMap<String, DataClass>> {
    let d = svc.deps();
    let Some(typed) = d.typed.get(tenant, node).await? else {
        return Ok(HashMap::new());
    };
    let manifest = d
        .store
        .manifest(tenant, typed.desired_version)
        .await?
        .unwrap_or_default();
    Ok(manifest_classes(&manifest))
}

/// The [`StrictSync`] of a database.
pub async fn strict_sync(
    svc: &DatabaseService,
    tenant: TenantId,
    node: NodeId,
    engine: EngineKind,
) -> Result<StrictSync> {
    Ok(StrictSync {
        columns: svc.column_sync(tenant, node, engine),
        engine,
        manifest: head_classes(svc, tenant, node).await?,
    })
}

/// Rebuilds the lens of a Postgres database from its classes now (see the module docs). Tries
/// twice; a database that is gone or not ready is left alone.
pub async fn rebuild_lens(svc: &DatabaseService, tenant: TenantId, node: NodeId) -> Result<()> {
    let d = svc.deps();
    let Some(row) = d.store.get(tenant, node).await? else {
        return Ok(());
    };
    if row.engine != EngineKind::Postgres {
        return Ok(());
    }
    let Some(engine) = d.engines.get(row.engine).cloned() else {
        return Ok(());
    };
    if !engine.has_lens() {
        return Ok(());
    }
    let key = d.queries.keys.engine_key(tenant, row.key_node_id).await?;
    let db = row.database_ref();
    let mut last = None;
    for _ in 0..2 {
        let sync = strict_sync(svc, tenant, node, row.engine).await?;
        match engine.sync_lens(&db, &key, &sync).await {
            Ok(_) => return Ok(()),
            Err(EngineError::NotFound) => return Ok(()),
            Err(EngineError::NotReady) => {
                tracing::info!(%tenant, %node, "the lens rebuild waits for the running change");
                return Ok(());
            }
            Err(e) => last = Some(e),
        }
    }
    let e = last.map(|e| e.to_string()).unwrap_or_default();
    tracing::warn!(%tenant, %node, error = %e, "the lens of a database was not rebuilt");
    Err(Error::Internal(format!(
        "the lens of the database was not rebuilt after a change of its classes: {e}"
    )))
}

/// The `MaskingRules` of Postgres (see the module docs).
#[derive(Default)]
pub struct PostgresMaskingRules {
    service: OnceLock<Weak<DatabaseService>>,
}

impl PostgresMaskingRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds the service whose databases these rules rebuild. Only the first call counts.
    pub fn bind(&self, service: &Arc<DatabaseService>) {
        let _ = self.service.set(Arc::downgrade(service));
    }
}

#[async_trait]
impl MaskingRules for PostgresMaskingRules {
    async fn classes_changed(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let Some(svc) = self.service.get().and_then(Weak::upgrade) else {
            return Ok(());
        };
        rebuild_lens(&svc, tenant, node).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_dbengine::{CatalogColumn, CatalogTable, RelationKind};

    #[test]
    fn the_lens_takes_the_stricter_of_store_and_manifest() {
        let col = |n: &str| CatalogColumn {
            name: n.into(),
            data_type: "text".into(),
            nullable: true,
            identity: None,
        };
        let catalog = Catalog {
            tables: vec![CatalogTable {
                schema: "public".into(),
                name: "t".into(),
                kind: RelationKind::Table,
                columns: vec![col("a"), col("b"), col("c"), col("d")],
                primary_key: vec![],
                foreign_keys: vec![],
            }],
        };
        let r = |c: &str| ColumnRef {
            schema: "public".into(),
            table: "t".into(),
            column: c.into(),
        };
        // The store: a and b normal (b was taken back in the manifest), c normal.
        let spec = LensSpec {
            columns: vec![
                (r("a"), DataClass::Normal),
                (r("b"), DataClass::Normal),
                (r("c"), DataClass::Normal),
            ],
        };
        let manifest = HashMap::from([
            ("public.t.a".to_string(), DataClass::Normal),
            ("public.t.c".to_string(), DataClass::Secret),
            ("public.t.d".to_string(), DataClass::Secret),
            ("public.t.gone".to_string(), DataClass::Secret),
        ]);
        let out = stricter_spec(EngineKind::Postgres, &catalog, spec, &manifest);
        assert_eq!(out.class_of(&r("a")), DataClass::Normal);
        assert_eq!(out.class_of(&r("b")), DataClass::Personal);
        assert_eq!(out.class_of(&r("c")), DataClass::Secret);
        assert_eq!(out.class_of(&r("d")), DataClass::Secret);
        assert!(out.columns.iter().all(|(c, _)| c.column != "gone"));
    }
}
