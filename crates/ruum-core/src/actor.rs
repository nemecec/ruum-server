//! Who did a change, and for whom (`docs/DESIGN.md`, section 3.4).
//!
//! Every write records two principals: `actor` (the user, agent or app that did the write) and
//! `on_behalf_of` (the user, when the actor is an agent or an app).

use crate::ids::{PrincipalId, TenantId, UserId};
use serde::{Deserialize, Serialize};

/// The kind of a principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "principal_kind", rename_all = "snake_case")]
pub enum PrincipalKind {
    User,
    Group,
    App,
    Agent,
    /// The bearer of a share link.
    ShareLink,
}

/// A reference to a principal: its kind and its ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalRef {
    pub kind: PrincipalKind,
    pub id: PrincipalId,
}

impl PrincipalRef {
    pub fn user(id: UserId) -> Self {
        Self {
            kind: PrincipalKind::User,
            id: id.into(),
        }
    }
}

/// The principals of one request or one write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// The principal that did the write: a user, an agent or an app.
    pub actor: PrincipalRef,
    /// The user for whom the actor acts. `None` when the actor is the user.
    pub on_behalf_of: Option<UserId>,
}

impl Actor {
    /// A user acting for themselves.
    pub fn user(id: UserId) -> Self {
        Self {
            actor: PrincipalRef::user(id),
            on_behalf_of: None,
        }
    }

    /// The user whose rights apply: `on_behalf_of` when set, else the actor itself.
    pub fn effective_user(&self) -> Option<UserId> {
        match (self.on_behalf_of, self.actor.kind) {
            (Some(u), _) => Some(u),
            (None, PrincipalKind::User) => Some(UserId(self.actor.id.0)),
            _ => None,
        }
    }

    /// True when an agent did the write.
    pub fn is_agent(&self) -> bool {
        self.actor.kind == PrincipalKind::Agent
    }
}

/// The request context that every core operation takes: the tenant and the actor.
/// Every query filters by `tenant`.
#[derive(Clone, Debug)]
pub struct Context {
    pub tenant: TenantId,
    pub actor: Actor,
    /// The agent scope, when the actor is an agent. The effective right is the user's right
    /// intersected with this scope (section 3.3).
    pub scope: Option<crate::agents::AgentScope>,
    /// The idempotency key of the request, if the client sent one.
    pub idempotency_key: Option<String>,
}
