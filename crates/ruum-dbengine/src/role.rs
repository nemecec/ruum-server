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
    /// One app binding (step 3, card D-5): lens `base` (real values and secrets), reads only.
    /// A LOGIN role per binding with a short-lived password, used by the app itself, never by a
    /// platform pool.
    AppRead,
    /// One app binding (step 3, card D-5): lens `base`, reads and writes rows, never the schema.
    AppWrite,
}

impl RoleKind {
    /// The per-database kinds of step 2, in the order of `database_roles.kind`: the roles that
    /// `create_database` makes (exactly these seven). The app kinds are not here
    /// ([`RoleKind::APP_KINDS`]).
    pub const ALL: [RoleKind; 7] = [
        RoleKind::Read,
        RoleKind::Write,
        RoleKind::Owner,
        RoleKind::MaskedRead,
        RoleKind::MaskedWrite,
        RoleKind::SecretRead,
        RoleKind::Mask,
    ];

    /// The kinds a platform session can run as (every kind of [`RoleKind::ALL`] but `mask`).
    /// The platform's pools log in as these kinds only, never as an app kind.
    pub const SESSION_KINDS: [RoleKind; 6] = [
        RoleKind::Read,
        RoleKind::Write,
        RoleKind::Owner,
        RoleKind::MaskedRead,
        RoleKind::MaskedWrite,
        RoleKind::SecretRead,
    ];

    /// The kinds of an app binding (step 3, decisions 0004-14 and 0004-32): one role per
    /// (database, kind, binding), made by `create_app_role`, never by `create_database`.
    pub const APP_KINDS: [RoleKind; 2] = [RoleKind::AppRead, RoleKind::AppWrite];

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
            RoleKind::AppRead => "app_read",
            RoleKind::AppWrite => "app_write",
        }
    }

    /// Parses a value of `database_roles.kind` (the kinds of step 2 and the app kinds).
    pub fn parse(value: &str) -> Option<RoleKind> {
        RoleKind::ALL
            .into_iter()
            .chain(RoleKind::APP_KINDS)
            .find(|k| k.as_str() == value)
    }

    /// Whether a session of this kind may run (`mask` is `NOLOGIN`).
    pub fn is_session_kind(self) -> bool {
        self != RoleKind::Mask
    }

    /// Whether a session of this kind may change rows.
    pub fn can_write(self) -> bool {
        matches!(
            self,
            RoleKind::Owner | RoleKind::Write | RoleKind::MaskedWrite | RoleKind::AppWrite
        )
    }

    /// Whether a session of this kind may change the schema (only `owner`: migrations).
    pub fn can_change_schema(self) -> bool {
        self == RoleKind::Owner
    }

    /// Whether the kind belongs to an app binding (step 3): `app_read` and `app_write`. None of
    /// the frozen kinds of step 2 does.
    pub fn needs_binding(self) -> bool {
        self.is_app()
    }

    /// Whether the kind is an app kind ([`RoleKind::APP_KINDS`]). A platform path (a query, a
    /// migration, a pool) refuses such a kind with [`EngineError::InvalidRole`].
    pub fn is_app(self) -> bool {
        matches!(self, RoleKind::AppRead | RoleKind::AppWrite)
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

    /// The role of an app binding: the only constructor that sets a binding, and only for the
    /// app kinds (`app_read`, `app_write`).
    pub fn app(
        database: DatabaseRef,
        kind: RoleKind,
        binding: Uuid,
    ) -> Result<RoleRef, EngineError> {
        if !kind.is_app() {
            return Err(EngineError::InvalidRole(format!(
                "{} is not an app kind",
                kind.as_str()
            )));
        }
        Ok(RoleRef {
            database,
            kind,
            binding: Some(binding),
        })
    }

    /// Refuses an app role on a platform path (a query or a migration of the platform): the
    /// platform never runs a statement as an app kind.
    pub fn require_platform(&self) -> Result<(), EngineError> {
        if self.kind.is_app() || self.binding.is_some() {
            return Err(EngineError::InvalidRole(format!(
                "{} is an app kind; the platform does not run statements as it",
                self.kind.as_str()
            )));
        }
        Ok(())
    }

    pub fn database(&self) -> &DatabaseRef {
        &self.database
    }

    pub fn kind(&self) -> RoleKind {
        self.kind
    }

    /// The app binding (`database_roles.binding_id`); set exactly for the app kinds.
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
        for kind in RoleKind::APP_KINDS {
            assert_eq!(RoleKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert!(!RoleKind::ALL.contains(&kind));
            assert!(!RoleKind::SESSION_KINDS.contains(&kind));
        }
        assert_eq!(RoleKind::parse("app_read"), Some(RoleKind::AppRead));
        assert_eq!(RoleKind::parse("app_write"), Some(RoleKind::AppWrite));
        assert_eq!(RoleKind::parse("superuser"), None);
    }

    #[test]
    fn app_kinds_need_a_binding_and_only_they_get_one() {
        let binding = Uuid::now_v7();
        for kind in RoleKind::APP_KINDS {
            assert!(matches!(
                RoleRef::new(db(), kind),
                Err(EngineError::InvalidRole(_))
            ));
            let role = RoleRef::app(db(), kind, binding).unwrap();
            assert_eq!(role.kind(), kind);
            assert_eq!(role.binding(), Some(binding));
            assert!(matches!(
                role.require_platform(),
                Err(EngineError::InvalidRole(_))
            ));
            assert!(!kind.can_change_schema());
        }
        for kind in RoleKind::ALL {
            assert!(matches!(
                RoleRef::app(db(), kind, binding),
                Err(EngineError::InvalidRole(_))
            ));
        }
        for kind in RoleKind::SESSION_KINDS {
            RoleRef::new(db(), kind)
                .unwrap()
                .require_platform()
                .unwrap();
        }
        assert!(RoleKind::AppWrite.can_write());
        assert!(!RoleKind::AppRead.can_write());
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
