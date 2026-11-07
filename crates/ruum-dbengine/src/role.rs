//! Role kinds and role references (decisions 0004-1, 0004-14, 0004-32, 0004-34).
//!
//! The engine selects the database role of a request by a [`RoleRef`]: the database, the kind
//! and an optional app binding. Never by a bare kind: step 3 adds the app kinds (`app_read`,
//! `app_write`, one pair per binding) as new [`RoleKind`] values and a constructor that sets
//! the binding, and nothing else at this border changes.

use crate::error::EngineError;
use crate::model::DatabaseRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The kinds of `database_roles.kind`, as frozen by spike S-1 (decision 0004-34).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleKind {
    /// People with `manage`; migrations. Lens `base`; owns the tables.
    Owner,
    /// People with `view`, agents with `read_personal`. Lens `nosecret`; no writes.
    Read,
    /// People with `edit`, agents with `read_personal` (with `write_live` or on a draft branch).
    Write,
    /// Agents without `read_personal`. Lens `masked`; no writes.
    MaskedRead,
    /// Agents without `read_personal` (with `write_live` or on a draft branch). Plain columns.
    MaskedWrite,
    /// People with `manage`. Lens `base`; no writes.
    SecretRead,
    /// Nobody (`NOLOGIN`): owns the lens views, `ruum_mask` and the key. Never a session kind.
    Mask,
}

impl RoleKind {
    /// Every kind, in the order of `database_roles.kind`.
    pub const ALL: [RoleKind; 7] = [
        RoleKind::Read,
        RoleKind::Write,
        RoleKind::Owner,
        RoleKind::MaskedRead,
        RoleKind::MaskedWrite,
        RoleKind::SecretRead,
        RoleKind::Mask,
    ];

    /// The kinds an untrusted session can run as (every kind but `mask`).
    pub const SESSION_KINDS: [RoleKind; 6] = [
        RoleKind::Read,
        RoleKind::Write,
        RoleKind::Owner,
        RoleKind::MaskedRead,
        RoleKind::MaskedWrite,
        RoleKind::SecretRead,
    ];

    /// The value in `database_roles.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            RoleKind::Owner => "owner",
            RoleKind::Read => "read",
            RoleKind::Write => "write",
            RoleKind::MaskedRead => "masked_read",
            RoleKind::MaskedWrite => "masked_write",
            RoleKind::SecretRead => "secret_read",
            RoleKind::Mask => "mask",
        }
    }

    /// Parses a value of `database_roles.kind`.
    pub fn parse(value: &str) -> Option<RoleKind> {
        RoleKind::ALL.into_iter().find(|k| k.as_str() == value)
    }

    /// Whether a session of this kind may run (`mask` is `NOLOGIN`).
    pub fn is_session_kind(self) -> bool {
        self != RoleKind::Mask
    }

    /// Whether a session of this kind may change rows.
    pub fn can_write(self) -> bool {
        matches!(
            self,
            RoleKind::Owner | RoleKind::Write | RoleKind::MaskedWrite
        )
    }

    /// Whether a session of this kind may change the schema (only `owner`: migrations).
    pub fn can_change_schema(self) -> bool {
        self == RoleKind::Owner
    }

    /// Whether the kind belongs to an app binding (step 3). None of the frozen kinds does.
    pub fn needs_binding(self) -> bool {
        false
    }
}

/// The role of one request: (database, kind, optional binding). The pool key of the Postgres
/// engine (decision 0004-32). The binding is set exactly for the app kinds of step 3.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoleRef {
    database: DatabaseRef,
    kind: RoleKind,
    binding: Option<Uuid>,
}

impl RoleRef {
    /// The role of a kind without a binding. `mask` is refused: it is never a session kind.
    pub fn new(database: DatabaseRef, kind: RoleKind) -> Result<RoleRef, EngineError> {
        if !kind.is_session_kind() {
            return Err(EngineError::InvalidRole(format!(
                "{} is not a session kind",
                kind.as_str()
            )));
        }
        if kind.needs_binding() {
            return Err(EngineError::InvalidRole(format!(
                "{} needs an app binding",
                kind.as_str()
            )));
        }
        Ok(RoleRef {
            database,
            kind,
            binding: None,
        })
    }

    pub fn database(&self) -> &DatabaseRef {
        &self.database
    }

    pub fn kind(&self) -> RoleKind {
        self.kind
    }

    /// The app binding (`database_roles.binding_id`); `None` for every kind of step 2.
    pub fn binding(&self) -> Option<Uuid> {
        self.binding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> DatabaseRef {
        DatabaseRef {
            tenant_id: Uuid::now_v7(),
            node_id: Uuid::now_v7(),
            cluster: "memory".into(),
            engine_name: "d_0123456789abcdef".into(),
        }
    }

    #[test]
    fn kinds_round_trip_the_column_values() {
        for kind in RoleKind::ALL {
            assert_eq!(RoleKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(RoleKind::parse("app_read"), None);
        assert_eq!(RoleKind::parse("superuser"), None);
    }

    #[test]
    fn mask_is_never_a_session_kind() {
        assert!(matches!(
            RoleRef::new(db(), RoleKind::Mask),
            Err(EngineError::InvalidRole(_))
        ));
        for kind in RoleKind::SESSION_KINDS {
            let role = RoleRef::new(db(), kind).unwrap();
            assert_eq!(role.kind(), kind);
            assert_eq!(role.binding(), None);
        }
    }

    #[test]
    fn write_and_schema_rights_follow_the_kinds_table() {
        let writers: Vec<_> = RoleKind::ALL
            .into_iter()
            .filter(|k| k.can_write())
            .collect();
        assert_eq!(
            writers,
            [RoleKind::Write, RoleKind::Owner, RoleKind::MaskedWrite]
        );
        let schema: Vec<_> = RoleKind::ALL
            .into_iter()
            .filter(|k| k.can_change_schema())
            .collect();
        assert_eq!(schema, [RoleKind::Owner]);
    }
}
