//! The sharing rules that the in-memory and the Postgres store share: who may grant which role,
//! the tenant policy for links and guests, share link tokens and passwords, and list cursors.

use super::{Grantee, NewShareLink, ShareLink, ShareLinkRole};
use crate::actor::{Context, PrincipalKind};
use crate::authz::{self, Authz, Permission, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{GrantId, NodeId};
use crate::nodes::NodeStore;
use crate::paging::Cursor;
use crate::tenants::TenantPolicy;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// The visible prefix of share link tokens, so secret scanners can find leaked ones.
pub const LINK_TOKEN_PREFIX: &str = "ruum_link_";

/// How long the access token of `resolveShareLink` lives, at most (seconds). It never outlives
/// the link.
pub const LINK_ACCESS_TOKEN_SECONDS: i64 = 3600;

/// A new secret share link token: the prefix and 32 random bytes in base64url.
pub fn new_link_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("{LINK_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// The SHA-256 of a token, as stored in `share_links.token_hash`.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// An Argon2id PHC string of a link password.
pub fn hash_password(password: &str) -> Result<String> {
    use argon2::PasswordHasher as _;
    argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| Error::Internal(format!("argon2: {e}")))
}

/// Whether `password` matches the PHC string.
pub fn verify_password(phc: &str, password: &str) -> bool {
    use argon2::PasswordVerifier as _;
    let Ok(hash) = argon2::PasswordHash::new(phc) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok()
}

/// The answer to a user grantee who is not a member or guest of the tenant, the same as for an
/// unknown user ID.
pub fn no_such_user() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::NotFound, "no such user in this tenant").with_hint(
            "grant to an email address, or invite the person to the tenant; a grant by user ID \
             works only for members and guests",
        ),
    )
}

pub fn validation(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::ValidationFailed, message)
}

fn denied(message: &str, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::PermissionDenied, message).with_hint(hint))
}

/// Sharing needs a user or an agent. A share-link bearer gets `user_required` (the `Users only`
/// section of the contract: links may only read).
pub fn refuse_share_links(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

/// The roles of the caller on `node`: in the graph (its user, or the link) and after the scope.
pub async fn caller_roles(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
) -> Result<(Option<Role>, Option<Role>)> {
    let graph = authz::graph_role(authz, &authz::subject_of(ctx), node).await?;
    let effective = authz::role_on(authz, nodes, ctx, node).await?;
    Ok((graph, effective))
}

/// Fails unless `role` is at most the caller's role: `permission_denied` above the graph role,
/// `scope_exceeded` above the role after the scope.
pub fn at_most_own_role(role: Role, graph: Option<Role>, effective: Option<Role>) -> Result<()> {
    if graph.is_none_or(|g| role > g) {
        return Err(denied(
            "you can grant at most your own role on this node",
            "choose a lower role, or ask a manager of the node",
        ));
    }
    if effective.is_none_or(|e| role > e) {
        return Err(authz::scope_exceeded());
    }
    Ok(())
}

/// The check of `addGrant` and of a new share link role: `share` on the node, and the role at
/// most the caller's own role.
pub async fn check_grant(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
    role: Role,
) -> Result<()> {
    refuse_share_links(ctx)?;
    authz::require(authz, nodes, ctx, node, Permission::Share).await?;
    let (graph, effective) = caller_roles(authz, nodes, ctx, node).await?;
    at_most_own_role(role, graph, effective)
}

/// The check of `updateGrant` and `removeGrant`: `share` on the node; the caller sets at most
/// its own role and changes (or removes) a grant above its own role only with `manage` (which is
/// the highest role, so: never above its own role).
pub async fn check_change(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    node: NodeId,
    old: Role,
    new: Option<Role>,
) -> Result<()> {
    refuse_share_links(ctx)?;
    authz::require(authz, nodes, ctx, node, Permission::Share).await?;
    let (graph, effective) = caller_roles(authz, nodes, ctx, node).await?;
    at_most_own_role(old, graph, effective)?;
    if let Some(new) = new {
        at_most_own_role(new, graph, effective)?;
    }
    Ok(())
}

/// Checks the grantee of a new grant. Returns the email in its stored form for email grants.
pub fn check_grantee(grantee: &Grantee) -> Result<Option<String>> {
    match grantee {
        Grantee::Email { email } => {
            let e = email.trim();
            let valid = e.len() <= 320
                && e.split_once('@')
                    .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'))
                && !e.chars().any(char::is_whitespace);
            if valid {
                Ok(Some(e.to_string()))
            } else {
                Err(validation("the email address is not valid"))
            }
        }
        _ => Ok(None),
    }
}

/// The tenant policy refuses people outside the tenant.
pub fn guests_refused() -> Error {
    denied(
        "the tenant policy does not allow sharing with people outside the tenant",
        "ask a tenant admin to allow guests",
    )
}

/// Checks a new share link against the tenant policy and the caller's role.
pub fn check_new_link(
    policy: &TenantPolicy,
    req: &NewShareLink,
    now: OffsetDateTime,
) -> Result<()> {
    if !policy.allow_share_links {
        return Err(denied(
            "the tenant policy does not allow share links",
            "ask a tenant admin to allow share links",
        ));
    }
    if req.role == ShareLinkRole::Edit && !policy.allow_edit_links {
        return Err(denied(
            "the tenant policy does not allow edit links",
            "make a view or comment link, or ask a tenant admin to allow edit links",
        ));
    }
    if let Some(at) = req.expires_at
        && at <= now
    {
        return Err(validation("expires_at must be in the future"));
    }
    if let Some(days) = policy.share_link_max_days {
        let limit = now + time::Duration::days(i64::from(days));
        match req.expires_at {
            None => {
                return Err(validation(format!(
                    "the tenant policy needs an expiry at most {days} days ahead"
                )));
            }
            Some(at) if at > limit => {
                return Err(validation(format!(
                    "the tenant policy allows an expiry at most {days} days ahead"
                )));
            }
            _ => {}
        }
    }
    if let Some(p) = &req.password {
        let n = p.chars().count();
        if !(8..=200).contains(&n) {
            return Err(validation("the password must have 8 to 200 characters"));
        }
    }
    Ok(())
}

/// Whether a stored link can be opened now: not revoked, not expired, and allowed by the policy.
pub fn link_is_open(policy: &TenantPolicy, link: &ShareLink, now: OffsetDateTime) -> bool {
    link.revoked_at.is_none()
        && link.expires_at.is_none_or(|t| t > now)
        && policy.allow_share_links
        && (link.role != ShareLinkRole::Edit || policy.allow_edit_links)
}

/// The scope of a share-link bearer: the link's node, role and expiry.
pub fn link_scope(link: &ShareLink) -> authz::Scope {
    authz::Scope {
        subtree: Some(link.node_id),
        max_role: link.role.as_role(),
        expires_at: link.expires_at,
    }
}

/// The context of a share-link bearer in the link's tenant.
pub fn link_context(tenant: crate::ids::TenantId, link: &ShareLink) -> Context {
    Context {
        tenant,
        actor: crate::actor::Actor {
            actor: crate::actor::PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: crate::ids::PrincipalId(link.id.0),
            },
            on_behalf_of: None,
        },
        scope: Some(link_scope(link)),
        idempotency_key: None,
    }
}

/// How many wrong passwords one share link accepts per [`LINK_PASSWORD_WINDOW`] before
/// `resolveShareLink` answers `429 rate_limited` without running Argon2.
pub const LINK_PASSWORD_FAILURES: u32 = 10;
/// The window of [`LINK_PASSWORD_FAILURES`].
pub const LINK_PASSWORD_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Wrong password attempts per link token hash, in this process: `(window start, failures)`.
/// A limit per client IP belongs to the API (the core does not see the IP).
type FailuresByLink = std::collections::HashMap<Vec<u8>, (std::time::Instant, u32)>;
static LINK_PASSWORD_FAILURES_BY_LINK: std::sync::LazyLock<std::sync::Mutex<FailuresByLink>> =
    std::sync::LazyLock::new(Default::default);

/// Checks the password of a password-protected link (`phc`). A link that had
/// [`LINK_PASSWORD_FAILURES`] wrong passwords in the current window gets `rate_limited` before
/// the Argon2 verify, so nobody can brute-force the password or burn CPU with it. Only wrong
/// passwords count; wrong attempts by others can block a link for at most one window at a time.
pub fn check_link_password(token_hash: &[u8], phc: &str, password: Option<&str>) -> Result<()> {
    let now = std::time::Instant::now();
    {
        let m = LINK_PASSWORD_FAILURES_BY_LINK
            .lock()
            .expect("link password limiter");
        if let Some((start, n)) = m.get(token_hash)
            && now.duration_since(*start) < LINK_PASSWORD_WINDOW
            && *n >= LINK_PASSWORD_FAILURES
        {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::RateLimited,
                    "too many wrong passwords for this share link",
                )
                .with_hint("wait a minute and try again"),
            ));
        }
    }
    if password.is_some_and(|p| verify_password(phc, p)) {
        return Ok(());
    }
    let mut m = LINK_PASSWORD_FAILURES_BY_LINK
        .lock()
        .expect("link password limiter");
    if m.len() > 10_000 {
        m.retain(|_, (start, _)| now.duration_since(*start) < LINK_PASSWORD_WINDOW);
    }
    let e = m.entry(token_hash.to_vec()).or_insert((now, 0));
    if now.duration_since(e.0) >= LINK_PASSWORD_WINDOW {
        *e = (now, 0);
    }
    // A missing password costs no Argon2 run and does not count.
    if password.is_some() {
        e.1 += 1;
    }
    Err(wrong_password())
}

/// The wrong-password answer of `resolveShareLink`.
pub fn wrong_password() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Unauthenticated,
            "this share link needs a password",
        )
        .with_hint("send the password of the link in `password`"),
    )
}

/// A cursor over a UUIDv7 key (lists ordered by ID).
pub fn id_cursor(id: uuid::Uuid) -> Cursor {
    Cursor(URL_SAFE_NO_PAD.encode(id.as_bytes()))
}

/// Reads a cursor of [`id_cursor`].
pub fn parse_id_cursor(c: &Cursor) -> Result<uuid::Uuid> {
    URL_SAFE_NO_PAD
        .decode(&c.0)
        .ok()
        .and_then(|b| uuid::Uuid::from_slice(&b).ok())
        .ok_or_else(|| Error::api(ErrorCode::BadRequest, "the cursor is not valid"))
}

/// A cursor of "Shared with me": the position `(shared_at, node_id)` of the last item.
pub fn shared_cursor(at: OffsetDateTime, node: NodeId) -> Cursor {
    let mut b = at.unix_timestamp_nanos().to_be_bytes().to_vec();
    b.extend_from_slice(node.0.as_bytes());
    Cursor(URL_SAFE_NO_PAD.encode(b))
}

/// Reads a cursor of [`shared_cursor`].
pub fn parse_shared_cursor(c: &Cursor) -> Result<(OffsetDateTime, NodeId)> {
    let bad = || Error::api(ErrorCode::BadRequest, "the cursor is not valid");
    let b = URL_SAFE_NO_PAD.decode(&c.0).map_err(|_| bad())?;
    if b.len() != 32 {
        return Err(bad());
    }
    let nanos = i128::from_be_bytes(b[..16].try_into().map_err(|_| bad())?);
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let node = uuid::Uuid::from_slice(&b[16..]).map_err(|_| bad())?;
    Ok((at, NodeId(node)))
}

/// Keeps one row per `(tenant, node)` of "Shared with me": the one with the highest role (a
/// node can have a direct grant to the user and a grant to `tenant#member`). The first row wins
/// a tie, so callers put the direct rows first.
pub fn one_per_node<T>(
    rows: Vec<T>,
    key: impl Fn(&T) -> (crate::ids::TenantId, NodeId),
    role: impl Fn(&T) -> Role,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    let mut at: std::collections::HashMap<(crate::ids::TenantId, NodeId), usize> =
        std::collections::HashMap::new();
    for r in rows {
        match at.get(&key(&r)) {
            Some(&i) if role(&r) > role(&out[i]) => out[i] = r,
            Some(_) => {}
            None => {
                at.insert(key(&r), out.len());
                out.push(r);
            }
        }
    }
    out
}

/// The data of `grant.added`. An email grant carries `may_add_guest: true` (decision 0003-2):
/// it binds to whoever proves the address, and that person may become a guest. Every stored
/// email grant may do so (callers who may list the members get a user grant for the persons of
/// the tenant, and while guests are off an email grant to an outsider is refused), so the flag
/// tells a reader of the feed nothing about the address. Admins find "who invited outsiders"
/// in the audit log by it.
pub fn grant_added_data(grant: GrantId, grantee: &Grantee, role: Role) -> serde_json::Value {
    let mut data = serde_json::json!({ "grant_id": grant, "grantee": grantee, "role": role });
    if matches!(grantee, Grantee::Email { .. }) {
        data["may_add_guest"] = serde_json::Value::Bool(true);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = new_link_token();
        let b = new_link_token();
        assert_ne!(a, b);
        assert!(a.starts_with(LINK_TOKEN_PREFIX));
        assert_eq!(hash_token(&a).len(), 32);
        assert_eq!(hash_token(&a), hash_token(&a));
    }

    #[test]
    fn passwords_verify() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "correct horse"));
        assert!(!verify_password(&h, "wrong horse"));
        assert!(!verify_password("garbage", "x"));
    }

    #[test]
    fn grant_roles_are_capped() {
        assert!(at_most_own_role(Role::Edit, Some(Role::Edit), Some(Role::Edit)).is_ok());
        let code = |r: Result<()>| match r {
            Err(Error::Api(e)) => Some(e.code),
            _ => None,
        };
        assert_eq!(
            code(at_most_own_role(
                Role::Manage,
                Some(Role::Edit),
                Some(Role::Edit)
            )),
            Some(ErrorCode::PermissionDenied)
        );
        assert_eq!(
            code(at_most_own_role(
                Role::Edit,
                Some(Role::Manage),
                Some(Role::View)
            )),
            Some(ErrorCode::ScopeExceeded)
        );
    }

    #[test]
    fn link_policy() {
        let now = OffsetDateTime::now_utc();
        let req = |role, days: Option<i64>| NewShareLink {
            role,
            expires_at: days.map(|d| now + time::Duration::days(d)),
            password: None,
        };
        let p = TenantPolicy::default();
        assert!(check_new_link(&p, &req(ShareLinkRole::View, None), now).is_ok());
        assert!(check_new_link(&p, &req(ShareLinkRole::Edit, None), now).is_err());
        assert!(check_new_link(&p, &req(ShareLinkRole::View, Some(-1)), now).is_err());
        let p = TenantPolicy {
            share_link_max_days: Some(7),
            allow_edit_links: true,
            ..TenantPolicy::default()
        };
        assert!(check_new_link(&p, &req(ShareLinkRole::Edit, Some(3)), now).is_ok());
        assert!(check_new_link(&p, &req(ShareLinkRole::View, None), now).is_err());
        assert!(check_new_link(&p, &req(ShareLinkRole::View, Some(8)), now).is_err());
        let p = TenantPolicy {
            allow_share_links: false,
            ..TenantPolicy::default()
        };
        assert!(check_new_link(&p, &req(ShareLinkRole::View, None), now).is_err());
        let mut short = req(ShareLinkRole::View, None);
        short.password = Some("short".into());
        assert!(check_new_link(&TenantPolicy::default(), &short, now).is_err());
    }

    #[test]
    fn cursors_round_trip() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(parse_id_cursor(&id_cursor(id)).unwrap(), id);
        let at = OffsetDateTime::now_utc();
        let n = NodeId::new();
        let (at2, n2) = parse_shared_cursor(&shared_cursor(at, n)).unwrap();
        assert_eq!(
            (at2.unix_timestamp_nanos(), n2),
            (at.unix_timestamp_nanos(), n)
        );
        assert!(parse_id_cursor(&Cursor("!!".into())).is_err());
    }

    #[test]
    fn emails_are_checked() {
        let e = |s: &str| check_grantee(&Grantee::Email { email: s.into() });
        assert_eq!(e(" a@b.ee ").unwrap(), Some("a@b.ee".into()));
        assert!(e("nope").is_err());
        assert!(e("a@b").is_err());
        assert!(e("a b@c.ee").is_err());
    }
}
