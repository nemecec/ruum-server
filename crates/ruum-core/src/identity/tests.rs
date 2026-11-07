//! The conformance suite of the identity area: every test runs against the in-memory and the
//! Postgres implementation (`#[sqlx::test]`, one database per test).
//!
//! [`Area`] holds the four stores of the area. [`conformance!`] makes one test per
//! implementation from an `async fn(Area)`.

use super::*;
use crate::agents::AgentStore;
use crate::agents::oauth::OAuthServer;
use crate::error::ErrorCode;
use crate::nodes::NodeStore;
use crate::tenants::mail::MemoryMailer;
use crate::tenants::{RootCreated, Tenant, TenantStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The stores of the area under test.
pub(crate) struct Area {
    pub tenants: Arc<dyn TenantStore>,
    pub identity: Arc<dyn IdentityStore>,
    pub agents: Arc<dyn AgentStore>,
    pub oauth: Arc<dyn OAuthServer>,
    pub tokens: Arc<dyn TokenIssuer>,
    pub mailer: Arc<MemoryMailer>,
    /// The audit log the stores write to (decision 0003-4 checks read it).
    pub audit: Arc<dyn crate::audit::AuditLog>,
    pub pool: Option<sqlx::PgPool>,
    /// The in-memory engine that the in-memory stores write their tuples to (`None` for
    /// Postgres, whose tuples go to `authz_outbox`).
    pub authz: Option<Arc<crate::authz::memory::MemoryAuthz>>,
}

impl Area {
    /// The relationships of the tenant now, as `relation@type:id` strings of `tenant:<t>`. For
    /// the in-memory stores from the engine; for Postgres folded from the tenant's outbox (each
    /// batch in order: a touch adds, a delete removes), the state SpiceDB gets.
    pub async fn tenant_tuples(&self, tenant: TenantId) -> Vec<String> {
        use crate::authz::{Relationship, RelationshipUpdate};
        let object = format!("tenant:{tenant}");
        let fmt = |r: &Relationship| {
            format!(
                "{}@{}:{}",
                r.relation, r.subject.object.object_type, r.subject.object.object_id
            )
        };
        let mut out: Vec<String> = Vec::new();
        if let Some(authz) = &self.authz {
            for r in authz.all() {
                if r.object.to_string() == object {
                    out.push(fmt(&r));
                }
            }
        } else if let Some(pool) = &self.pool {
            let mut c = system(pool).await;
            let batches: Vec<serde_json::Value> = sqlx::query_scalar(
                "select updates from authz_outbox where tenant_id = $1 order by id",
            )
            .bind(tenant)
            .fetch_all(&mut *c)
            .await
            .unwrap();
            for b in batches {
                let ups: Vec<RelationshipUpdate> = serde_json::from_value(b).unwrap();
                for u in ups {
                    match u {
                        RelationshipUpdate::Touch(r) if r.object.to_string() == object => {
                            let k = fmt(&r);
                            if !out.contains(&k) {
                                out.push(k);
                            }
                        }
                        RelationshipUpdate::Delete(r) if r.object.to_string() == object => {
                            let k = fmt(&r);
                            out.retain(|x| *x != k);
                        }
                        _ => {}
                    }
                }
            }
        }
        out.sort();
        out
    }
}

impl Area {
    /// Sends the queued mails: the Postgres stores queue them in `mail_outbox` and the worker
    /// sends them (one round here); the in-memory stores sent them already.
    pub async fn deliver(&self) -> crate::mailing::outbox::Round {
        match &self.pool {
            Some(pool) => crate::mailing::outbox::run_once(
                pool,
                self.mailer.as_ref(),
                "https://ruum.localhost",
            )
            .await
            .unwrap(),
            None => crate::mailing::outbox::Round::default(),
        }
    }
}

/// A node store that knows only tenant roots, for the scope subtree checks of the in-memory
/// stores. Every other method is not used by the area.
#[derive(Default)]
pub(crate) struct Roots {
    roots: Mutex<HashMap<crate::ids::NodeId, TenantId>>,
}

impl RootCreated for Roots {
    fn root_created(&self, tenant: &Tenant, _owner: UserId) {
        self.roots
            .lock()
            .unwrap()
            .insert(tenant.root_node_id, tenant.id);
    }
}

#[async_trait]
impl NodeStore for Roots {
    async fn root(&self, _: &Context) -> Result<crate::nodes::Node> {
        unimplemented!()
    }
    async fn create(
        &self,
        _: &Context,
        _: crate::nodes::NewNode,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn get(&self, _: &Context, _: crate::ids::NodeId) -> Result<crate::nodes::Node> {
        unimplemented!()
    }
    async fn get_by_path(&self, _: &Context, _: &str) -> Result<crate::nodes::Node> {
        unimplemented!()
    }
    async fn list_children(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: crate::nodes::ChildOrder,
        _: crate::paging::PageRequest,
    ) -> Result<crate::paging::Page<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn rename(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: String,
        _: crate::nodes::OnConflict,
        _: Option<i64>,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn move_node(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: crate::nodes::MoveRequest,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn copy(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: crate::nodes::CopyRequest,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn trash(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: Option<i64>,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn restore(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
        _: crate::nodes::RestoreRequest,
    ) -> Result<crate::journal::Written<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn list_trash(
        &self,
        _: &Context,
        _: bool,
        _: crate::paging::PageRequest,
    ) -> Result<crate::paging::Page<crate::nodes::Node>> {
        unimplemented!()
    }
    async fn purge(
        &self,
        _: &Context,
        _: crate::ids::NodeId,
    ) -> Result<crate::journal::Written<()>> {
        unimplemented!()
    }
    async fn empty_trash(&self, _: &Context) -> Result<(u32, Option<crate::journal::ChangeInfo>)> {
        unimplemented!()
    }
    async fn path_of(&self, _: &Context, _: crate::ids::NodeId) -> Result<String> {
        unimplemented!()
    }
    async fn ancestors(
        &self,
        ctx: &Context,
        id: crate::ids::NodeId,
    ) -> Result<Vec<crate::ids::NodeId>> {
        match self.roots.lock().unwrap().get(&id) {
            Some(t) if *t == ctx.tenant => Ok(vec![]),
            _ => Err(Error::not_found("node")),
        }
    }
}

/// The in-memory area, with a node store that knows the tenant roots.
pub(crate) fn memory() -> Area {
    let roots = Arc::new(Roots::default());
    let mailer = Arc::new(MemoryMailer::default());
    let tokens: Arc<dyn TokenIssuer> = Arc::new(memory::MemoryTokenIssuer::default());
    let authz = Arc::new(crate::authz::memory::MemoryAuthz::new(
        crate::authz::schema::Schema::contract(),
    ));
    let audit: Arc<dyn crate::audit::AuditLog> =
        Arc::new(crate::audit::memory::MemoryAuditLog::default());
    let area = Arc::new(memory::MemoryArea::new(memory::MemoryDeps {
        tokens: tokens.clone(),
        authz: Some(authz.clone()),
        nodes: Some(roots.clone()),
        root_created: Some(roots),
        mailer: mailer.clone(),
        audit: Some(audit.clone()),
        ..memory::MemoryDeps::default()
    }));
    Area {
        tenants: Arc::new(crate::tenants::memory::MemoryTenantStore::new(area.clone())),
        identity: Arc::new(memory::MemoryIdentityStore::new(area.clone())),
        agents: Arc::new(crate::agents::memory::MemoryAgentStore::new(area.clone())),
        oauth: Arc::new(crate::agents::oauth::MemoryOAuthServer::new(area)),
        tokens,
        mailer,
        audit,
        pool: None,
        authz: Some(authz),
    }
}

/// The Postgres area on a test database.
pub(crate) fn postgres(pool: sqlx::PgPool) -> Area {
    let mailer = Arc::new(MemoryMailer::default());
    let tokens: Arc<dyn TokenIssuer> = Arc::new(memory::MemoryTokenIssuer::default());
    let mut area = postgres::PgArea::new(pool.clone(), tokens.clone());
    area.mailer = mailer.clone();
    Area {
        tenants: Arc::new(crate::tenants::postgres::PgTenantStore::new(area.clone())),
        identity: Arc::new(postgres::PgIdentityStore::new(area.clone())),
        agents: Arc::new(crate::agents::postgres::PgAgentStore::new(area.clone())),
        oauth: Arc::new(crate::agents::postgres::PgOAuthServer::new(area)),
        tokens,
        mailer,
        audit: Arc::new(crate::audit::postgres::PgAuditLog::new(pool.clone())),
        pool: Some(pool),
        authz: None,
    }
}

/// One test per implementation for each `async fn(Area)` named.
#[macro_export]
#[doc(hidden)]
macro_rules! conformance {
    ($($name:ident),* $(,)?) => {
        mod memory_impl {
            $(
                #[tokio::test]
                async fn $name() {
                    super::$name($crate::identity::tests::memory()).await
                }
            )*
        }
        mod postgres_impl {
            $(
                #[sqlx::test(migrations = "../../migrations")]
                async fn $name(pool: sqlx::PgPool) {
                    super::$name($crate::identity::tests::postgres(pool)).await
                }
            )*
        }
    };
}

/// The error code of a result.
pub(crate) fn code<T: std::fmt::Debug>(r: Result<T>) -> ErrorCode {
    match r {
        Err(e) => e.to_api().code,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

/// A verified login for `email`.
pub(crate) fn login(email: &str) -> VerifiedLogin {
    VerifiedLogin {
        issuer: "https://idp.ruum.test".into(),
        subject: Uuid::now_v7().to_string(),
        email: Some(email.into()),
        email_verified: true,
        name: Some(format!("User {email}")),
        id_token: None,
    }
}

/// A new user with a unique email.
pub(crate) async fn user(a: &Area, name: &str) -> User {
    let email = format!("{name}-{}@example.test", Uuid::now_v7().simple());
    a.identity.upsert_from_login(&login(&email)).await.unwrap()
}

/// The context of `user` in `tenant`.
pub(crate) fn ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

async fn login_makes_a_user_with_a_personal_tenant(a: Area) {
    let l = login("mari@example.test");
    let u = a.identity.upsert_from_login(&l).await.unwrap();
    assert!(u.email_verified);
    assert_eq!(u.email, "mari@example.test");
    let again = a.identity.upsert_from_login(&l).await.unwrap();
    assert_eq!(again.id, u.id);
    let tenants = a.tenants.list_for_user(u.id).await.unwrap();
    assert_eq!(tenants.len(), 1);
    assert_eq!(tenants[0].0.id, u.home_tenant_id);
    assert_eq!(tenants[0].0.kind, crate::tenants::TenantKind::Personal);
    assert_eq!(tenants[0].1, crate::tenants::MemberRole::Owner);
    assert_eq!(a.identity.identities_of(u.id).await.unwrap().len(), 1);
    assert_eq!(a.identity.get_user(u.id).await.unwrap().id, u.id);
    assert_eq!(
        code(a.identity.get_user(UserId::new()).await),
        ErrorCode::NotFound
    );
    if let Some(pool) = &a.pool {
        // The personal root and its tuples are in the database and in the outbox.
        let n: i64 = sqlx::query_scalar("select count(*) from authz_outbox where tenant_id = $1")
            .bind(u.home_tenant_id)
            .fetch_one(&mut *system(pool).await)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
}

/// A connection with `ruum.system = on`, for checks of the tests.
pub(crate) async fn system(pool: &sqlx::PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut c = pool.acquire().await.unwrap();
    sqlx::query("select set_config('ruum.system', 'on', false)")
        .execute(&mut *c)
        .await
        .unwrap();
    c
}

async fn only_a_verified_email_links_identities(a: Area) {
    let first = a
        .identity
        .upsert_from_login(&login("kati@example.test"))
        .await
        .unwrap();
    // Another provider with the same verified email: the same user.
    let linked = a
        .identity
        .upsert_from_login(&login("KATI@example.test"))
        .await
        .unwrap();
    assert_eq!(linked.id, first.id);
    assert_eq!(a.identity.identities_of(first.id).await.unwrap().len(), 2);
    // An unverified email never takes the address.
    let mut l = login("kati@example.test");
    l.email_verified = false;
    let other = a.identity.upsert_from_login(&l).await.unwrap();
    assert_ne!(other.id, first.id);
    assert!(!other.email_verified);
    assert_eq!(
        a.identity
            .find_verified_email("kati@example.test")
            .await
            .unwrap()
            .map(|u| u.id),
        Some(first.id)
    );
}

async fn sessions_start_find_and_end(a: Area) {
    let u = user(&a, "s").await;
    let other = user(&a, "o").await;
    let (s, secret) = a
        .identity
        .create_session(u.id, Some("test".into()))
        .await
        .unwrap();
    assert_eq!(
        a.identity.session_by_secret(&secret).await.unwrap().id,
        s.id
    );
    assert_eq!(
        code(a.identity.session_by_secret("nope").await),
        ErrorCode::NotFound
    );
    let list = a.identity.sessions_of(u.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        code(a.identity.end_session(other.id, s.id).await),
        ErrorCode::NotFound
    );
    a.identity.end_session(u.id, s.id).await.unwrap();
    assert_eq!(
        code(a.identity.session_by_secret(&secret).await),
        ErrorCode::NotFound
    );
    assert!(a.identity.sessions_of(u.id).await.unwrap().is_empty());
    assert_eq!(
        code(a.identity.end_session(u.id, s.id).await),
        ErrorCode::NotFound
    );
}

async fn app_passwords_are_the_user_in_one_tenant(a: Area) {
    let u = user(&a, "webdav").await;
    let other = user(&a, "other").await;
    let c = ctx(u.home_tenant_id, u.id);
    let (pw, secret) = a
        .identity
        .create_app_password(&c, "Finder".into(), None)
        .await
        .unwrap();
    assert!(secret.starts_with(APP_PASSWORD_PREFIX));
    assert_eq!(secret.len(), APP_PASSWORD_PREFIX.len() + 43);
    assert!(secret.starts_with(&pw.prefix));
    assert_eq!(a.identity.list_app_passwords(&c).await.unwrap().len(), 1);
    let who = a
        .identity
        .authenticate_app_password(&u.email, &secret)
        .await
        .unwrap();
    assert_eq!(who.actor, Actor::user(u.id));
    assert_eq!(who.bound_tenant, Some(u.home_tenant_id));
    // Decision 0003-4: a second sign-in within the hour writes no second audit row.
    let client = crate::audit::events::ClientInfo {
        ip: Some("198.51.100.4".into()),
        user_agent: Some("WebDAVFS/3.0".into()),
    };
    a.identity
        .authenticate_app_password_from(&u.email, &secret, &client)
        .await
        .unwrap();
    let used: Vec<_> = audit_actions(&a, u.home_tenant_id)
        .await
        .into_iter()
        .filter(|e| e.action == "auth.app_password_used")
        .collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].actor, Actor::user(u.id));
    assert_eq!(
        used[0].target_id.as_deref(),
        Some(pw.id.to_string().as_str())
    );
    assert_eq!(
        code(
            a.identity
                .authenticate_app_password(&other.email, &secret)
                .await
        ),
        ErrorCode::Unauthenticated
    );
    // Not a member of the other user's tenant.
    assert_eq!(
        code(
            a.identity
                .create_app_password(&ctx(other.home_tenant_id, u.id), "x".into(), None)
                .await
        ),
        ErrorCode::NotFound
    );
    // Another user's password is not found.
    assert_eq!(
        code(
            a.identity
                .revoke_app_password(&ctx(u.home_tenant_id, other.id), pw.id)
                .await
        ),
        ErrorCode::NotFound
    );
    // Agents cannot make app passwords.
    let mut agent_ctx = c.clone();
    agent_ctx.actor = Actor {
        actor: crate::actor::PrincipalRef {
            kind: crate::actor::PrincipalKind::Agent,
            id: crate::ids::PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(u.id),
    };
    assert_eq!(
        code(
            a.identity
                .create_app_password(&agent_ctx, "x".into(), None)
                .await
        ),
        ErrorCode::UserRequired
    );
    a.identity.revoke_app_password(&c, pw.id).await.unwrap();
    assert_eq!(
        code(
            a.identity
                .authenticate_app_password(&u.email, &secret)
                .await
        ),
        ErrorCode::Unauthenticated
    );
    let listed = a.identity.list_app_passwords(&c).await.unwrap();
    assert!(listed[0].revoked_at.is_some());
    assert_eq!(
        code(
            a.identity
                .create_app_password(
                    &c,
                    "past".into(),
                    Some(crate::now() - time::Duration::hours(1))
                )
                .await
        ),
        ErrorCode::ValidationFailed
    );
}

async fn the_token_issuer_signs_what_it_verifies(a: Area) {
    let u = user(&a, "t").await;
    let t = issue_user_token(a.tokens.as_ref(), u.id, 60).unwrap();
    let c = a.tokens.verify(&t).unwrap();
    assert_eq!(c.sub, u.id.0);
    assert_eq!(c.typ, TokenType::User);
}

async fn preferences_belong_to_one_user(a: Area) {
    let mari = user(&a, "mari").await;
    let jaan = user(&a, "jaan").await;
    assert_eq!(
        a.identity.preferences_of(mari.id).await.unwrap(),
        UserPreferences::default()
    );
    let p = a
        .identity
        .set_locale(mari.id, Some("et".into()))
        .await
        .unwrap();
    assert_eq!(p.locale.as_deref(), Some("et"));
    assert_eq!(
        a.identity
            .preferences_of(mari.id)
            .await
            .unwrap()
            .locale
            .as_deref(),
        Some("et")
    );
    // Another user keeps their own preferences.
    assert_eq!(
        a.identity.preferences_of(jaan.id).await.unwrap().locale,
        None
    );
    // A new sign-in keeps the preference.
    let again = a
        .identity
        .upsert_from_login(&login(&mari.email))
        .await
        .unwrap();
    assert_eq!(again.id, mari.id);
    assert_eq!(
        a.identity
            .preferences_of(mari.id)
            .await
            .unwrap()
            .locale
            .as_deref(),
        Some("et")
    );
    // `None` clears it.
    let cleared = a.identity.set_locale(mari.id, None).await.unwrap();
    assert_eq!(cleared.locale, None);
    // An unknown user is not found.
    assert_eq!(
        code(a.identity.preferences_of(UserId::new()).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(
            a.identity
                .set_locale(UserId::new(), Some("et".into()))
                .await
        ),
        ErrorCode::NotFound
    );
}

#[test]
fn locale_tags() {
    for ok in [
        "en",
        "et",
        "fil",
        "en-GB",
        "es-419",
        "sr-Latn",
        "sr-Latn-RS",
        "en-XA",
    ] {
        assert!(valid_locale(ok), "{ok}");
        assert!(check_locale(Some(ok)).is_ok());
    }
    for bad in [
        "",
        "e",
        "EN",
        "en_GB",
        "en-gb",
        "english",
        "en-GB-x",
        "et-",
        "-et",
        "en-Latn-gb",
    ] {
        assert!(!valid_locale(bad), "{bad}");
        assert_eq!(code(check_locale(Some(bad))), ErrorCode::ValidationFailed);
    }
    assert!(check_locale(None).is_ok());
}

crate::conformance!(
    login_makes_a_user_with_a_personal_tenant,
    only_a_verified_email_links_identities,
    sessions_start_find_and_end,
    app_passwords_are_the_user_in_one_tenant,
    the_token_issuer_signs_what_it_verifies,
    preferences_belong_to_one_user,
);

mod idempotency_tests {
    use super::super::idempotency::*;
    use crate::ids::TenantId;

    fn key(t: TenantId) -> KeyRef {
        KeyRef {
            tenant: t,
            actor: uuid::Uuid::now_v7(),
            key: "key-12345678".into(),
        }
    }

    async fn replay_and_reuse(store: &dyn IdempotencyStore, tenant: TenantId) {
        let k = key(tenant);
        let h1 = request_hash("POST", "/v1/x", None, b"{}");
        let h2 = request_hash("POST", "/v1/x", None, b"{\"a\":1}");
        assert_eq!(
            store.begin(&k, "POST", "/v1/x", h1).await.unwrap(),
            Begin::Fresh
        );
        // While the first request runs.
        assert!(store.begin(&k, "POST", "/v1/x", h1).await.is_err());
        let resp = StoredResponse {
            status: 201,
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{\"id\":1}".to_vec(),
        };
        store.finish(&k, &resp).await.unwrap();
        assert_eq!(
            store.begin(&k, "POST", "/v1/x", h1).await.unwrap(),
            Begin::Replay(resp)
        );
        let e = store.begin(&k, "POST", "/v1/x", h2).await.unwrap_err();
        assert_eq!(
            e.to_api().code,
            crate::error::ErrorCode::IdempotencyKeyReused
        );
        // A server error gives the key up.
        let k2 = key(tenant);
        store.begin(&k2, "POST", "/v1/x", h1).await.unwrap();
        store.abandon(&k2).await.unwrap();
        assert_eq!(
            store.begin(&k2, "POST", "/v1/x", h1).await.unwrap(),
            Begin::Fresh
        );
        assert!(check_key("short").is_err());
        assert!(check_key("a-good-key-1234").is_ok());
    }

    #[tokio::test]
    async fn memory_store() {
        replay_and_reuse(&MemoryIdempotencyStore::default(), TenantId::new()).await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store(pool: sqlx::PgPool) {
        let a = super::postgres(pool.clone());
        let u = super::user(&a, "idem").await;
        let store = super::super::postgres::PgIdempotencyStore::new(pool);
        replay_and_reuse(&store, u.home_tenant_id).await;
    }

    /// The row of a `createAgentKey` answer holds no `ruum_agent_` secret (decision 0001-7).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_keeps_no_agent_key(pool: sqlx::PgPool) {
        let a = super::postgres(pool.clone());
        let u = super::user(&a, "idem-secret").await;
        let store = super::super::postgres::PgIdempotencyStore::new(pool.clone());
        let k = KeyRef {
            tenant: u.home_tenant_id,
            actor: u.id.0,
            key: "create-key-0001".into(),
        };
        let h = request_hash("POST", "/v1/agents/x/keys", None, b"{}");
        assert_eq!(
            store
                .begin(&k, "POST", "/v1/agents/x/keys", h)
                .await
                .unwrap(),
            Begin::Fresh
        );
        let secret = crate::identity::secret::random_secret(crate::agents::API_KEY_PREFIX);
        let body = format!(r#"{{"id":"k","prefix":"ruum_agent_abcd","secret":"{secret}"}}"#);
        store
            .finish(
                &k,
                &StoredResponse {
                    status: 201,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: body.into_bytes(),
                },
            )
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("select set_config('ruum.system', 'on', false)")
            .execute(&mut *conn)
            .await
            .unwrap();
        let (stored, status): (Vec<u8>, i16) = sqlx::query_as(
            "select response_body, status_code from idempotency_keys where key = $1",
        )
        .bind("create-key-0001")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(status, 409);
        assert!(!String::from_utf8_lossy(&stored).contains("ruum_agent_"));
        let Begin::Replay(r) = store
            .begin(&k, "POST", "/v1/agents/x/keys", h)
            .await
            .unwrap()
        else {
            panic!("expected a replay");
        };
        assert_eq!(r.status, 409);
        assert!(!String::from_utf8_lossy(&r.body).contains(&secret));
    }

    /// The row of a `createShareLink` answer holds no `ruum_link_` token, and neither does the
    /// row of a `no-store` answer (`resolveShareLink`, whose access token has no prefix).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_keeps_no_share_link_token(pool: sqlx::PgPool) {
        let a = super::postgres(pool.clone());
        let u = super::user(&a, "idem-link").await;
        let store = super::super::postgres::PgIdempotencyStore::new(pool.clone());
        let token = crate::sharing::rules::new_link_token();
        let answers = [
            (
                "create-link-0001",
                format!(r#"{{"share_link":{{"token":"{token}","url":"https://x/s#{token}"}}}}"#),
                vec![("content-type".to_string(), "application/json".to_string())],
            ),
            (
                "resolve-link-001",
                r#"{"access_token":"eyJhbGciOiJFZERTQSJ9.e30.c2ln"}"#.to_string(),
                vec![("cache-control".to_string(), "no-store".to_string())],
            ),
        ];
        for (key, body, headers) in answers {
            let k = KeyRef {
                tenant: u.home_tenant_id,
                actor: u.id.0,
                key: key.into(),
            };
            let h = request_hash("POST", "/v1/x", None, key.as_bytes());
            assert_eq!(
                store.begin(&k, "POST", "/v1/x", h).await.unwrap(),
                Begin::Fresh
            );
            store
                .finish(
                    &k,
                    &StoredResponse {
                        status: 201,
                        headers,
                        body: body.into_bytes(),
                    },
                )
                .await
                .unwrap();
            let Begin::Replay(r) = store.begin(&k, "POST", "/v1/x", h).await.unwrap() else {
                panic!("expected a replay");
            };
            assert_eq!(r.status, 409);
        }
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("select set_config('ruum.system', 'on', false)")
            .execute(&mut *conn)
            .await
            .unwrap();
        let bodies: Vec<Vec<u8>> = sqlx::query_scalar("select response_body from idempotency_keys")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert_eq!(bodies.len(), 2);
        for b in bodies {
            let b = String::from_utf8_lossy(&b);
            assert!(
                !b.contains("ruum_link_") && !b.contains("access_token"),
                "{b}"
            );
        }
    }
}

/// The actions of the audit entries of `tenant`, oldest first.
pub(crate) async fn audit_actions(a: &Area, tenant: TenantId) -> Vec<crate::audit::AuditEntry> {
    let ctx = ctx(tenant, UserId::new());
    let mut items = a
        .audit
        .list(&ctx, crate::audit::AuditQuery::default())
        .await
        .unwrap()
        .items;
    items.reverse();
    items
}
