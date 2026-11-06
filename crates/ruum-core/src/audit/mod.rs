//! The audit log per tenant (`docs/DESIGN.md`, section 15): auth events, membership changes,
//! agent and key changes, sharing changes and node changes from the journal. Exportable.
//! Every entry records `actor` and `on_behalf_of`.
//!
//! Two sources make up the log:
//!
//! - the `audit` table: events that are not node changes (sign-in, key use, members, agents,
//!   keys, tenant settings). Other areas write them with [`AuditLog::record`] or the helpers in
//!   [`events`];
//! - the journal: every node, version, tag, grant and share-link change. [`AuditLog::list`]
//!   merges them in, with the event type as the action (for example `grant.added`), target
//!   `node`, and the journal `seq` in `data.seq`. Their `id` is [`journal_audit_id`] of the `seq`.
//!
//! Only tenant owners and admins read the log ([`require_admin`]).

pub mod events;
pub mod memory;
pub mod postgres;

#[cfg(test)]
mod conformance;

use crate::actor::{Actor, Context, require_user_actor};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::UserId;
use crate::journal::JournalEntry;
use crate::paging::{Cursor, Page, PageRequest};
use crate::tenants::{MemberRole, TenantStore};
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// One audit entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    /// A dotted action name, for example `auth.login`, `agent.key_created`, `grant.added`.
    pub action: String,
    #[serde(flatten)]
    pub actor: Actor,
    /// The type of the target, for example `node`, `agent`, `member`.
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub data: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// A new entry. The tenant and the actor come from the [`Context`] of [`AuditLog::record`].
#[derive(Clone, Debug, Default)]
pub struct NewAuditEntry {
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    /// The client IP address (IPv4 or IPv6). A value that is not an IP address is dropped.
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub data: serde_json::Value,
}

impl NewAuditEntry {
    /// An entry with this action and no target.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            data: serde_json::json!({}),
            ..Default::default()
        }
    }

    /// Sets the target, for example `("agent", agent_id)`.
    pub fn target(mut self, kind: impl Into<String>, id: impl ToString) -> Self {
        self.target_type = Some(kind.into());
        self.target_id = Some(id.to_string());
        self
    }

    /// Sets the client IP address and user agent.
    pub fn client(mut self, ip: Option<String>, user_agent: Option<String>) -> Self {
        self.ip = ip;
        self.user_agent = user_agent;
        self
    }

    /// Sets the action-specific data (a JSON object).
    pub fn data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }

    /// The entry as stored: an IP that does not parse is dropped, the user agent is cut to 512
    /// characters, and data that is not an object is wrapped as `{ "value": ... }`.
    pub(crate) fn normalized(mut self) -> Result<Self> {
        if self.action.is_empty() || self.action.len() > 100 {
            return Err(Error::Api(ApiError::new(
                ErrorCode::ValidationFailed,
                "an audit action has 1 to 100 characters",
            )));
        }
        self.ip = self
            .ip
            .and_then(|ip| ip.trim().parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string());
        self.user_agent = self
            .user_agent
            .map(|ua| ua.chars().take(512).collect::<String>());
        if !self.data.is_object() {
            self.data = if self.data.is_null() {
                serde_json::json!({})
            } else {
                serde_json::json!({ "value": self.data })
            };
        }
        Ok(self)
    }
}

/// Filters for the audit log.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuditQuery {
    /// Only entries at or after this time.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub since: Option<OffsetDateTime>,
    /// Only entries at or before this time.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub until: Option<OffsetDateTime>,
    /// Only entries by this actor (user, agent or app ID).
    pub actor: Option<Uuid>,
    /// Only entries whose action starts with this prefix, for example `agent.`.
    pub action: Option<String>,
    #[serde(flatten)]
    pub page: PageRequest,
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    /// Stores one entry in the tenant of `ctx`, with the actor of `ctx`.
    async fn record(&self, ctx: &Context, entry: NewAuditEntry) -> Result<()>;
    /// Newest first, the `audit` table and the journal merged. No permission check: the caller
    /// checks [`require_admin`] first.
    async fn list(&self, ctx: &Context, query: AuditQuery) -> Result<Page<AuditEntry>>;
}

/// Checks that the caller is a signed-in user (not an agent or a share link) with the tenant
/// role `admin` or `owner` in `ctx.tenant`. Others get `user_required` or `permission_denied`.
pub async fn require_admin(tenants: &dyn TenantStore, ctx: &Context) -> Result<UserId> {
    let user = require_user_actor(ctx)?;
    match tenants.role_of(ctx.tenant, user).await? {
        Some(r) if r >= MemberRole::Admin => Ok(user),
        _ => Err(Error::Api(
            ApiError::new(
                ErrorCode::PermissionDenied,
                "only tenant owners and admins can read the audit log",
            )
            .with_hint("ask a tenant admin"),
        )),
    }
}

/// The audit `id` of a journal entry: a fixed UUID pattern with the `seq` in the last 48 bits
/// (`00000000-0000-8000-8000-<seq>`). Unique within the tenant.
pub fn journal_audit_id(seq: i64) -> Uuid {
    Uuid::from_u128((0x8000u128 << 64) | (0x8000u128 << 48) | (seq as u128 & 0xffff_ffff_ffff))
}

/// A journal entry as an audit entry.
pub fn from_journal(e: &JournalEntry) -> AuditEntry {
    let mut data = match &e.data {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    data.insert("seq".into(), e.seq.into());
    if let Some(r) = e.revision {
        data.insert("revision".into(), r.into());
    }
    AuditEntry {
        id: journal_audit_id(e.seq),
        action: e.event.as_str().to_string(),
        actor: e.actor,
        target_type: e.node_id.map(|_| "node".to_string()),
        target_id: e.node_id.map(|n| n.to_string()),
        ip: None,
        user_agent: None,
        data: serde_json::Value::Object(data),
        at: e.at,
    }
}

const CURSOR_PREFIX: &str = "a1.";

/// The cursor after an entry: its time and id (the order is `at` desc, `id` desc).
pub(crate) fn encode_cursor(at: OffsetDateTime, id: Uuid) -> Cursor {
    Cursor(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{CURSOR_PREFIX}{}.{id}", at.unix_timestamp_nanos())),
    )
}

/// The position of an audit cursor. Fails with `bad_request` for a cursor this server did not
/// make.
pub(crate) fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid)> {
    let bad = || {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                .with_hint("pass the next_cursor of the last page, or no cursor"),
        )
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .map_err(|_| bad())?;
    let text = String::from_utf8(bytes).map_err(|_| bad())?;
    let rest = text.strip_prefix(CURSOR_PREFIX).ok_or_else(bad)?;
    let (nanos, id) = rest.split_once('.').ok_or_else(bad)?;
    let nanos: i128 = nanos.parse().map_err(|_| bad())?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    Ok((at, id))
}

/// Whether an entry passes the filters of the query (not the cursor).
pub(crate) fn matches(e: &AuditEntry, q: &AuditQuery) -> bool {
    q.since.is_none_or(|s| e.at >= s)
        && q.until.is_none_or(|u| e.at <= u)
        && q.actor.is_none_or(|a| e.actor.actor.id.0 == a)
        && q.action.as_deref().is_none_or(|p| e.action.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_cursors_round_trip() {
        let at = crate::now();
        let id = Uuid::now_v7();
        let (a, i) = decode_cursor(&encode_cursor(at, id)).unwrap();
        assert_eq!((a, i), (at, id));
        assert!(decode_cursor(&Cursor("nope".into())).is_err());
        assert!(decode_cursor(&crate::journal::encode_cursor(3)).is_err());
    }

    #[test]
    fn journal_ids_carry_the_seq() {
        assert_eq!(
            journal_audit_id(255).to_string(),
            "00000000-0000-8000-8000-0000000000ff"
        );
        assert!(journal_audit_id(1) < journal_audit_id(2));
    }

    #[test]
    fn new_entries_are_normalized() {
        let e = NewAuditEntry::new("auth.login")
            .client(Some(" 192.0.2.1 ".into()), Some("x".repeat(600)))
            .data(serde_json::json!(5))
            .normalized()
            .unwrap();
        assert_eq!(e.ip.as_deref(), Some("192.0.2.1"));
        assert_eq!(e.user_agent.unwrap().len(), 512);
        assert_eq!(e.data, serde_json::json!({ "value": 5 }));
        let e = NewAuditEntry::new("auth.login")
            .client(Some("not an ip".into()), None)
            .normalized()
            .unwrap();
        assert_eq!(e.ip, None);
        assert!(NewAuditEntry::new("").normalized().is_err());
    }
}
