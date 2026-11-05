//! In-memory identity store and OIDC client, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryIdentityStore {}

#[async_trait]
impl IdentityStore for MemoryIdentityStore {
    async fn upsert_from_login(&self, _login: &VerifiedLogin) -> Result<User> {
        todo!("identity: upsert_from_login")
    }
    async fn get_user(&self, _id: UserId) -> Result<User> {
        todo!("identity: get_user")
    }
    async fn identities_of(&self, _user: UserId) -> Result<Vec<Identity>> {
        todo!("identity: identities_of")
    }
    async fn create_session(
        &self,
        _user: UserId,
        _user_agent: Option<String>,
    ) -> Result<(Session, String)> {
        todo!("identity: create_session")
    }
    async fn session_by_secret(&self, _secret: &str) -> Result<Session> {
        todo!("identity: session_by_secret")
    }
    async fn end_session(&self, _id: Uuid) -> Result<()> {
        todo!("identity: end_session")
    }
    async fn authenticate_bearer(&self, _token: &str) -> Result<Authenticated> {
        todo!("identity: authenticate_bearer")
    }
}

#[derive(Debug, Default)]
pub struct MemoryOidcClient {}

#[async_trait]
impl OidcClient for MemoryOidcClient {
    async fn start_login(
        &self,
        _return_to: Option<String>,
        _idp_hint: Option<String>,
    ) -> Result<LoginStart> {
        todo!("identity: start_login")
    }
    async fn finish_login(&self, _code: &str, _start: &LoginStart) -> Result<VerifiedLogin> {
        todo!("identity: finish_login")
    }
    async fn logout_url(&self, _id_token_hint: Option<String>) -> Result<String> {
        todo!("identity: logout_url")
    }
}

#[derive(Debug, Default)]
pub struct MemoryTokenIssuer {}

impl TokenIssuer for MemoryTokenIssuer {
    fn issue(&self, _claims: &AccessClaims) -> Result<String> {
        todo!("identity: issue")
    }
    fn verify(&self, _token: &str) -> Result<AccessClaims> {
        todo!("identity: verify")
    }
    fn jwks(&self) -> serde_json::Value {
        todo!("identity: jwks")
    }
}
