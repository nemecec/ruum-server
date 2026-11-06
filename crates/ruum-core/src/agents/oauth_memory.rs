//! The in-memory OAuth server. It shares the state of the area ([`MemoryArea`]).

use super::*;
use crate::actor::PrincipalKind;
use crate::agents::check_new_agent;
use crate::agents::memory::MemoryAgentStore;
use crate::identity::memory::{MemoryArea, OAuthCodeRow, OAuthGrantRow};
use crate::ids::UserId;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct MemoryOAuthServer {
    area: Arc<MemoryArea>,
}

impl MemoryOAuthServer {
    pub fn new(area: Arc<MemoryArea>) -> Self {
        Self { area }
    }
}

/// Opens the sealed request with `tokens`.
pub(crate) fn open_request(
    tokens: &dyn crate::identity::TokenIssuer,
    request: &str,
) -> Result<AuthorizationRequest> {
    let v = tokens.open(REQUEST_PURPOSE, request)?;
    serde_json::from_value(v).map_err(|_| crate::identity::invalid("the request is not valid"))
}

/// The checks of the consent that need no store: the request, the user, the shape.
pub(crate) fn check_consent(
    tokens: &dyn crate::identity::TokenIssuer,
    who: &Actor,
    consent: &Consent,
) -> Result<(UserId, AuthorizationRequest)> {
    if !who.is_user() || who.actor.kind != PrincipalKind::User {
        return Err(Error::user_required());
    }
    let req = open_request(tokens, &consent.request)?;
    if consent.approve {
        if consent.tenant_id.is_none() {
            return Err(crate::identity::invalid("tenant_id is needed to approve"));
        }
        match (&consent.agent_id, &consent.new_agent) {
            (Some(_), Some(_)) => {
                return Err(crate::identity::invalid(
                    "give agent_id or new_agent, not both",
                ));
            }
            (None, None) => {
                return Err(crate::identity::invalid("give agent_id or new_agent"));
            }
            _ => {}
        }
    }
    Ok((UserId(who.actor.id.0), req))
}

#[async_trait]
impl OAuthServer for MemoryOAuthServer {
    async fn register_client(
        &self,
        req: ClientRegistration,
    ) -> std::result::Result<OAuthClient, OAuthError> {
        let req = check_registration(req)?;
        let client = OAuthClient {
            client_id: new_client_id(),
            client_id_issued_at: crate::now().unix_timestamp(),
            client_name: req.client_name,
            redirect_uris: req.redirect_uris,
            grant_types: req.grant_types,
            response_types: req.response_types,
            token_endpoint_auth_method: "none".into(),
            client_uri: req.client_uri,
            logo_uri: req.logo_uri,
        };
        self.area
            .lock()
            .oauth_clients
            .insert(client.client_id.clone(), client.clone());
        Ok(client)
    }

    async fn get_client(&self, client_id: &str) -> Result<OAuthClient> {
        self.area
            .lock()
            .oauth_clients
            .get(client_id)
            .cloned()
            .ok_or_else(|| Error::not_found("OAuth client"))
    }

    async fn start_authorization(&self, req: AuthorizationRequest) -> Result<String> {
        let client = self.get_client(&req.client_id).await?;
        if !client.redirect_uris.contains(&req.redirect_uri) {
            return Err(crate::identity::invalid(
                "the redirect URI is not registered for this client",
            ));
        }
        if !secret::is_pkce_value(&req.code_challenge) {
            return Err(crate::identity::invalid("the code challenge is not valid"));
        }
        let value =
            serde_json::to_value(&req).map_err(|e| Error::Internal(format!("seal: {e}")))?;
        self.area
            .deps
            .tokens
            .seal(REQUEST_PURPOSE, &value, REQUEST_SECONDS)
    }

    fn open_request(&self, request: &str) -> Result<AuthorizationRequest> {
        open_request(self.area.deps.tokens.as_ref(), request)
    }

    async fn consent(&self, who: &Actor, consent: Consent) -> Result<String> {
        let tokens = self.area.deps.tokens.clone();
        let (user, req) = check_consent(tokens.as_ref(), who, &consent)?;
        let issuer = tokens.issuer().to_string();
        if !consent.approve {
            if let Some(t) = consent.tenant_id
                && self.area.lock().role_of(t, user).is_some()
            {
                self.area
                    .audit(
                        &consent_context(who, t),
                        "agent.oauth_denied",
                        "oauth_client",
                        &req.client_id,
                        serde_json::json!({}),
                    )
                    .await?;
            }
            return redirect_with(
                &req.redirect_uri,
                &[("error", "access_denied")],
                req.state.as_deref(),
                &issuer,
            );
        }
        let tenant = consent.tenant_id.expect("checked");
        if self.area.lock().role_of(tenant, user).is_none() {
            return Err(Error::not_found("tenant"));
        }
        let ctx = consent_context(who, tenant);
        // The client must still exist.
        self.get_client(&req.client_id).await?;
        let agent = match (consent.agent_id, consent.new_agent) {
            (Some(id), _) => {
                let st = self.area.lock();
                let a = st
                    .agents
                    .get(&id)
                    .filter(|a| a.tenant_id == tenant && a.owner_id == user)
                    .cloned()
                    .ok_or_else(|| Error::not_found("agent"))?;
                if a.disabled_at.is_some() {
                    return Err(crate::agents::agent_disabled());
                }
                a
            }
            (None, Some(new)) => {
                let new = check_new_agent(&new)?;
                if let Some(s) = new.scope.subtree {
                    self.area.check_node(&ctx, s).await?;
                }
                MemoryAgentStore::insert(&self.area, &ctx, user, new).await?
            }
            (None, None) => unreachable!("checked"),
        };
        let code = secret::random_secret("");
        {
            let mut st = self.area.lock();
            let now = crate::now();
            for g in st.oauth_grants.values_mut() {
                if g.tenant_id == tenant
                    && g.agent_id == agent.id
                    && g.client_id == req.client_id
                    && g.ended_at.is_none()
                {
                    g.ended_at = Some(now);
                }
            }
            let grant = OAuthGrantRow {
                id: Uuid::now_v7(),
                tenant_id: tenant,
                agent_id: agent.id,
                client_id: req.client_id.clone(),
                audience: req.resource.clone(),
                refresh_hash: None,
                previous_refresh_hash: None,
                refresh_expires_at: None,
                ended_at: None,
            };
            st.oauth_codes.insert(
                secret::sha256(&code),
                OAuthCodeRow {
                    grant_id: grant.id,
                    code_challenge: req.code_challenge.clone(),
                    redirect_uri: req.redirect_uri.clone(),
                    expires_at: now + time::Duration::seconds(CODE_SECONDS),
                    used_at: None,
                },
            );
            st.oauth_grants.insert(grant.id, grant);
        }
        self.area
            .audit(
                &ctx,
                "agent.oauth_approved",
                "agent",
                agent.id,
                serde_json::json!({ "client_id": req.client_id }),
            )
            .await?;
        redirect_with(
            &req.redirect_uri,
            &[("code", &code)],
            req.state.as_deref(),
            &issuer,
        )
    }

    async fn token(&self, req: TokenRequest) -> std::result::Result<TokenResponse, OAuthError> {
        let now = crate::now();
        match req {
            TokenRequest::AuthorizationCode {
                code,
                code_verifier,
                client_id,
                redirect_uri,
                resource,
            } => {
                if !secret::is_pkce_value(&code_verifier) {
                    return Err(OAuthError::invalid_request("code_verifier is not valid"));
                }
                let (agent, audience, refresh) = {
                    let mut st = self.area.lock();
                    let hash = secret::sha256(&code);
                    let row = st
                        .oauth_codes
                        .get_mut(&hash)
                        .ok_or_else(|| OAuthError::invalid_grant("the code is not valid"))?;
                    let grant_id = row.grant_id;
                    if row.used_at.is_some() {
                        // A replayed code ends the grant (RFC 6749, section 4.1.2).
                        if let Some(g) = st.oauth_grants.get_mut(&grant_id) {
                            g.ended_at.get_or_insert(now);
                        }
                        return Err(OAuthError::invalid_grant("the code was already used"));
                    }
                    row.used_at = Some(now);
                    if row.expires_at <= now {
                        return Err(OAuthError::invalid_grant("the code has expired"));
                    }
                    if row.redirect_uri != redirect_uri {
                        return Err(OAuthError::invalid_grant("redirect_uri does not match"));
                    }
                    if secret::pkce_challenge(&code_verifier) != row.code_challenge {
                        return Err(OAuthError::invalid_grant(
                            "the PKCE verifier does not match",
                        ));
                    }
                    let g = st
                        .oauth_grants
                        .get(&grant_id)
                        .filter(|g| g.ended_at.is_none())
                        .ok_or_else(|| OAuthError::invalid_grant("the grant has ended"))?;
                    if g.client_id != client_id {
                        return Err(OAuthError::invalid_grant("the code is for another client"));
                    }
                    if resource
                        .as_deref()
                        .is_some_and(|r| r.trim_end_matches('/') != g.audience)
                    {
                        return Err(OAuthError::new(
                            "invalid_target",
                            "resource does not match the authorization",
                            ErrorCode::ValidationFailed,
                        ));
                    }
                    let agent = st
                        .agents
                        .get(&g.agent_id)
                        .cloned()
                        .ok_or_else(|| OAuthError::invalid_grant("the agent does not exist"))?;
                    crate::agents::check_live(&agent)?;
                    let audience = g.audience.clone();
                    let refresh = secret::random_secret(REFRESH_PREFIX);
                    if let Some(g) = st.oauth_grants.get_mut(&grant_id) {
                        g.refresh_hash = Some(secret::sha256(&refresh));
                        g.previous_refresh_hash = None;
                        g.refresh_expires_at = Some(now + time::Duration::days(REFRESH_DAYS));
                    }
                    if let Some(a) = st.agents.get_mut(&agent.id) {
                        a.last_used_at = Some(now);
                    }
                    (agent, audience, refresh)
                };
                let access = MemoryAgentStore::token_for(&self.area, &agent, Some(&audience))?;
                Ok(TokenResponse {
                    access,
                    refresh_token: Some(refresh),
                })
            }
            TokenRequest::RefreshToken {
                refresh_token,
                client_id,
                resource,
            } => {
                let hash = secret::sha256(&refresh_token);
                let (agent, audience, refresh) = {
                    let mut st = self.area.lock();
                    let live = st
                        .oauth_grants
                        .values()
                        .find(|g| g.ended_at.is_none() && g.refresh_hash == Some(hash))
                        .map(|g| g.id);
                    let Some(grant_id) = live else {
                        // Reuse of an old refresh token ends the grant.
                        if let Some(g) = st
                            .oauth_grants
                            .values_mut()
                            .find(|g| g.previous_refresh_hash == Some(hash))
                        {
                            g.ended_at.get_or_insert(now);
                        }
                        return Err(OAuthError::invalid_grant("the refresh token is not valid"));
                    };
                    let g = st.oauth_grants.get(&grant_id).expect("found above");
                    if g.client_id != client_id {
                        return Err(OAuthError::invalid_grant(
                            "the refresh token is for another client",
                        ));
                    }
                    if g.refresh_expires_at.is_none_or(|e| e <= now) {
                        return Err(OAuthError::invalid_grant("the refresh token has expired"));
                    }
                    if resource
                        .as_deref()
                        .is_some_and(|r| r.trim_end_matches('/') != g.audience)
                    {
                        return Err(OAuthError::new(
                            "invalid_target",
                            "resource does not match the authorization",
                            ErrorCode::ValidationFailed,
                        ));
                    }
                    let agent = st
                        .agents
                        .get(&g.agent_id)
                        .cloned()
                        .ok_or_else(|| OAuthError::invalid_grant("the agent does not exist"))?;
                    crate::agents::check_live(&agent)?;
                    let audience = g.audience.clone();
                    let refresh = secret::random_secret(REFRESH_PREFIX);
                    if let Some(g) = st.oauth_grants.get_mut(&grant_id) {
                        g.previous_refresh_hash = g.refresh_hash;
                        g.refresh_hash = Some(secret::sha256(&refresh));
                        g.refresh_expires_at = Some(now + time::Duration::days(REFRESH_DAYS));
                    }
                    if let Some(a) = st.agents.get_mut(&agent.id) {
                        a.last_used_at = Some(now);
                    }
                    (agent, audience, refresh)
                };
                let access = MemoryAgentStore::token_for(&self.area, &agent, Some(&audience))?;
                Ok(TokenResponse {
                    access,
                    refresh_token: Some(refresh),
                })
            }
            TokenRequest::TokenExchange { .. } => Err(OAuthError::new(
                "unsupported_grant_type",
                "the token exchange goes to the agent store",
                ErrorCode::BadRequest,
            )),
        }
    }

    async fn end_grants(&self, tenant: TenantId, agent: AgentId) -> Result<u32> {
        let mut st = self.area.lock();
        let now = crate::now();
        let mut n = 0;
        for g in st.oauth_grants.values_mut() {
            if g.tenant_id == tenant && g.agent_id == agent && g.ended_at.is_none() {
                g.ended_at = Some(now);
                n += 1;
            }
        }
        Ok(n)
    }
}
