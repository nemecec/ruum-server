//! In-memory implementations of the identity, tenants and agents area, for unit tests and for
//! running without Docker.
//!
//! The four stores of the area ([`MemoryIdentityStore`], [`crate::tenants::memory::MemoryTenantStore`],
//! [`crate::agents::memory::MemoryAgentStore`], [`crate::agents::oauth::MemoryOAuthServer`]) share one
//! [`MemoryArea`], as the Postgres stores share one database. [`MemoryArea::new`] takes the
//! stores of other areas that this area calls ([`MemoryDeps`]); each one is optional, so a store
//! can run alone in a unit test.

use super::*;
use crate::agents::Agent;
use crate::agents::oauth::OAuthClient;
use crate::audit::AuditLog;
use crate::authz::{Authz, RelationshipUpdate};
use crate::ids::{AgentKeyId, InvitationId, NodeId};
use crate::journal::Journal;
use crate::nodes::NodeStore;
use crate::paging::{Cursor, Page, PageRequest};
use crate::tenants::mail::{Mailer, MemoryMailer};
use crate::tenants::{Invitation, MemberCleanup, MemberRole, RootCreated, Tenant};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// The stores of other areas that the in-memory stores of this area call.
#[derive(Clone)]
pub struct MemoryDeps {
    /// Signs access tokens (key exchange, OAuth) and seals OAuth requests.
    pub tokens: Arc<dyn TokenIssuer>,
    /// The relationship engine: tenant, membership and agent tuples.
    pub authz: Option<Arc<dyn Authz>>,
    /// Checks that a scope subtree is a node of the tenant (`NodeStore::ancestors`).
    pub nodes: Option<Arc<dyn NodeStore>>,
    /// Agent activity reads the journal.
    pub journal: Option<Arc<dyn Journal>>,
    /// Membership, agent and key changes are audited.
    pub audit: Option<Arc<dyn AuditLog>>,
    /// Invitation emails.
    pub mailer: Arc<dyn Mailer>,
    /// Tells the in-memory node store about a new tenant root.
    pub root_created: Option<Arc<dyn RootCreated>>,
    /// The sharing side of `removeMember` and `acceptInvitation`.
    pub member_cleanup: Option<Arc<dyn MemberCleanup>>,
    /// The public URL of the web UI, for links in emails.
    pub public_url: String,
}

impl Default for MemoryDeps {
    fn default() -> Self {
        Self {
            tokens: Arc::new(MemoryTokenIssuer::default()),
            authz: None,
            nodes: None,
            journal: None,
            audit: None,
            mailer: Arc::new(MemoryMailer::default()),
            root_created: None,
            member_cleanup: None,
            public_url: "https://ruum.localhost".into(),
        }
    }
}

pub(crate) struct UserRow {
    pub user: User,
    pub preferences: UserPreferences,
}

pub(crate) struct SessionRow {
    pub session: Session,
    pub secret_hash: [u8; 32],
    pub ended_at: Option<OffsetDateTime>,
}

pub(crate) struct AppPasswordRow {
    pub pw: AppPassword,
    pub user_id: UserId,
    pub secret_hash: [u8; 32],
    /// When `auth.app_password_used` was last written for it.
    pub audited_at: Option<OffsetDateTime>,
}

pub(crate) struct MembershipRow {
    pub role: MemberRole,
    /// The content role of a member (decision 0003-1); `None` unless `role` is `member`.
    pub content_role: Option<crate::authz::Role>,
    pub joined_at: OffsetDateTime,
    /// The join or the last change of the role (`memberships.updated_at` in Postgres).
    pub role_since: OffsetDateTime,
    /// Insertion order, for stable member lists.
    pub order: u64,
}

pub(crate) struct InvitationRow {
    pub inv: Invitation,
    pub token_hash: [u8; 32],
    pub revoked_at: Option<OffsetDateTime>,
    pub accepted_by: Option<UserId>,
}

pub(crate) struct KeyRow {
    pub key: crate::agents::AgentKey,
    pub tenant_id: TenantId,
    pub hash: [u8; 32],
}

pub(crate) struct OAuthGrantRow {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub agent_id: AgentId,
    pub client_id: String,
    pub audience: String,
    pub refresh_hash: Option<[u8; 32]>,
    pub previous_refresh_hash: Option<[u8; 32]>,
    pub refresh_expires_at: Option<OffsetDateTime>,
    pub ended_at: Option<OffsetDateTime>,
}

pub(crate) struct OAuthCodeRow {
    pub grant_id: Uuid,
    pub code_challenge: String,
    pub redirect_uri: String,
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
}

/// A one-time sign-in code of an app origin (`app_sign_in_codes`).
pub(crate) struct AppCodeRow {
    pub binding: super::app_sign_in::AppBinding,
    pub user: UserId,
    pub platform_session: Uuid,
    pub state_hash: [u8; 32],
    pub expires_at: OffsetDateTime,
    pub used_at: Option<OffsetDateTime>,
}

/// A session of an app origin (`app_sessions`).
pub(crate) struct AppSessionRow {
    pub session: super::app_sign_in::AppSession,
    pub ended_at: Option<OffsetDateTime>,
}

/// All rows of the area.
#[derive(Default)]
pub(crate) struct State {
    pub users: HashMap<UserId, UserRow>,
    pub identities: Vec<Identity>,
    pub sessions: HashMap<Uuid, SessionRow>,
    pub app_passwords: HashMap<AppPasswordId, AppPasswordRow>,
    pub tenants: HashMap<TenantId, Tenant>,
    pub memberships: BTreeMap<(TenantId, UserId), MembershipRow>,
    pub invitations: HashMap<InvitationId, InvitationRow>,
    pub agents: HashMap<AgentId, Agent>,
    pub keys: HashMap<AgentKeyId, KeyRow>,
    pub oauth_clients: HashMap<String, OAuthClient>,
    pub oauth_grants: HashMap<Uuid, OAuthGrantRow>,
    pub oauth_codes: HashMap<[u8; 32], OAuthCodeRow>,
    /// The last known names of people who left a tenant (`principal_names`, decision 0003-5).
    pub principal_names: HashMap<(TenantId, Uuid), String>,
    /// Sign-in codes of app origins, by tenant and SHA-256 of the code.
    pub app_codes: HashMap<(TenantId, [u8; 32]), AppCodeRow>,
    /// Sessions of app origins, by tenant and SHA-256 of the secret.
    pub app_sessions: HashMap<(TenantId, [u8; 32]), AppSessionRow>,
    pub order: u64,
}

impl State {
    pub fn next_order(&mut self) -> u64 {
        self.order += 1;
        self.order
    }

    pub fn role_of(&self, t: TenantId, u: UserId) -> Option<MemberRole> {
        self.memberships.get(&(t, u)).map(|m| m.role)
    }

    /// Owners, admins and members of a tenant (not guests).
    pub fn member_count(&self, t: TenantId) -> u32 {
        self.memberships
            .iter()
            .filter(|((tt, _), m)| *tt == t && m.role != MemberRole::Guest)
            .count() as u32
    }

    pub fn owner_count(&self, t: TenantId) -> u32 {
        self.memberships
            .iter()
            .filter(|((tt, _), m)| *tt == t && m.role == MemberRole::Owner)
            .count() as u32
    }
}

/// The shared state and dependencies of the in-memory stores of the area.
pub struct MemoryArea {
    pub(crate) state: Mutex<State>,
    pub(crate) deps: MemoryDeps,
    /// Hooks set after construction, for stores that need the tenant store themselves (the
    /// sharing store reads tenant policies, and cleans up when a member leaves).
    late_root: std::sync::OnceLock<Arc<dyn RootCreated>>,
    late_cleanup: std::sync::OnceLock<Arc<dyn MemberCleanup>>,
}

impl Default for MemoryArea {
    fn default() -> Self {
        Self::new(MemoryDeps::default())
    }
}

impl MemoryArea {
    pub fn new(deps: MemoryDeps) -> Self {
        Self {
            state: Mutex::new(State::default()),
            deps,
            late_root: std::sync::OnceLock::new(),
            late_cleanup: std::sync::OnceLock::new(),
        }
    }

    /// Sets the root hook after construction (when `MemoryDeps::root_created` was `None`).
    /// A second call is ignored.
    pub fn set_root_created(&self, hook: Arc<dyn RootCreated>) {
        let _ = self.late_root.set(hook);
    }

    /// Sets the member cleanup after construction (when `MemoryDeps::member_cleanup` was
    /// `None`), for a sharing store that is built with the tenant store. A second call is
    /// ignored.
    pub fn set_member_cleanup(&self, hook: Arc<dyn MemberCleanup>) {
        let _ = self.late_cleanup.set(hook);
    }

    pub(crate) fn root_hook(&self) -> Option<&Arc<dyn RootCreated>> {
        self.deps.root_created.as_ref().or(self.late_root.get())
    }

    pub(crate) fn cleanup_hook(&self) -> Option<&Arc<dyn MemberCleanup>> {
        self.deps
            .member_cleanup
            .as_ref()
            .or(self.late_cleanup.get())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The mailer, for tests that read the sent mails.
    pub fn mailer(&self) -> &Arc<dyn Mailer> {
        &self.deps.mailer
    }

    /// Writes relationship tuples to the engine, if there is one.
    pub(crate) async fn relate(&self, updates: Vec<RelationshipUpdate>) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        if let Some(a) = &self.deps.authz {
            a.write(updates).await?;
        }
        Ok(())
    }

    /// Writes a whole audit entry (with the client), if there is an audit log.
    pub(crate) async fn record_audit(
        &self,
        ctx: &Context,
        entry: crate::audit::NewAuditEntry,
    ) -> Result<()> {
        if let Some(a) = &self.deps.audit {
            a.record(ctx, entry).await?;
        }
        Ok(())
    }

    /// Writes an audit entry, if there is an audit log.
    pub(crate) async fn audit(
        &self,
        ctx: &Context,
        action: &str,
        target_type: &str,
        target_id: impl ToString,
        data: serde_json::Value,
    ) -> Result<()> {
        if let Some(a) = &self.deps.audit {
            a.record(ctx, audit_entry(action, target_type, target_id, data))
                .await?;
        }
        Ok(())
    }

    /// Checks that `node` is a node of `ctx.tenant` (for scope subtrees).
    ///
    /// A node store that cannot answer (it panics, as the placeholder of an area that is not
    /// built yet does) gives `501 not_implemented`, not a panic of the request.
    pub(crate) async fn check_node(&self, ctx: &Context, node: NodeId) -> Result<()> {
        use futures::FutureExt as _;
        if let Some(n) = &self.deps.nodes {
            let found = std::panic::AssertUnwindSafe(n.ancestors(ctx, node))
                .catch_unwind()
                .await
                .map_err(|_| Error::not_implemented("the scope subtree check (nodes.ancestors)"))?;
            found.map_err(|e| match e {
                Error::Api(_) => Error::not_found("scope subtree"),
                other => other,
            })?;
        }
        Ok(())
    }

    /// Makes a tenant with its root folder and its owner, in the state. Returns the tenant and
    /// the tuples to write.
    pub(crate) fn insert_tenant(
        st: &mut State,
        kind: crate::tenants::TenantKind,
        plan: crate::tenants::Plan,
        name: String,
        owner: UserId,
    ) -> (Tenant, Vec<RelationshipUpdate>) {
        let tenant = Tenant {
            id: TenantId::new(),
            name,
            kind,
            plan,
            region: "hel1".into(),
            root_node_id: NodeId::new(),
            policy: crate::tenants::TenantPolicy::default(),
            created_at: crate::now(),
        };
        st.tenants.insert(tenant.id, tenant.clone());
        let order = st.next_order();
        st.memberships.insert(
            (tenant.id, owner),
            MembershipRow {
                role: MemberRole::Owner,
                content_role: None,
                joined_at: tenant.created_at,
                role_since: tenant.created_at,
                order,
            },
        );
        let tuples = crate::tenants::relations::tenant_created(
            kind,
            tenant.id,
            tenant.root_node_id,
            owner,
            tenant.policy.members_can_share,
        );
        (tenant, tuples)
    }
}

/// One page of `items` (already in order). The cursor is the offset.
pub(crate) fn page_of<T>(items: Vec<T>, page: &PageRequest) -> Result<Page<T>> {
    let offset: usize = match &page.cursor {
        None => 0,
        Some(Cursor(c)) => c.parse().map_err(|_| {
            Error::api(
                crate::error::ErrorCode::BadRequest,
                "the cursor is not valid",
            )
        })?,
    };
    let limit = page.limit() as usize;
    let total = items.len();
    let items: Vec<T> = items.into_iter().skip(offset).take(limit).collect();
    let next = offset + items.len();
    Ok(Page {
        items,
        next_cursor: (next < total).then(|| Cursor(next.to_string())),
    })
}

/// The token issuer of the in-memory services: a [`JwtTokenIssuer`] with a new key for the
/// laptop origin `https://ruum.localhost`.
pub struct MemoryTokenIssuer(pub JwtTokenIssuer);

impl Default for MemoryTokenIssuer {
    fn default() -> Self {
        Self(JwtTokenIssuer::generate(
            "https://ruum.localhost",
            "https://ruum.localhost",
        ))
    }
}

impl std::fmt::Debug for MemoryTokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TokenIssuer for MemoryTokenIssuer {
    fn issuer(&self) -> &str {
        self.0.issuer()
    }
    fn audience(&self) -> &str {
        self.0.audience()
    }
    fn issue(&self, claims: &AccessClaims) -> Result<String> {
        self.0.issue(claims)
    }
    fn verify(&self, token: &str) -> Result<AccessClaims> {
        self.0.verify(token)
    }
    fn verify_for(&self, token: &str, audiences: &[&str]) -> Result<AccessClaims> {
        self.0.verify_for(token, audiences)
    }
    fn signed_by_platform(&self, token: &str) -> bool {
        self.0.signed_by_platform(token)
    }
    fn jwks(&self) -> serde_json::Value {
        self.0.jwks()
    }
    fn seal(&self, purpose: &str, value: &serde_json::Value, seconds: i64) -> Result<String> {
        self.0.seal(purpose, value, seconds)
    }
    fn open(&self, purpose: &str, sealed: &str) -> Result<serde_json::Value> {
        self.0.open(purpose, sealed)
    }
}

/// The in-memory identity store.
#[derive(Clone, Default)]
pub struct MemoryIdentityStore {
    area: Arc<MemoryArea>,
    /// The last bind check of each session ([`IdentityStore::bind_email_grants`]).
    bind_checked: Arc<Mutex<HashMap<Uuid, OffsetDateTime>>>,
}

impl MemoryIdentityStore {
    pub fn new(area: Arc<MemoryArea>) -> Self {
        Self {
            area,
            bind_checked: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn not_found_user() -> Error {
    Error::not_found("user")
}

#[async_trait]
impl IdentityStore for MemoryIdentityStore {
    async fn upsert_from_login(&self, login: &VerifiedLogin) -> Result<User> {
        let now = crate::now();
        let (user, tuples, new_tenant) = {
            let mut st = self.area.lock();
            // 1. A known identity.
            if let Some(id) = st
                .identities
                .iter()
                .find(|i| i.issuer == login.issuer && i.subject == login.subject)
            {
                let uid = id.user_id;
                let row = st.users.get(&uid).ok_or_else(not_found_user)?;
                return Ok(row.user.clone());
            }
            let email = login.email.clone().unwrap_or_default();
            // 2. A verified email of a trusted provider links to the user with that verified
            //    email (design 4.2).
            let linked = if login.email_verified && !email.is_empty() {
                st.users
                    .values()
                    .find(|r| r.user.email_verified && r.user.email.eq_ignore_ascii_case(&email))
                    .map(|r| r.user.clone())
            } else {
                None
            };
            let (user, tuples, new_tenant) = match linked {
                Some(u) => (u, Vec::new(), None),
                None => {
                    let uid = UserId::new();
                    let display = login
                        .name
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| default_display_name(&email));
                    let (tenant, tuples) = MemoryArea::insert_tenant(
                        &mut st,
                        crate::tenants::TenantKind::Personal,
                        crate::tenants::Plan::Free,
                        personal_tenant_name(&display),
                        uid,
                    );
                    // Only a verified email is unique: an unverified sign-in keeps the address
                    // but never as verified while a verified user has it.
                    let verified = login.email_verified
                        && !st.users.values().any(|r| {
                            r.user.email_verified && r.user.email.eq_ignore_ascii_case(&email)
                        });
                    let user = User {
                        id: uid,
                        home_tenant_id: tenant.id,
                        email: email.clone(),
                        email_verified: verified,
                        display_name: display,
                        avatar_url: None,
                        created_at: now,
                    };
                    st.users.insert(
                        uid,
                        UserRow {
                            user: user.clone(),
                            preferences: UserPreferences::default(),
                        },
                    );
                    (user, tuples, Some(tenant))
                }
            };
            st.identities.push(Identity {
                id: Uuid::now_v7(),
                user_id: user.id,
                issuer: login.issuer.clone(),
                subject: login.subject.clone(),
                email: login.email.clone(),
                email_verified: login.email_verified,
                linked_at: now,
            });
            (user, tuples, new_tenant)
        };
        self.area.relate(tuples).await?;
        if let (Some(t), Some(hook)) = (&new_tenant, self.area.root_hook()) {
            hook.root_created(t, user.id);
        }
        // Email grants to a verified email of a trusted provider bind to the user (the sharing
        // side, through the hook). A failure here does not fail the sign-in.
        if user.email_verified
            && login.email_verified
            && login
                .email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&user.email))
            && let Some(c) = self.area.cleanup_hook()
            && let Err(e) = c.verified_email_signed_in(&user.email, user.id).await
        {
            tracing::warn!(error = %e, user = %user.id, "could not bind email grants");
        }
        Ok(user)
    }

    async fn get_user(&self, id: UserId) -> Result<User> {
        self.area
            .lock()
            .users
            .get(&id)
            .map(|r| r.user.clone())
            .ok_or_else(not_found_user)
    }

    async fn preferences_of(&self, user: UserId) -> Result<UserPreferences> {
        self.area
            .lock()
            .users
            .get(&user)
            .map(|r| r.preferences.clone())
            .ok_or_else(not_found_user)
    }

    async fn set_locale(&self, user: UserId, locale: Option<String>) -> Result<UserPreferences> {
        let mut st = self.area.lock();
        let row = st.users.get_mut(&user).ok_or_else(not_found_user)?;
        row.preferences.locale = locale;
        Ok(row.preferences.clone())
    }

    async fn find_verified_email(&self, email: &str) -> Result<Option<User>> {
        Ok(self
            .area
            .lock()
            .users
            .values()
            .find(|r| r.user.email_verified && r.user.email.eq_ignore_ascii_case(email.trim()))
            .map(|r| r.user.clone()))
    }

    async fn identities_of(&self, user: UserId) -> Result<Vec<Identity>> {
        let st = self.area.lock();
        let mut v: Vec<Identity> = st
            .identities
            .iter()
            .filter(|i| i.user_id == user)
            .cloned()
            .collect();
        v.sort_by_key(|i| i.linked_at);
        Ok(v)
    }

    async fn create_session(
        &self,
        user: UserId,
        user_agent: Option<String>,
    ) -> Result<(Session, String)> {
        let mut st = self.area.lock();
        if !st.users.contains_key(&user) {
            return Err(not_found_user());
        }
        let now = crate::now();
        let secret = secret::random_secret("");
        let session = Session {
            id: Uuid::now_v7(),
            user_id: user,
            created_at: now,
            expires_at: now + time::Duration::days(SESSION_DAYS),
            last_seen_at: now,
            user_agent: user_agent.map(|u| u.chars().take(500).collect()),
        };
        st.sessions.insert(
            session.id,
            SessionRow {
                session: session.clone(),
                secret_hash: secret::sha256(&secret),
                ended_at: None,
            },
        );
        Ok((session, secret))
    }

    async fn session_by_secret(&self, secret_value: &str) -> Result<Session> {
        let hash = secret::sha256(secret_value);
        let now = crate::now();
        let mut st = self.area.lock();
        let row = st
            .sessions
            .values_mut()
            .find(|r| r.secret_hash == hash)
            .filter(|r| r.ended_at.is_none() && r.session.expires_at > now)
            .ok_or_else(|| Error::not_found("session"))?;
        // Sliding expiry: a session lives SESSION_DAYS after its last use. The row is touched
        // at most every SESSION_TOUCH_SECONDS, so reads do not write on every request.
        if now - row.session.last_seen_at >= time::Duration::seconds(SESSION_TOUCH_SECONDS) {
            row.session.last_seen_at = now;
            row.session.expires_at = now + time::Duration::days(SESSION_DAYS);
        }
        Ok(row.session.clone())
    }

    async fn sessions_of(&self, user: UserId) -> Result<Vec<Session>> {
        let now = crate::now();
        let st = self.area.lock();
        let mut v: Vec<Session> = st
            .sessions
            .values()
            .filter(|r| {
                r.session.user_id == user && r.ended_at.is_none() && r.session.expires_at > now
            })
            .map(|r| r.session.clone())
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        Ok(v)
    }

    async fn end_session(&self, user: UserId, id: Uuid) -> Result<()> {
        let mut st = self.area.lock();
        match st.sessions.get_mut(&id) {
            Some(r) if r.session.user_id == user && r.ended_at.is_none() => {
                r.ended_at = Some(crate::now());
                Ok(())
            }
            _ => Err(Error::not_found("session")),
        }
    }

    async fn create_app_code(&self, code: super::app_sign_in::NewAppCode) -> Result<String> {
        let now = crate::now();
        let secret = secret::random_secret("");
        let mut st = self.area.lock();
        let live = st.sessions.get(&code.platform_session).is_some_and(|r| {
            r.session.user_id == code.user && r.ended_at.is_none() && r.session.expires_at > now
        });
        if !live {
            return Err(Error::not_found("session"));
        }
        st.app_codes.retain(|_, r| r.expires_at > now);
        st.app_codes.insert(
            (code.binding.tenant_id, secret::sha256(&secret)),
            AppCodeRow {
                binding: code.binding,
                user: code.user,
                platform_session: code.platform_session,
                state_hash: super::app_sign_in::state_hash(&code.state),
                expires_at: now + time::Duration::seconds(super::app_sign_in::APP_CODE_SECONDS),
                used_at: None,
            },
        );
        Ok(secret)
    }

    async fn redeem_app_code(
        &self,
        binding: &super::app_sign_in::AppBinding,
        code: &str,
        state: &str,
    ) -> Result<Option<(super::app_sign_in::AppSession, String)>> {
        let now = crate::now();
        let mut st = self.area.lock();
        let key = (binding.tenant_id, secret::sha256(code));
        let Some(row) = st.app_codes.get_mut(&key) else {
            return Ok(None);
        };
        // A code is spent by the first try, also a refused one.
        let fresh = row.used_at.is_none() && row.expires_at > now;
        row.used_at = Some(now);
        if !fresh
            || row.binding != *binding
            || !secret::constant_time_eq(&row.state_hash, &super::app_sign_in::state_hash(state))
        {
            return Ok(None);
        }
        let (user, platform_session) = (row.user, row.platform_session);
        let live = st.sessions.get(&platform_session).is_some_and(|r| {
            r.session.user_id == user && r.ended_at.is_none() && r.session.expires_at > now
        });
        if !live {
            return Ok(None);
        }
        let secret = secret::random_secret("");
        let session = super::app_sign_in::AppSession {
            id: Uuid::now_v7(),
            binding: *binding,
            user_id: user,
            platform_session_id: platform_session,
            created_at: now,
            expires_at: now + time::Duration::hours(super::app_sign_in::APP_SESSION_HOURS),
        };
        st.app_sessions
            .retain(|_, r| r.ended_at.is_none() && r.session.expires_at > now);
        st.app_sessions.insert(
            (binding.tenant_id, secret::sha256(&secret)),
            AppSessionRow {
                session: session.clone(),
                ended_at: None,
            },
        );
        Ok(Some((session, secret)))
    }

    async fn app_session(
        &self,
        tenant: TenantId,
        secret_value: &str,
    ) -> Result<Option<super::app_sign_in::AppSession>> {
        let now = crate::now();
        let st = self.area.lock();
        let Some(row) = st.app_sessions.get(&(tenant, secret::sha256(secret_value))) else {
            return Ok(None);
        };
        if row.ended_at.is_some()
            || row.session.expires_at <= now
            || row.session.binding.tenant_id != tenant
        {
            return Ok(None);
        }
        let live = st
            .sessions
            .get(&row.session.platform_session_id)
            .is_some_and(|r| {
                r.session.user_id == row.session.user_id
                    && r.ended_at.is_none()
                    && r.session.expires_at > now
            });
        Ok(live.then(|| row.session.clone()))
    }

    async fn end_app_session(&self, tenant: TenantId, secret_value: &str) -> Result<()> {
        let mut st = self.area.lock();
        if let Some(row) = st
            .app_sessions
            .get_mut(&(tenant, secret::sha256(secret_value)))
            && row.ended_at.is_none()
        {
            row.ended_at = Some(crate::now());
        }
        Ok(())
    }

    async fn bind_email_grants(&self, user: UserId, session: Uuid, force: bool) -> Result<usize> {
        let now = crate::now();
        let email = {
            let st = self.area.lock();
            let live = st.sessions.values().any(|r| {
                r.session.id == session
                    && r.session.user_id == user
                    && r.ended_at.is_none()
                    && r.session.expires_at > now
            });
            if !live {
                return Ok(0);
            }
            match st.users.get(&user) {
                Some(r) if r.user.email_verified => r.user.email.clone(),
                _ => return Ok(0),
            }
        };
        {
            let mut checked = self.bind_checked.lock().unwrap_or_else(|e| e.into_inner());
            let due = force
                || checked
                    .get(&session)
                    .is_none_or(|t| now - *t >= time::Duration::seconds(super::BIND_CHECK_SECONDS));
            if !due {
                return Ok(0);
            }
            checked.insert(session, now);
        }
        if let Some(c) = self.area.cleanup_hook() {
            c.verified_email_signed_in(&email, user).await?;
        }
        Ok(0)
    }

    async fn list_app_passwords(&self, ctx: &Context) -> Result<Vec<AppPassword>> {
        let user = crate::actor::require_user_actor(ctx)?;
        let st = self.area.lock();
        let mut v: Vec<AppPassword> = st
            .app_passwords
            .values()
            .filter(|r| r.user_id == user && r.pw.tenant_id == ctx.tenant)
            .map(|r| r.pw.clone())
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        Ok(v)
    }

    async fn create_app_password(
        &self,
        ctx: &Context,
        name: String,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AppPassword, String)> {
        let user = crate::actor::require_user_actor(ctx)?;
        let name = check_name("the name", &name, 100)?;
        let now = crate::now();
        if expires_at.is_some_and(|e| e <= now) {
            return Err(invalid("expires_at must be in the future"));
        }
        let (pw, secret_value) = {
            let mut st = self.area.lock();
            if st.role_of(ctx.tenant, user).is_none() {
                return Err(Error::not_found("tenant"));
            }
            let secret_value = secret::random_secret(APP_PASSWORD_PREFIX);
            let pw = AppPassword {
                id: AppPasswordId::new(),
                tenant_id: ctx.tenant,
                name,
                prefix: secret::display_prefix(&secret_value, APP_PASSWORD_PREFIX),
                created_at: now,
                expires_at,
                last_used_at: None,
                revoked_at: None,
            };
            st.app_passwords.insert(
                pw.id,
                AppPasswordRow {
                    pw: pw.clone(),
                    user_id: user,
                    secret_hash: secret::sha256(&secret_value),
                    audited_at: None,
                },
            );
            (pw, secret_value)
        };
        self.area
            .audit(
                ctx,
                "auth.app_password_created",
                "app_password",
                pw.id,
                serde_json::json!({ "name": pw.name }),
            )
            .await?;
        Ok((pw, secret_value))
    }

    async fn revoke_app_password(&self, ctx: &Context, id: AppPasswordId) -> Result<()> {
        let user = crate::actor::require_user_actor(ctx)?;
        {
            let mut st = self.area.lock();
            match st.app_passwords.get_mut(&id) {
                Some(r) if r.user_id == user && r.pw.tenant_id == ctx.tenant => {
                    if r.pw.revoked_at.is_none() {
                        r.pw.revoked_at = Some(crate::now());
                    }
                }
                _ => return Err(Error::not_found("app password")),
            }
        }
        self.area
            .audit(
                ctx,
                "auth.app_password_revoked",
                "app_password",
                id,
                serde_json::json!({}),
            )
            .await
    }

    async fn authenticate_app_password_from(
        &self,
        email: &str,
        secret_value: &str,
        client: &crate::audit::events::ClientInfo,
    ) -> Result<Authenticated> {
        let refuse = || {
            unauthenticated(
                "the email or app password is wrong",
                "make a new app password in the web UI",
            )
        };
        if !secret_value.starts_with(APP_PASSWORD_PREFIX) {
            return Err(refuse());
        }
        let hash = secret::sha256(secret_value);
        let now = crate::now();
        let (user_id, tenant_id, id, audit) = {
            let mut st = self.area.lock();
            let (user_id, tenant_id, id) = {
                let r = st
                    .app_passwords
                    .values()
                    .find(|r| secret::constant_time_eq(&r.secret_hash, &hash))
                    .ok_or_else(refuse)?;
                if r.pw.revoked_at.is_some() || r.pw.expires_at.is_some_and(|e| e <= now) {
                    return Err(refuse());
                }
                (r.user_id, r.pw.tenant_id, r.pw.id)
            };
            let user = st.users.get(&user_id).ok_or_else(refuse)?;
            if !user.user.email.eq_ignore_ascii_case(email.trim()) {
                return Err(refuse());
            }
            if st.role_of(tenant_id, user_id).is_none() {
                return Err(refuse());
            }
            let mut audit = false;
            if let Some(r) = st.app_passwords.get_mut(&id) {
                r.pw.last_used_at = Some(now);
                let every =
                    time::Duration::seconds(crate::audit::events::APP_PASSWORD_AUDIT_SECONDS);
                if r.audited_at.is_none_or(|t| now - t >= every) {
                    r.audited_at = Some(now);
                    audit = true;
                }
            }
            (user_id, tenant_id, id, audit)
        };
        if audit {
            self.area
                .record_audit(
                    &ctx_of(tenant_id, user_id),
                    crate::audit::events::app_password_used_entry(id, client),
                )
                .await?;
        }
        Ok(Authenticated {
            actor: Actor::user(user_id),
            bound_tenant: Some(tenant_id),
            scope: None,
            session_id: None,
        })
    }
}

fn ctx_of(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

/// The display name when the identity provider sends none: the part of the email before `@`.
pub fn default_display_name(email: &str) -> String {
    let local = email.split('@').next().unwrap_or_default().trim();
    if local.is_empty() {
        "New user".to_string()
    } else {
        local.chars().take(100).collect()
    }
}

/// The name of a personal tenant.
pub fn personal_tenant_name(display_name: &str) -> String {
    let base: String = display_name.chars().take(80).collect();
    format!("{base}'s space")
}

/// A fake identity provider for tests: [`MemoryOidcClient::add_login`] registers the login that
/// a code stands for.
#[derive(Debug, Default)]
pub struct MemoryOidcClient {
    logins: Mutex<HashMap<String, VerifiedLogin>>,
}

impl MemoryOidcClient {
    /// The issuer of the fake provider.
    pub const ISSUER: &'static str = "https://idp.ruum.test";

    /// Registers the login that `code` gives at the callback.
    pub fn add_login(&self, code: &str, login: VerifiedLogin) {
        self.logins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(code.to_string(), login);
    }
}

#[async_trait]
impl OidcClient for MemoryOidcClient {
    async fn start_login(
        &self,
        return_to: Option<String>,
        idp_hint: Option<String>,
    ) -> Result<LoginStart> {
        let state = secret::random_secret("");
        let nonce = secret::random_secret("");
        let pkce_verifier = secret::random_secret("");
        let mut url = url::Url::parse(&format!("{}/oauth/v2/authorize", Self::ISSUER))
            .map_err(|e| Error::Internal(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", "ruum-test")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &secret::pkce_challenge(&pkce_verifier))
            .append_pair("code_challenge_method", "S256");
        if let Some(h) = idp_hint {
            url.query_pairs_mut().append_pair("idp_hint", &h);
        }
        Ok(LoginStart {
            authorize_url: url.to_string(),
            state,
            nonce,
            pkce_verifier,
            return_to: return_to.unwrap_or_else(|| "/".into()),
        })
    }

    async fn finish_login(&self, code: &str, _start: &LoginStart) -> Result<VerifiedLogin> {
        self.logins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(code)
            .ok_or_else(|| {
                unauthenticated("the sign-in code is not valid", "start the sign-in again")
            })
    }

    async fn logout_url(&self, _id_token_hint: Option<String>) -> Result<String> {
        Ok(format!("{}/oidc/v1/end_session", Self::ISSUER))
    }
}
