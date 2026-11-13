//! The sign-in on an app origin (card G-1a; `contracts/apps/gateway.openapi.yaml`, "Sign-in on an
//! app origin"; design 9.5).
//!
//! The apps domain is another registered domain, so the platform session cookie never reaches
//! an app origin. The gateway runs a short code flow with the platform:
//!
//! 1. `/.ruum/v1/auth/start` on the app origin: the gateway signs a state (bound to the host and
//!    `return_to`) and sends the browser to the platform's `loginStart` with the app audience
//!    (`app_tenant`, `app_id`, `app_deployment`, `app_state`).
//! 2. The platform checks that the app and the deployment exist and serve, builds the host from
//!    its own records, and issues a one-time code ([`NewAppCode`]) to the signed-in user, bound to
//!    the user, the platform session, the app node, the environment, the deployment of a preview
//!    and the state. It redirects only to `https://<that host>/.ruum/v1/auth/callback`.
//! 3. `/.ruum/v1/auth/callback`: the gateway checks its state and redeems the code through the
//!    core's internal listener ([`super::IdentityStore::redeem_app_code`]): once, within
//!    [`APP_CODE_SECONDS`], only for the same binding and state. The result is an app session
//!    ([`AppSession`]); the cookie `__Host-ruum_app` holds its secret, never a token.
//!
//! An app session is refused by every other app, environment and preview
//! ([`AppSession::serves`]); it ends after [`APP_SESSION_HOURS`], at logout, and with the platform
//! session it came from (signing out of Ruum ends it at once; the gateway caches a check at most
//! a few seconds).

use crate::ids::{NodeId, TenantId, UserId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a sign-in code may be redeemed.
pub const APP_CODE_SECONDS: i64 = 60;
/// How long an app session lives (the `Max-Age` of `__Host-ruum_app`).
pub const APP_SESSION_HOURS: i64 = 12;
/// The longest state of the gateway that the platform takes.
pub const APP_STATE_MAX: usize = 2048;

/// The environment of an app origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEnv {
    Live,
    Preview,
}

impl AppEnv {
    pub fn as_str(self) -> &'static str {
        match self {
            AppEnv::Live => "live",
            AppEnv::Preview => "preview",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "live" => Some(AppEnv::Live),
            "preview" => Some(AppEnv::Preview),
            _ => None,
        }
    }
}

/// What an app session (and a sign-in code) is bound to: the app node, the environment and, for
/// a preview, the deployment. A live session survives a publish; a preview session opens only
/// its own preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppBinding {
    pub tenant_id: TenantId,
    pub app_id: NodeId,
    pub environment: AppEnv,
    /// The preview; `None` for the live app.
    pub deployment_id: Option<Uuid>,
}

impl AppBinding {
    /// The binding of a live app.
    pub fn live(tenant_id: TenantId, app_id: NodeId) -> Self {
        Self {
            tenant_id,
            app_id,
            environment: AppEnv::Live,
            deployment_id: None,
        }
    }

    /// The binding of a preview deployment.
    pub fn preview(tenant_id: TenantId, app_id: NodeId, deployment: Uuid) -> Self {
        Self {
            tenant_id,
            app_id,
            environment: AppEnv::Preview,
            deployment_id: Some(deployment),
        }
    }

    /// The binding of an environment: the deployment is kept only for a preview.
    pub fn of(tenant_id: TenantId, app_id: NodeId, environment: AppEnv, deployment: Uuid) -> Self {
        match environment {
            AppEnv::Live => Self::live(tenant_id, app_id),
            AppEnv::Preview => Self::preview(tenant_id, app_id, deployment),
        }
    }
}

/// A new sign-in code (the platform side of the code flow).
#[derive(Clone, Debug)]
pub struct NewAppCode {
    pub binding: AppBinding,
    pub user: UserId,
    /// The platform session of the request; the app session ends with it.
    pub platform_session: Uuid,
    /// The gateway's state (only its SHA-256 is stored).
    pub state: String,
}

/// A session of an app origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSession {
    pub id: Uuid,
    pub binding: AppBinding,
    pub user_id: UserId,
    pub platform_session_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl AppSession {
    /// Whether the session may open `binding`: the same tenant, app node and environment, and
    /// for a preview the same deployment.
    pub fn serves(&self, binding: &AppBinding) -> bool {
        self.binding == *binding
    }
}

/// The SHA-256 of a gateway state.
pub fn state_hash(state: &str) -> [u8; 32] {
    super::secret::sha256(&format!("ruum-app-state\0{state}"))
}

/// Whether `state` has the form the platform takes: 1 to [`APP_STATE_MAX`] characters of
/// base64url and `.` (a compact JWS).
pub fn valid_state(state: &str) -> bool {
    !state.is_empty()
        && state.len() <= APP_STATE_MAX
        && state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The pairwise subject of a principal (a person or an agent) for one app (design 9.4 and 9.6):
/// `pairwise:<22 characters of base64url>`, stable for this app and different for every other
/// app, so two apps cannot join their user lists. `key` is the tenant's pseudonym key (the core
/// holds it; the gateway never does), so the subject moves with a tenant export.
pub fn pairwise(key: &[u8; 32], app: NodeId, principal: Uuid) -> String {
    use base64::Engine as _;
    let mut msg = Vec::with_capacity(48);
    msg.extend_from_slice(b"ruum-app-pairwise\0");
    msg.extend_from_slice(app.0.as_bytes());
    msg.extend_from_slice(principal.as_bytes());
    let digest = blake3::keyed_hash(key, &msg);
    format!(
        "pairwise:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest.as_bytes()[..16])
    )
}

/// The small internal API between the app gateway and the core (`ruum-api`'s internal listener,
/// `RUUM_INTERNAL_BIND`, never reachable from the internet). JSON bodies of `POST` requests
/// under `/internal/gateway/v1/`. The gateway holds no tenant keys and no general system access:
/// it asks these narrow questions instead.
pub mod wire {
    use super::{AppBinding, AppEnv};
    use crate::ids::{NodeId, TenantId};
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// The path prefix of the gateway's internal API.
    pub const PREFIX: &str = "/internal/gateway/v1";

    /// `POST /deployment`: what the gateway needs about the deployment a host resolved to.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct DeploymentQuery {
        pub tenant_id: TenantId,
        pub app_id: NodeId,
        pub deployment_id: Uuid,
    }

    /// The answer of `POST /deployment` (`404` when the app or the deployment is gone).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct DeploymentInfo {
        pub environment: AppEnv,
        /// `platform` or `public` (`AppSettings.access`).
        pub access: String,
        /// `Tenant.app_approval.public_apps`: a public app passes anonymous visitors only while
        /// this approval is current (backlog S-33).
        pub public_approved: bool,
        /// The deployment serves: the live deployment of the app, or a ready preview that has not
        /// expired.
        pub serving: bool,
    }

    /// `POST /redeem`: redeem a sign-in code for an app session.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct RedeemRequest {
        pub binding: AppBinding,
        pub code: String,
        pub state: String,
    }

    /// The answer of `POST /redeem` (`400` for any refused code).
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct RedeemResponse {
        /// The secret of the cookie `__Host-ruum_app`.
        pub session: String,
        /// Seconds until the session expires (the cookie's `Max-Age`).
        pub max_age: i64,
    }

    /// `POST /session`: the check of an app session for one request.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct SessionRequest {
        pub binding: AppBinding,
        pub session: String,
    }

    /// The answer of `POST /session` (`404` when the session is not live or serves another app,
    /// environment or preview).
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SessionInfo {
        /// The pairwise subject of the person for this app.
        pub sub: String,
        /// `use` on the app node (live) or `open_preview` (preview).
        pub allowed: bool,
        /// The active app roles of the person (card B-2; empty until it is merged).
        pub roles: Vec<String>,
    }

    /// `POST /session/end`: logout on the app origin.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct EndSessionRequest {
        pub tenant_id: TenantId,
        pub session: String,
    }

    /// `POST /agent`: a bearer token that the gateway got on an app origin.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct AgentRequest {
        pub binding: AppBinding,
        pub token: String,
    }

    /// The answer of `POST /agent`.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum AgentAnswer {
        /// An agent token of the audience `ruumapps` (decision 0006-10).
        Agent {
            /// `open_preview` through its user and its scope, in the host's tenant.
            allowed: bool,
            /// The pairwise subject of the agent for this app.
            sub: String,
            /// The pairwise subject of its user for this app.
            on_behalf_of: String,
        },
        /// A credential of the platform that is not an agent token for apps (another audience
        /// or type): refused, never passed to the app.
        Platform,
        /// Not a platform credential: the app's own scheme, passed to the app unchanged.
        Foreign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_keep_the_deployment_only_for_previews() {
        let t = TenantId(Uuid::now_v7());
        let a = NodeId(Uuid::now_v7());
        let d = Uuid::now_v7();
        assert_eq!(AppBinding::of(t, a, AppEnv::Live, d).deployment_id, None);
        assert_eq!(
            AppBinding::of(t, a, AppEnv::Preview, d).deployment_id,
            Some(d)
        );
        let s = AppSession {
            id: Uuid::now_v7(),
            binding: AppBinding::preview(t, a, d),
            user_id: UserId(Uuid::now_v7()),
            platform_session_id: Uuid::now_v7(),
            created_at: crate::now(),
            expires_at: crate::now(),
        };
        assert!(s.serves(&AppBinding::preview(t, a, d)));
        assert!(!s.serves(&AppBinding::preview(t, a, Uuid::now_v7())));
        assert!(!s.serves(&AppBinding::live(t, a)));
        assert!(!s.serves(&AppBinding::preview(t, NodeId(Uuid::now_v7()), d)));
    }

    #[test]
    fn states_are_compact_and_bounded() {
        assert!(valid_state("eyJhbGc.eyJ4Ijox.c2ln-_"));
        assert!(!valid_state(""));
        assert!(!valid_state("a b"));
        assert!(!valid_state("a/b"));
        assert!(!valid_state(&"a".repeat(APP_STATE_MAX + 1)));
        assert_ne!(state_hash("a"), state_hash("b"));
    }

    #[test]
    fn pairwise_subjects_differ_per_app_and_fit_the_token_schema() {
        let key = [7u8; 32];
        let user = Uuid::now_v7();
        let a = NodeId(Uuid::now_v7());
        let b = NodeId(Uuid::now_v7());
        let sa = pairwise(&key, a, user);
        assert_eq!(sa, pairwise(&key, a, user));
        assert_ne!(sa, pairwise(&key, b, user));
        assert_ne!(sa, pairwise(&[8u8; 32], a, user));
        let id = sa.strip_prefix("pairwise:").unwrap();
        assert_eq!(id.len(), 22);
        assert!(
            id.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert!(!sa.contains(&user.to_string()));
    }
}

/// The conformance of the stores (memory and Postgres): one test per implementation.
#[cfg(test)]
mod conformance {
    use super::*;
    use crate::identity::VerifiedLogin;
    use crate::identity::tests::Area;

    async fn person(a: &Area, n: &str) -> (UserId, TenantId, Uuid) {
        let u = a
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://issuer.example".into(),
                subject: n.into(),
                email: Some(format!("{n}@example.test")),
                email_verified: true,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
        let (s, _) = a.identity.create_session(u.id, None).await.unwrap();
        (u.id, u.home_tenant_id, s.id)
    }

    async fn code(a: &Area, b: AppBinding, u: UserId, ps: Uuid) -> String {
        a.identity
            .create_app_code(NewAppCode {
                binding: b,
                user: u,
                platform_session: ps,
                state: "the-state".into(),
            })
            .await
            .unwrap()
    }

    async fn codes_are_bound_and_used_once(a: Area) {
        let (u, t, ps) = person(&a, "codes").await;
        let (app, dep) = (NodeId::new(), Uuid::now_v7());
        let b = AppBinding::preview(t, app, dep);
        // Another state, then the right one: the first try spent the code.
        let c = code(&a, b, u, ps).await;
        assert!(
            a.identity
                .redeem_app_code(&b, &c, "other")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.identity
                .redeem_app_code(&b, &c, "the-state")
                .await
                .unwrap()
                .is_none()
        );
        // Another preview, another app, the live app, another tenant.
        for other in [
            AppBinding::preview(t, app, Uuid::now_v7()),
            AppBinding::preview(t, NodeId::new(), dep),
            AppBinding::live(t, app),
            AppBinding::preview(TenantId::new(), app, dep),
        ] {
            let c = code(&a, b, u, ps).await;
            assert!(
                a.identity
                    .redeem_app_code(&other, &c, "the-state")
                    .await
                    .unwrap()
                    .is_none(),
                "{other:?}"
            );
        }
        // The right binding and state, once.
        let c = code(&a, b, u, ps).await;
        let (s, secret) = a
            .identity
            .redeem_app_code(&b, &c, "the-state")
            .await
            .unwrap()
            .expect("the code is redeemed");
        assert!(s.serves(&b) && s.user_id == u && s.platform_session_id == ps);
        let hours = (s.expires_at - s.created_at).whole_hours();
        assert_eq!(hours, APP_SESSION_HOURS);
        assert!(
            a.identity
                .redeem_app_code(&b, &c, "the-state")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.identity
                .redeem_app_code(&b, "made-up", "the-state")
                .await
                .unwrap()
                .is_none()
        );
        // The session, in its tenant only.
        let got = a.identity.app_session(t, &secret).await.unwrap().unwrap();
        assert_eq!(got, s);
        assert!(
            a.identity
                .app_session(TenantId::new(), &secret)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.identity
                .app_session(t, "made-up")
                .await
                .unwrap()
                .is_none()
        );
        // Logout ends it; ending it again is fine.
        a.identity.end_app_session(t, &secret).await.unwrap();
        a.identity.end_app_session(t, &secret).await.unwrap();
        assert!(a.identity.app_session(t, &secret).await.unwrap().is_none());
    }

    async fn app_sessions_end_with_the_platform_session(a: Area) {
        let (u, t, ps) = person(&a, "signout").await;
        let b = AppBinding::live(t, NodeId::new());
        let c = code(&a, b, u, ps).await;
        let (_, secret) = a
            .identity
            .redeem_app_code(&b, &c, "the-state")
            .await
            .unwrap()
            .unwrap();
        let pending = code(&a, b, u, ps).await;
        a.identity.end_session(u, ps).await.unwrap();
        assert!(a.identity.app_session(t, &secret).await.unwrap().is_none());
        // A code issued before the sign-out is refused, and no new code is issued.
        assert!(
            a.identity
                .redeem_app_code(&b, &pending, "the-state")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            a.identity
                .create_app_code(NewAppCode {
                    binding: b,
                    user: u,
                    platform_session: ps,
                    state: "s".into(),
                })
                .await
                .is_err()
        );
        // A code for another user's platform session is refused.
        let (_, _, other_ps) = person(&a, "someone-else").await;
        assert!(
            a.identity
                .create_app_code(NewAppCode {
                    binding: b,
                    user: u,
                    platform_session: other_ps,
                    state: "s".into(),
                })
                .await
                .is_err()
        );
    }

    async fn an_expired_code_is_refused(a: Area) {
        let Some(pool) = a.pool.clone() else {
            // The in-memory store has no clock to move; its expiry check is the same compare.
            return;
        };
        let (u, t, ps) = person(&a, "expired").await;
        let b = AppBinding::live(t, NodeId::new());
        let c = code(&a, b, u, ps).await;
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "update app_sign_in_codes set expires_at = now() - interval '1 second' \
             where tenant_id = $1",
        )
        .bind(t)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(
            a.identity
                .redeem_app_code(&b, &c, "the-state")
                .await
                .unwrap()
                .is_none()
        );
    }

    crate::conformance!(
        codes_are_bound_and_used_once,
        app_sessions_end_with_the_platform_session,
        an_expired_code_is_refused,
    );
}
