//! Who did a change, and for whom (`docs/DESIGN.md`, section 3.4).
//!
//! Every write records two principals: `actor` (the user, agent or app that did the write) and
//! `on_behalf_of` (the user, when the actor is an agent or an app).

use crate::authz::Scope;
use crate::error::{Error, Result};
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

    /// True when the actor is a signed-in user acting for themselves (not an agent, an app or a
    /// share-link bearer).
    pub fn is_user(&self) -> bool {
        self.actor.kind == PrincipalKind::User && self.on_behalf_of.is_none()
    }
}

/// The request context that every core operation takes: the tenant and the actor.
/// Every query filters by `tenant`.
#[derive(Clone, Debug)]
pub struct Context {
    pub tenant: TenantId,
    /// The principal (`actor.actor`) and the user it acts for (`actor.on_behalf_of`).
    pub actor: Actor,
    /// The scope of the credentials: the agent scope for an agent, the link's node, role and
    /// expiry for a share-link bearer, the app scope for an app later. `None` for a user. The
    /// effective right is the principal's right intersected with this scope (section 3.3).
    pub scope: Option<Scope>,
    /// The idempotency key of the request, if the client sent one.
    pub idempotency_key: Option<String>,
}

/// Checks that the caller is a signed-in user, not an agent, an app or a share-link bearer, and
/// returns the user. Every operation marked `x-ruum-user-only` in `contracts/openapi.yaml` calls
/// this first: registering and changing agents and their keys, OAuth consent, undo, tenant
/// administration, invitations, the audit log, and the caller's identities and sessions. It fails
/// with `user_required` (403), also when the agent's user would be allowed, so an agent can never
/// widen its own scope or act as a tenant admin.
pub fn require_user_actor(ctx: &Context) -> Result<UserId> {
    if ctx.actor.is_user() && ctx.scope.is_none() {
        Ok(UserId(ctx.actor.actor.id.0))
    } else {
        Err(Error::user_required())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::Role;
    use crate::error::ErrorCode;
    use uuid::Uuid;

    fn ctx(actor: Actor, scope: Option<Scope>) -> Context {
        Context {
            tenant: TenantId(Uuid::now_v7()),
            actor,
            scope,
            idempotency_key: None,
        }
    }

    fn code(r: Result<UserId>) -> Option<ErrorCode> {
        match r {
            Err(Error::Api(e)) => Some(e.code),
            _ => None,
        }
    }

    #[test]
    fn a_user_passes() {
        let user = UserId(Uuid::now_v7());
        assert_eq!(
            require_user_actor(&ctx(Actor::user(user), None)).ok(),
            Some(user)
        );
    }

    #[test]
    fn an_agent_is_refused_even_for_its_owner() {
        let owner = UserId(Uuid::now_v7());
        let agent = Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(owner),
        };
        let scope = Scope {
            subtree: None,
            max_role: Role::Share,
            expires_at: None,
        };
        assert_eq!(agent.effective_user(), Some(owner));
        assert_eq!(
            code(require_user_actor(&ctx(agent, Some(scope)))),
            Some(ErrorCode::UserRequired)
        );
    }

    #[test]
    fn a_share_link_is_refused() {
        let link = Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: None,
        };
        assert_eq!(
            code(require_user_actor(&ctx(link, None))),
            Some(ErrorCode::UserRequired)
        );
    }
}
