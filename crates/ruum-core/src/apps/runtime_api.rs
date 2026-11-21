//! The core side of the runtime API (card R-3; `contracts/apps/runtime.openapi.yaml`, decision
//! 0006-1 and 0006-10): the wire types of the core's internal routes for `ruum-runtime-api`
//! (`POST /internal/runtime/v1/...` on the internal listener, [`wire`]) and the service that
//! answers them ([`RuntimeApiService`]).
//!
//! The runtime API holds no tenant keys, no signing key and no database credential, so it asks
//! the core. It checks the app's token itself and then passes it on: every internal route takes
//! the app's workload token (or, for `content`, `children` and `principal`, its delegated token)
//! as `Authorization: Bearer`, and the core checks it again with the workload key
//! ([`RuntimeApiService::verify`]). The caller ([`wire::Caller`]) and the delegation of a
//! request come only from that token: a request without a valid token is refused `401`, and so
//! is one whose body names another caller than its token. So whoever reaches the core's internal
//! listener (the gateway, the edge, any pod that the network policy lets through) still acts
//! only as the app whose token it holds; the core never takes a principal or a tenant from
//! anything the app or a peer sent. Without a workload key (`Services::workload_tokens`), every
//! route refuses (fail closed). Every store read here filters by the caller's tenant.
//!
//! # The principal still serves
//!
//! [`RuntimeApiService::serving`]: the live principal (the app node ID) serves while its app
//! row exists, the token's deployment (`dep`) is the app's live deployment and is `live`, the
//! app node is not in the trash (read at once, without waiting for the provider hook), and the
//! app is not suspended; a preview principal (the deployment ID) serves while the deployment is
//! a ready preview of that app that has not expired (a superseded preview is no longer
//! `ready`). The runtime API keeps an answer for at most 3 seconds, so a trashed app, a replaced
//! live deployment and a stale preview stop within 5 seconds.
//!
//! # Bound content
//!
//! A read ([`RuntimeApiService::content`], [`RuntimeApiService::children`]) is allowed when all
//! of these hold, in this order:
//!
//! 1. The node exists in the caller's tenant ([`crate::nodes::NodeStore::ancestors`], no
//!    permission check): else `not_found`. A node of another tenant takes the same path and
//!    gets the same answer as a missing node.
//! 2. An active binding row of the principal (a `node` binding; with a delegated token only the
//!    rows it names that have `on_behalf_of_user`) has the node as its target, or one of the
//!    node's ancestors (the path from the node up to the root, bounded by the tree depth):
//!    else `denied` (`403`).
//! 3. SpiceDB says the app principal may `view` the node (`node:N#viewer@app:<principal>` on
//!    the target, inherited below a folder): else `denied`. So a binding whose tuple is gone
//!    stops at once, and so does a tuple without a row.
//! 4. The node is not in the trash (the node store answers a trashed node only to `manage`):
//!    else `not_found`.
//! 5. With a delegated token, the user (an agent: its user, within its scope) may `view` the
//!    node too ([`crate::authz::require`]): else `not_found` (or `denied` when the user sees it
//!    without the right). The user's right intersected with the binding.
//!
//! A listing with a delegated token keeps only the children the user may see: one bulk check
//! ([`crate::authz::Authz::check_bulk`]) per page of at most [`MAX_PAGE`] children, never one
//! call per child. Links are left out of listings (an app reads no link target).
//!
//! # Delegated tokens and the user
//!
//! The user token names the user by the pairwise subject of this app. The exchange maps it back
//! ([`RuntimeApiService::exchange`]): it pages through the people of the tenant (members and
//! guests, `list_members` with the context of the person who made the caller's deployment,
//! backlog P-69) and compares each one's pairwise subject, at most [`MAX_PEOPLE_SCANNED`]
//! people, and keeps what it found for [`SUBJECT_TTL`] (per tenant at most
//! [`SUBJECTS_PER_TENANT`] entries). The delegated token then carries the user as a **sealed
//! subject**: the platform user ID encrypted with a key that only the core has (AES-256 of one
//! block, the key derived from the tenant's pseudonym key and the app), formatted as a UUID. So
//! `on_behalf_of` has the UUID format of `delegated-token.schema.json`, the runtime API and the
//! app never see a platform user ID, and a read with the token needs no second lookup. The
//! agent of an `agent` actor is sealed the same way (`agent`).

use crate::Services;
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::apps::AppService;
use crate::apps::runtime_tokens::{Credential, Workload};
use crate::apps::store::{AppRow, BindingKind, BindingQuery, BindingRow, DeploymentRow};
use crate::apps::store::{DeploymentState, Environment as RecordEnv};
use crate::authz::{self, ObjectRef, Role, SubjectRef};
use crate::blobs::Content;
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::identity::app_sign_in::{AppEnv, pairwise};
use crate::ids::{AgentId, NodeId, PrincipalId, TenantId, UserId};
use crate::nodes::{ChildOrder, Node, NodeKind};
use crate::paging::{Cursor, PageRequest};
use crate::providers::ProviderState;
use aes_gcm::aes::Aes256;
use aes_gcm::aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;
use wire::*;

#[cfg(test)]
mod tests;

/// The most items of a page of holders or children (the contract's `maximum`).
pub const MAX_PAGE: u32 = 200;
/// The binding rows of one principal that one read looks at (a manifest has at most 30).
const ROWS_PER_PRINCIPAL: u32 = 500;
/// The people one exchange looks at, at most.
pub const MAX_PEOPLE_SCANNED: usize = 20_000;
/// The people of one page of that scan.
const PEOPLE_PER_PAGE: u32 = 500;
/// The agents of one user that an exchange looks at, at most.
const MAX_AGENTS_SCANNED: u32 = 200;
/// How long a found pairwise subject is kept.
pub const SUBJECT_TTL: Duration = Duration::from_secs(600);
/// Kept subjects per tenant.
pub const SUBJECTS_PER_TENANT: usize = 4096;
/// Kept subjects in all; over it, the tenant with the most entries loses its oldest first.
pub const SUBJECTS_MAX: usize = 100_000;

/// The wire types of the core's internal routes for the runtime API.
pub mod wire {
    use crate::identity::app_sign_in::AppEnv;
    use crate::ids::{NodeId, TenantId};
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// The path prefix of the runtime API's internal routes.
    pub const PREFIX: &str = "/internal/runtime/v1";

    /// The verified principal of a request: from the workload token (and for a delegated token,
    /// from its claims), as the runtime API checked it. Never from the app's request.
    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Caller {
        pub tenant_id: TenantId,
        pub app_id: NodeId,
        /// The app node ID (live) or the deployment ID (preview).
        pub principal: Uuid,
        /// The deployment the token was made for (`dep`; a preview's is its principal).
        pub deployment_id: Uuid,
        pub environment: AppEnv,
    }

    /// A binding of `AppSelf`.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SelfBinding {
        pub name: String,
        /// `node`, `database_branch` or `api`.
        pub kind: String,
        pub target_node_id: NodeId,
        pub role: Option<String>,
        pub address: Option<String>,
    }

    /// `AppSelf` of the contract.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AppSelf {
        pub app_id: NodeId,
        pub deployment_id: Uuid,
        pub environment: AppEnv,
        pub tenant_slug: String,
        pub bindings: Vec<SelfBinding>,
        pub roles: Vec<String>,
    }

    /// `POST /principal`: does the principal still serve, and who is it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum PrincipalAnswer {
        Serving { app: AppSelf },
        Gone,
    }

    /// `POST /holders`: one page of the holders of a role.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct HoldersQuery {
        pub caller: Caller,
        pub role: String,
        pub cursor: Option<String>,
        pub limit: u32,
    }

    /// One holder (`AppUserPage.items`).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Holder {
        pub sub: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pub everyone: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum HoldersAnswer {
        Page {
            items: Vec<Holder>,
            next_cursor: Option<String>,
        },
        NoRole,
        BadCursor,
        Gone,
    }

    /// `POST /exchange`: the bindings a user may use through this app, and the sealed user.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ExchangeQuery {
        pub caller: Caller,
        /// The user token's `sub` (a pairwise subject).
        pub sub: String,
        /// `user` or `agent`: the runtime API refuses every other actor before it asks (the
        /// actor rule is the runtime API's; an `agent` is mapped through `on_behalf_of`, any
        /// other value as a person).
        pub actor: String,
        /// The user token's `on_behalf_of` (the user of an agent).
        pub on_behalf_of: Option<String>,
        /// The user token's `jti`: the core grants one exchange per `jti` across every runtime
        /// API process and restart (Kimi review of run 14, R-3 finding 2).
        pub jti: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum ExchangeAnswer {
        Granted {
            /// The names of the bindings with `on_behalf_of_user`, sorted.
            bindings: Vec<String>,
            /// The sealed user (see the module docs).
            user: Uuid,
            /// The sealed agent of an `agent` actor.
            agent: Option<Uuid>,
            /// The delegated token, signed by the core with the workload key.
            access_token: String,
        },
        /// No binding allows acting for the user.
        NoBinding,
        /// The token's subject is not a person (or agent) of the tenant.
        UnknownUser,
        /// The user token was exchanged before (by any runtime API process).
        Replayed,
        Gone,
    }

    /// What a delegated token allows: its bindings and its sealed user (and agent).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Delegation {
        pub bindings: Vec<String>,
        pub user: Uuid,
        pub agent: Option<Uuid>,
    }

    /// `POST /content` and `POST /children`.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ReadQuery {
        pub caller: Caller,
        pub node_id: NodeId,
        pub delegation: Option<Delegation>,
        #[serde(default)]
        pub cursor: Option<String>,
        #[serde(default)]
        pub limit: Option<u32>,
    }

    /// One child (`BoundChildPage.items`).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Child {
        pub node_id: NodeId,
        pub name: String,
        pub kind: String,
        pub size: Option<i64>,
        pub version_id: Option<Uuid>,
        pub updated_at: String,
    }

    /// `BoundChildPage`.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ChildPage {
        pub items: Vec<Child>,
        pub next_cursor: Option<String>,
    }

    /// Why a read is refused. The internal route answers it as a status: `404`, `403`, `410`
    /// (the principal no longer serves) and `400` (a bad cursor).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Refusal {
        NotFound,
        Denied,
        Gone,
        BadCursor,
    }

    impl Refusal {
        pub fn status(self) -> u16 {
            match self {
                Refusal::NotFound => 404,
                Refusal::Denied => 403,
                Refusal::Gone => 410,
                Refusal::BadCursor => 400,
            }
        }
    }

    /// `POST /topic`.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TopicQuery {
        pub caller: Caller,
        pub topic: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    pub enum TopicAnswer {
        Declared,
        Undeclared,
        Gone,
    }
}

// ------------------------------------------------------------------------------- sealing

/// The key that seals the subjects of one app (`what`: `user` or `agent`).
fn sealing_key(tenant_key: &[u8; 32], app: NodeId, what: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("ruum runtime-api delegated subject v1");
    h.update(tenant_key);
    h.update(app.0.as_bytes());
    h.update(what.as_bytes());
    *h.finalize().as_bytes()
}

/// Seals `id` (one AES-256 block): a UUID-shaped value that only the core opens.
pub fn seal(key: &[u8; 32], id: Uuid) -> Uuid {
    let cipher = Aes256::new(&Array::from(*key));
    let mut block = Array::from(*id.as_bytes());
    cipher.encrypt_block(&mut block);
    Uuid::from_bytes(block.into())
}

/// Opens a value of [`seal`].
pub fn open(key: &[u8; 32], sealed: Uuid) -> Uuid {
    let cipher = Aes256::new(&Array::from(*key));
    let mut block = Array::from(*sealed.as_bytes());
    cipher.decrypt_block(&mut block);
    Uuid::from_bytes(block.into())
}

// ------------------------------------------------------------------------------- subjects

/// Kept subjects of one tenant: (app, pairwise subject) -> (platform ID, when found).
type TenantSubjects = HashMap<(NodeId, String), (Uuid, Instant)>;

#[derive(Default)]
struct Subjects {
    by_tenant: HashMap<TenantId, TenantSubjects>,
    total: usize,
}

impl Subjects {
    fn get(&self, tenant: TenantId, app: NodeId, sub: &str, now: Instant) -> Option<Uuid> {
        self.by_tenant
            .get(&tenant)?
            .get(&(app, sub.to_string()))
            .filter(|(_, at)| now.saturating_duration_since(*at) < SUBJECT_TTL)
            .map(|(id, _)| *id)
    }

    fn evict_oldest(&mut self, tenant: TenantId) {
        let Some(m) = self.by_tenant.get_mut(&tenant) else {
            return;
        };
        if let Some(k) = m
            .iter()
            .min_by_key(|(_, (_, at))| *at)
            .map(|(k, _)| k.clone())
        {
            m.remove(&k);
            self.total -= 1;
        }
        if m.is_empty() {
            self.by_tenant.remove(&tenant);
        }
    }

    fn put(&mut self, tenant: TenantId, app: NodeId, sub: &str, id: Uuid, now: Instant) {
        let len = self.by_tenant.get(&tenant).map_or(0, HashMap::len);
        if len >= SUBJECTS_PER_TENANT {
            if let Some(m) = self.by_tenant.get_mut(&tenant) {
                let before = m.len();
                m.retain(|_, (_, at)| now.saturating_duration_since(*at) < SUBJECT_TTL);
                self.total -= before - m.len();
            }
            if self.by_tenant.get(&tenant).map_or(0, HashMap::len) >= SUBJECTS_PER_TENANT {
                self.evict_oldest(tenant);
            }
        }
        while self.total >= SUBJECTS_MAX {
            let Some(largest) = self
                .by_tenant
                .iter()
                .max_by_key(|(_, m)| m.len())
                .map(|(t, _)| *t)
            else {
                break;
            };
            self.evict_oldest(largest);
        }
        let old = self
            .by_tenant
            .entry(tenant)
            .or_default()
            .insert((app, sub.to_string()), (id, now));
        if old.is_none() {
            self.total += 1;
        }
    }
}

// ------------------------------------------------------------------------------- the service

/// What a read found: the node, read as the app principal.
pub struct BoundNode {
    pub node: Node,
    pub app_ctx: Context,
}

/// The core service of the runtime API's internal routes (see the module docs).
#[derive(Clone)]
pub struct RuntimeApiService {
    s: Services,
    subjects: Arc<Mutex<Subjects>>,
}

/// The longest `jti` of a user token that an exchange takes (the gateway's are UUIDs).
pub const MAX_JTI_BYTES: usize = 128;

/// The context of the app principal: the actor is `app:<principal>`.
pub fn principal_ctx(c: &Caller) -> Context {
    Context {
        tenant: c.tenant_id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::App,
                id: PrincipalId(c.principal),
            },
            on_behalf_of: None,
        },
        scope: None,
        idempotency_key: None,
    }
}

fn app_subject(principal: Uuid) -> SubjectRef {
    SubjectRef::object(ObjectRef::new("app", principal))
}

fn record_env(e: AppEnv) -> RecordEnv {
    match e {
        AppEnv::Live => RecordEnv::Live,
        AppEnv::Preview => RecordEnv::Preview,
    }
}

/// The caller of a verified token.
pub fn caller_of(w: &Workload) -> Caller {
    Caller {
        tenant_id: w.tenant,
        app_id: w.app,
        principal: w.principal,
        deployment_id: w.deployment,
        environment: w.environment,
    }
}

/// The principal of a caller, as a token names it.
pub fn workload_of(c: &Caller) -> Workload {
    Workload {
        tenant: c.tenant_id,
        app: c.app_id,
        principal: c.principal,
        deployment: c.deployment_id,
        environment: c.environment,
    }
}

/// The caller and the delegation of a verified credential.
pub fn admitted(cred: &Credential) -> (Caller, Option<Delegation>) {
    match cred {
        Credential::Workload(w) => (caller_of(w), None),
        Credential::Delegated(d) => (
            caller_of(&d.caller),
            Some(Delegation {
                bindings: d.bindings.clone(),
                user: d.user,
                agent: d.agent,
            }),
        ),
    }
}

/// Whether a request body names the caller of its token: the caller of a route is always the
/// token's (`derived`), and a body that names another one is refused.
pub fn same_caller(derived: &Caller, sent: &Caller) -> bool {
    derived == sent
}

fn is_not_found(e: &Error) -> bool {
    matches!(e, Error::Api(a) if a.code == ErrorCode::NotFound)
}

/// Whether `topic` is declared by an `events` API of `manifest`. The manifest has no list of
/// topics in step 3 (they are in the AsyncAPI file of the archive, which card R-6 reads); until
/// then a topic is declared by the `events` API whose name is the topic or its first part
/// (`orders` declares `orders` and `orders.created`).
pub fn topic_declared(manifest: &Value, topic: &str) -> bool {
    manifest
        .get("apis")
        .and_then(Value::as_object)
        .is_some_and(|apis| {
            apis.iter().any(|(name, api)| {
                api.get("protocol").and_then(Value::as_str) == Some("events")
                    && (topic == name
                        || topic
                            .strip_prefix(name.as_str())
                            .is_some_and(|rest| rest.starts_with('.')))
            })
        })
}

/// The active binding rows of the caller's principal. The store filters by the caller's tenant
/// (its statement's `tenant_id = $1`; the mutation `runtime-api-tenant-filter-postgres` drops
/// it), the app and the principal; this keeps only rows of that app and principal.
pub async fn bound_rows(
    store: &dyn crate::apps::store::AppStore,
    c: &Caller,
) -> Result<Vec<BindingRow>> {
    store
        .list_bindings(
            c.tenant_id,
            &BindingQuery {
                app: Some(c.app_id),
                principal: Some(c.principal),
                active_only: true,
                limit: ROWS_PER_PRINCIPAL,
                ..Default::default()
            },
        )
        .await
        .map(|rows| {
            rows.into_iter()
                .filter(|r| {
                    r.principal_id == c.principal && r.app_node_id == c.app_id && r.is_active()
                })
                .collect()
        })
}

impl RuntimeApiService {
    pub fn new(s: &Services) -> Self {
        Self {
            s: s.clone(),
            subjects: Arc::default(),
        }
    }

    fn apps(&self) -> &Arc<AppService> {
        &self.s.apps
    }

    /// Raises the request's freshness floor to the tenant's newest relationship write, so a
    /// revoke made on another replica counts here at once.
    async fn fresh(&self, tenant: TenantId) -> Result<()> {
        if let Some(t) = self.s.freshness.tenant_token(tenant).await? {
            authz::raise_request_floor(&t);
        }
        Ok(())
    }

    /// Checks the app's token of an internal route (see the module docs): a workload token, or
    /// (when `delegated`) also a delegated token, of the core's workload key. `None` for a bad
    /// token and when the core has no workload key. Log lines name the reason, never a value.
    pub fn verify(&self, token: &str, delegated: bool) -> Option<(Caller, Option<Delegation>)> {
        let Some(issuer) = &self.s.workload_tokens else {
            tracing::warn!(
                target: "ruum::runtime_api",
                "a runtime route was refused: the core has no workload token key"
            );
            return None;
        };
        match issuer.verify(token, delegated) {
            Ok(c) => Some(admitted(&c)),
            Err(e) => {
                tracing::info!(target: "ruum::runtime_api", reason = %e, "a runtime route refused a token");
                None
            }
        }
    }

    /// The app row and the deployment of a principal that still serves (see the module docs).
    pub async fn serving(&self, c: &Caller) -> Result<Option<(AppRow, DeploymentRow)>> {
        let store = self.apps().store();
        let Some(app) = store.get_app(c.tenant_id, c.app_id).await? else {
            return Ok(None);
        };
        let dep_id = match c.environment {
            // A live token serves only while its deployment is the live one.
            AppEnv::Live if c.principal == c.app_id.0 => match app.live_deployment_id {
                Some(d) if d == c.deployment_id => d,
                _ => return Ok(None),
            },
            AppEnv::Live => return Ok(None),
            AppEnv::Preview if c.deployment_id == c.principal => c.principal,
            AppEnv::Preview => return Ok(None),
        };
        let Some(dep) = store.get_deployment(c.tenant_id, c.app_id, dep_id).await? else {
            return Ok(None);
        };
        let now = crate::now();
        let serves = dep.tenant_id == c.tenant_id
            && dep.app_node_id == c.app_id
            && dep.environment == record_env(c.environment)
            && match dep.environment {
                RecordEnv::Live => {
                    dep.state == DeploymentState::Live && app.live_deployment_id == Some(dep.id)
                }
                RecordEnv::Preview => {
                    dep.state == DeploymentState::Ready && dep.expires_at.is_none_or(|t| t > now)
                }
            };
        if !serves {
            return Ok(None);
        }
        // The trash at once (the provider hook that suspends the app runs later).
        match self.s.nodes.is_trashed(&principal_ctx(c), c.app_id).await {
            Ok(false) => {}
            Ok(true) => return Ok(None),
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(e),
        }
        if let Some(t) = self.s.typed_nodes.get(c.tenant_id, c.app_id).await?
            && matches!(
                t.state,
                ProviderState::Suspended | ProviderState::Deleted | ProviderState::Failed
            )
        {
            return Ok(None);
        }
        Ok(Some((app, dep)))
    }

    /// The active rows of the principal ([`bound_rows`]).
    pub async fn active_rows(&self, c: &Caller) -> Result<Vec<BindingRow>> {
        bound_rows(&**self.apps().store(), c).await
    }

    /// `getAppSelf` (`POST /principal`).
    pub async fn principal(&self, c: &Caller) -> Result<PrincipalAnswer> {
        self.fresh(c.tenant_id).await?;
        let Some((_, dep)) = self.serving(c).await? else {
            return Ok(PrincipalAnswer::Gone);
        };
        let tenant_slug = self
            .apps()
            .store()
            .tenant_settings(c.tenant_id)
            .await?
            .map(|t| t.slug)
            .unwrap_or_default();
        let mut bindings: Vec<SelfBinding> = self
            .active_rows(c)
            .await?
            .iter()
            .map(|b| SelfBinding {
                name: b.name.clone(),
                kind: b.kind.as_str().to_string(),
                target_node_id: b.target_node_id,
                // The manifest's names (`view`, `edit`), not the graph's relations.
                role: b.role.map(|r| r.permission().to_string()),
                address: None,
            })
            .collect();
        bindings.sort_by(|a, b| a.name.cmp(&b.name));
        let roles = dep
            .manifest
            .as_ref()
            .map(super::roles::manifest_roles)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect();
        Ok(PrincipalAnswer::Serving {
            app: AppSelf {
                app_id: c.app_id,
                deployment_id: dep.id,
                environment: c.environment,
                tenant_slug,
                bindings,
                roles,
            },
        })
    }

    /// The context that may list the people of the tenant (backlog P-69): the person who made
    /// the caller's deployment (`created_by`: who deployed it, live or preview; the user of an
    /// agent that deployed it). `None` when that is not a person.
    fn lister_ctx(c: &Caller, dep: &DeploymentRow) -> Option<Context> {
        dep.created_by.effective_user().map(|u| Context {
            tenant: c.tenant_id,
            actor: Actor::user(u),
            scope: None,
            idempotency_key: None,
        })
    }

    /// `listAppSelfUsers` (`POST /holders`).
    pub async fn holders(&self, q: &HoldersQuery) -> Result<HoldersAnswer> {
        let c = &q.caller;
        self.fresh(c.tenant_id).await?;
        let Some((_, dep)) = self.serving(c).await? else {
            return Ok(HoldersAnswer::Gone);
        };
        let own = dep
            .manifest
            .as_ref()
            .map(super::roles::manifest_roles)
            .unwrap_or_default();
        if !own.iter().any(|r| r.name == q.role) {
            return Ok(HoldersAnswer::NoRole);
        }
        // A preview sees the roles of its own manifest; the grants follow the live manifest, so
        // a role only the preview declares has no holder yet.
        let live = self.apps().live_roles(c.tenant_id, c.app_id).await?;
        if !live.iter().any(|r| r.name == q.role) {
            return Ok(HoldersAnswer::Page {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let Some(ctx) = Self::lister_ctx(c, &dep) else {
            return Ok(HoldersAnswer::Page {
                items: Vec::new(),
                next_cursor: None,
            });
        };
        let page = match self
            .apps()
            .role_holders(
                &ctx,
                c.app_id,
                &q.role,
                PageRequest {
                    cursor: q.cursor.clone().map(Cursor),
                    limit: Some(q.limit.clamp(1, MAX_PAGE)),
                },
            )
            .await
        {
            Ok(p) => p,
            Err(Error::Api(a)) if a.code == ErrorCode::BadRequest => {
                return Ok(HoldersAnswer::BadCursor);
            }
            Err(Error::Api(a)) if a.code == ErrorCode::NotFound => {
                return Ok(HoldersAnswer::NoRole);
            }
            // The lister may no longer list the people (a guest, or gone): only the grants.
            Err(Error::Api(a)) if a.code == ErrorCode::PermissionDenied => {
                return Ok(HoldersAnswer::Page {
                    items: Vec::new(),
                    next_cursor: None,
                });
            }
            Err(e) => return Err(e),
        };
        let key = self.s.tenant_keys.tenant_key(c.tenant_id).await?;
        let items = page
            .items
            .into_iter()
            .map(|h| match h {
                super::roles::Holder::User(u) => Holder {
                    sub: pairwise(key.as_bytes(), c.app_id, u.0),
                    everyone: false,
                },
                super::roles::Holder::Everyone => Holder {
                    sub: "everyone:tenant".into(),
                    everyone: true,
                },
            })
            .collect();
        Ok(HoldersAnswer::Page {
            items,
            next_cursor: page.next_cursor.map(|c| c.0),
        })
    }

    /// The person of the tenant whose pairwise subject for `app` is `sub` (see the module docs).
    async fn find_person(
        &self,
        c: &Caller,
        lister: &Context,
        key: &[u8; 32],
        sub: &str,
    ) -> Result<Option<UserId>> {
        let now = Instant::now();
        if let Some(u) = self.subjects.lock().unwrap_or_else(|e| e.into_inner()).get(
            c.tenant_id,
            c.app_id,
            sub,
            now,
        ) {
            return Ok(Some(UserId(u)));
        }
        let mut cursor = None;
        let mut scanned = 0usize;
        loop {
            let page = match self
                .s
                .tenants
                .list_members(
                    lister,
                    PageRequest {
                        cursor,
                        limit: Some(PEOPLE_PER_PAGE),
                    },
                )
                .await
            {
                Ok(p) => p,
                Err(Error::Api(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            for m in &page.items {
                if pairwise(key, c.app_id, m.user_id.0) == sub {
                    self.subjects.lock().unwrap_or_else(|e| e.into_inner()).put(
                        c.tenant_id,
                        c.app_id,
                        sub,
                        m.user_id.0,
                        now,
                    );
                    return Ok(Some(m.user_id));
                }
            }
            scanned += page.items.len();
            match page.next_cursor {
                Some(next) if scanned < MAX_PEOPLE_SCANNED && !page.items.is_empty() => {
                    cursor = Some(next)
                }
                _ => return Ok(None),
            }
        }
    }

    /// The agent of `user` whose pairwise subject for `app` is `sub`.
    async fn find_agent(
        &self,
        c: &Caller,
        user: UserId,
        key: &[u8; 32],
        sub: &str,
    ) -> Result<Option<AgentId>> {
        let ctx = Context {
            tenant: c.tenant_id,
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        };
        let page = match self
            .s
            .agents
            .list(
                &ctx,
                Some(user),
                PageRequest {
                    cursor: None,
                    limit: Some(MAX_AGENTS_SCANNED),
                },
            )
            .await
        {
            Ok(p) => p,
            Err(Error::Api(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(page
            .items
            .iter()
            .find(|a| a.owner_id == user && pairwise(key, c.app_id, a.id.0) == sub)
            .map(|a| a.id))
    }

    /// `exchangeAppUserToken` (`POST /exchange`): see the module docs.
    pub async fn exchange(&self, q: &ExchangeQuery) -> Result<ExchangeAnswer> {
        let c = &q.caller;
        self.fresh(c.tenant_id).await?;
        let Some((_, dep)) = self.serving(c).await? else {
            return Ok(ExchangeAnswer::Gone);
        };
        if !self.first_exchange(c, &q.jti).await? {
            return Ok(ExchangeAnswer::Replayed);
        }
        let mut bindings: Vec<String> = self
            .active_rows(c)
            .await?
            .into_iter()
            .filter(|r| r.kind == BindingKind::Node && r.on_behalf_of_user)
            .map(|r| r.name)
            .collect();
        bindings.sort();
        bindings.dedup();
        if bindings.is_empty() {
            return Ok(ExchangeAnswer::NoBinding);
        }
        let Some(lister) = Self::lister_ctx(c, &dep) else {
            return Ok(ExchangeAnswer::UnknownUser);
        };
        let key = self.s.tenant_keys.tenant_key(c.tenant_id).await?;
        let key = key.as_bytes();
        let (user_sub, agent_sub) = if q.actor == "agent" {
            match &q.on_behalf_of {
                Some(u) => (u.as_str(), Some(q.sub.as_str())),
                None => return Ok(ExchangeAnswer::UnknownUser),
            }
        } else {
            (q.sub.as_str(), None)
        };
        let Some(user) = self.find_person(c, &lister, key, user_sub).await? else {
            return Ok(ExchangeAnswer::UnknownUser);
        };
        let agent = match agent_sub {
            None => None,
            Some(s) => match self.find_agent(c, user, key, s).await? {
                Some(a) => Some(seal(&sealing_key(key, c.app_id, "agent"), a.0)),
                None => return Ok(ExchangeAnswer::UnknownUser),
            },
        };
        let user = seal(&sealing_key(key, c.app_id, "user"), user.0);
        let Some(issuer) = &self.s.workload_tokens else {
            return Err(Error::Internal("no workload token key".into()));
        };
        let access_token = issuer
            .sign_delegated(&workload_of(c), bindings.clone(), user, agent)?
            .expose()
            .to_string();
        Ok(ExchangeAnswer::Granted {
            bindings,
            user,
            agent,
            access_token,
        })
    }

    /// Claims the one exchange of the user token `jti` for the caller's app, in the shared
    /// idempotency store (Postgres on a server), so a second runtime API process, or one that
    /// restarted and lost its replay set, cannot exchange the same token again (Kimi review of
    /// run 14, R-3 finding 2). The insert is atomic: of two concurrent claims, one wins. The
    /// claim stays for the store's 24 hours, far longer than a user token lives (60 s), and it
    /// stores no response (the delegated token is a secret). A `jti` that is empty, long or not
    /// printable claims nothing (`false`).
    async fn first_exchange(&self, c: &Caller, jti: &str) -> Result<bool> {
        use crate::identity::idempotency::{Begin, KeyRef, request_hash};
        if jti.is_empty() || jti.len() > MAX_JTI_BYTES || !jti.bytes().all(|b| b.is_ascii_graphic())
        {
            return Ok(false);
        }
        let key = KeyRef {
            tenant: c.tenant_id,
            actor: c.app_id.0,
            key: format!("runtime-exchange:{jti}"),
        };
        let path = format!("{}/exchange", wire::PREFIX);
        let hash = request_hash("POST", &path, None, jti.as_bytes());
        match self.s.idempotency.begin(&key, "POST", &path, hash).await {
            Ok(Begin::Fresh) => Ok(true),
            // Exchanged before, or the first exchange still runs.
            Ok(Begin::Replay(_)) | Err(Error::Api(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The context of the user of a delegation (an agent's context with its scope).
    async fn delegated_ctx(&self, c: &Caller, d: &Delegation) -> Result<Option<Context>> {
        let key = self.s.tenant_keys.tenant_key(c.tenant_id).await?;
        let user = UserId(open(&sealing_key(key.as_bytes(), c.app_id, "user"), d.user));
        let Some(agent) = d.agent else {
            return Ok(Some(Context {
                tenant: c.tenant_id,
                actor: Actor::user(user),
                scope: None,
                idempotency_key: None,
            }));
        };
        let agent = AgentId(open(&sealing_key(key.as_bytes(), c.app_id, "agent"), agent));
        let a = match self.s.agents.authenticate(c.tenant_id, agent).await {
            Ok(a) if a.owner_id == user => a,
            Ok(_) | Err(Error::Api(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(Context {
            tenant: c.tenant_id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: PrincipalId(a.id.0),
                },
                on_behalf_of: Some(user),
            },
            scope: Some(a.scope.as_scope()),
            idempotency_key: None,
        }))
    }

    /// The checks of a read (see the module docs, "Bound content").
    pub async fn bound(&self, q: &ReadQuery) -> Result<std::result::Result<BoundNode, Refusal>> {
        let c = &q.caller;
        self.fresh(c.tenant_id).await?;
        if self.serving(c).await?.is_none() {
            return Ok(Err(Refusal::Gone));
        }
        let mut rows: Vec<BindingRow> = self
            .active_rows(c)
            .await?
            .into_iter()
            .filter(|r| r.kind == BindingKind::Node)
            .collect();
        if let Some(d) = &q.delegation {
            rows.retain(|r| r.on_behalf_of_user && d.bindings.contains(&r.name));
        }
        let app_ctx = principal_ctx(c);
        // 1. The node exists in this tenant (another tenant's node: the same answer).
        let above = match self.s.nodes.ancestors(&app_ctx, q.node_id).await {
            Ok(a) => a,
            Err(e) if is_not_found(&e) => return Ok(Err(Refusal::NotFound)),
            Err(Error::Api(_)) => return Ok(Err(Refusal::NotFound)),
            Err(e) => return Err(e),
        };
        // 2. A binding's target is the node or one of its ancestors.
        if !rows
            .iter()
            .any(|r| r.target_node_id == q.node_id || above.contains(&r.target_node_id))
        {
            return Ok(Err(Refusal::Denied));
        }
        // 3. The tuple: the app principal may view the node.
        if !self
            .s
            .authz
            .check(
                &app_subject(c.principal),
                "view",
                &ObjectRef::node(q.node_id),
                None,
            )
            .await?
        {
            return Ok(Err(Refusal::Denied));
        }
        // 4. Not in the trash (the store answers a trashed node only to `manage`).
        let node = match self.s.nodes.get(&app_ctx, q.node_id).await {
            Ok(n) if n.trashed_at.is_none() => n,
            Ok(_) => return Ok(Err(Refusal::NotFound)),
            Err(e) if is_not_found(&e) => return Ok(Err(Refusal::NotFound)),
            Err(Error::Api(_)) => return Ok(Err(Refusal::Denied)),
            Err(e) => return Err(e),
        };
        // 5. The user's right too.
        if let Some(d) = q.delegation.as_ref() {
            let Some(ctx) = self.delegated_ctx(c, d).await? else {
                return Ok(Err(Refusal::NotFound));
            };
            match authz::require(&*self.s.authz, &*self.s.nodes, &ctx, q.node_id, Role::View).await
            {
                Ok(()) => {}
                Err(e) if is_not_found(&e) => return Ok(Err(Refusal::NotFound)),
                Err(Error::Api(_)) => return Ok(Err(Refusal::Denied)),
                Err(e) => return Err(e),
            }
        }
        Ok(Ok(BoundNode { node, app_ctx }))
    }

    /// `readBoundContent` (`POST /content`): the head version of a bound file.
    pub async fn content(&self, q: &ReadQuery) -> Result<std::result::Result<Content, Refusal>> {
        let b = match self.bound(q).await? {
            Ok(b) => b,
            Err(r) => return Ok(Err(r)),
        };
        if b.node.kind != NodeKind::File || b.node.head_version.is_none() {
            return Ok(Err(Refusal::NotFound));
        }
        match self
            .s
            .transfers
            .open_content(&b.app_ctx, q.node_id, None, None)
            .await
        {
            Ok(c) => Ok(Ok(c)),
            Err(e) if is_not_found(&e) => Ok(Err(Refusal::NotFound)),
            Err(Error::Api(_)) => Ok(Err(Refusal::Denied)),
            Err(e) => Err(e),
        }
    }

    /// `listBoundChildren` (`POST /children`): one page of a bound folder.
    pub async fn children(&self, q: &ReadQuery) -> Result<std::result::Result<ChildPage, Refusal>> {
        let b = match self.bound(q).await? {
            Ok(b) => b,
            Err(r) => return Ok(Err(r)),
        };
        if b.node.kind != NodeKind::Folder {
            return Ok(Err(Refusal::NotFound));
        }
        let page = match self
            .s
            .nodes
            .list_children(
                &b.app_ctx,
                q.node_id,
                ChildOrder::Name,
                PageRequest {
                    cursor: q.cursor.clone().map(Cursor),
                    limit: Some(q.limit.unwrap_or(MAX_PAGE).clamp(1, MAX_PAGE)),
                },
            )
            .await
        {
            Ok(p) => p,
            Err(Error::Api(a)) if a.code == ErrorCode::BadRequest => {
                return Ok(Err(Refusal::BadCursor));
            }
            Err(e) if is_not_found(&e) => return Ok(Err(Refusal::NotFound)),
            Err(Error::Api(_)) => return Ok(Err(Refusal::Denied)),
            Err(e) => return Err(e),
        };
        let mut nodes: Vec<Node> = page
            .items
            .into_iter()
            .filter(|n| n.kind != NodeKind::Link && n.trashed_at.is_none())
            .collect();
        if let Some(d) = &q.delegation {
            let Some(ctx) = self.delegated_ctx(&q.caller, d).await? else {
                return Ok(Err(Refusal::NotFound));
            };
            // One bulk check for the page (at most MAX_PAGE children). The folder passed the
            // scope of an agent above, so its children are inside the scope's subtree too.
            let objects: Vec<ObjectRef> = nodes.iter().map(|n| ObjectRef::node(n.id)).collect();
            let seen = self
                .s
                .authz
                .check_bulk(&authz::subject_of(&ctx), "view", &objects)
                .await?;
            let mut keep = seen.into_iter();
            nodes.retain(|_| keep.next().unwrap_or(false));
        }
        let items = nodes
            .into_iter()
            .map(|n| Child {
                node_id: n.id,
                kind: match n.kind {
                    NodeKind::File => "file",
                    NodeKind::Folder => "folder",
                    NodeKind::Typed => "typed",
                    NodeKind::Link => "link",
                }
                .to_string(),
                size: (n.kind == NodeKind::File).then_some(n.size_bytes),
                version_id: n.head_version.map(|v| v.0),
                updated_at: n
                    .updated_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                name: n.name,
            })
            .collect();
        Ok(Ok(ChildPage {
            items,
            next_cursor: page.next_cursor.map(|c| c.0),
        }))
    }

    /// `publishAppEvent` (`POST /topic`): whether the live manifest declares the topic. A
    /// preview publishes nothing in step 3.
    pub async fn topic(&self, q: &TopicQuery) -> Result<TopicAnswer> {
        let c = &q.caller;
        self.fresh(c.tenant_id).await?;
        let Some((_, dep)) = self.serving(c).await? else {
            return Ok(TopicAnswer::Gone);
        };
        if c.environment != AppEnv::Live {
            return Ok(TopicAnswer::Undeclared);
        }
        Ok(
            if dep
                .manifest
                .as_ref()
                .is_some_and(|m| topic_declared(m, &q.topic))
            {
                TopicAnswer::Declared
            } else {
                TopicAnswer::Undeclared
            },
        )
    }
}

/// `500` of an internal route: logged without values.
pub fn internal_error(what: &str, e: &Error) -> ApiError {
    tracing::error!(target: "ruum::runtime_api", error = %e, "{what} failed");
    ApiError::new(ErrorCode::Internal, "the core could not answer")
}
