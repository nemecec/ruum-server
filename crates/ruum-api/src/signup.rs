//! The email allow-list of a server that is not open to everyone (staging, review 1 B-5).
//!
//! When the list is set, only the addresses on it can sign in (a sign-in creates the user and
//! their personal tenant) and create team tenants. Everyone else gets `401 unauthenticated` at
//! the sign-in callback and `403 permission_denied` at `createTenant`. Without the list, the
//! server is open, as before.
//!
//! Settings (read by `ruum-server`):
//!
//! | Name | What |
//! |---|---|
//! | `RUUM_SIGNUP_ALLOWED_EMAILS` | Comma- or space-separated entries. `alice@example.com` allows that address; `@example.com` allows the domain. The value `none` allows nobody. The email must be verified by the identity provider. |
//! | `RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS` | Exact addresses that may sign in also when the provider does not verify the email (Microsoft accounts on staging: Zitadel marks their email as not verified, review 2 section 7). No domains here: an unverified domain match lets anyone in. |
//!
//! Matching ignores case and surrounding space. A list that is set but holds no valid entry
//! allows nobody (fail closed).

use std::collections::BTreeSet;

/// The parsed allow-list. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailAllowList {
    /// Exact addresses that must be verified.
    emails: BTreeSet<String>,
    /// Domains (without `@`) whose verified addresses are allowed.
    domains: BTreeSet<String>,
    /// Exact addresses that are allowed also when not verified.
    unverified: BTreeSet<String>,
}

fn entries(raw: &str) -> impl Iterator<Item = String> + '_ {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
}

/// A plausible address: one `@`, something before it, a dot in the domain.
fn is_address(e: &str) -> bool {
    match e.split_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.contains('@') && is_domain(domain),
        None => false,
    }
}

fn is_domain(d: &str) -> bool {
    d.contains('.') && !d.starts_with('.') && !d.ends_with('.') && !d.contains('@')
}

impl EmailAllowList {
    /// Parses the two settings. Returns `None` when neither is set (the server is open).
    /// Entries that are not an address or `@domain` are ignored and logged by name only.
    pub fn from_settings(verified: Option<&str>, unverified: Option<&str>) -> Option<Self> {
        if verified.is_none() && unverified.is_none() {
            return None;
        }
        let mut list = Self::default();
        for e in verified.into_iter().flat_map(entries) {
            if e == "none" {
                continue;
            }
            if let Some(domain) = e.strip_prefix('@') {
                if is_domain(domain) {
                    list.domains.insert(domain.to_string());
                    continue;
                }
            } else if is_address(&e) {
                list.emails.insert(e);
                continue;
            }
            tracing::warn!(
                "RUUM_SIGNUP_ALLOWED_EMAILS: an entry is not an address or @domain; ignored"
            );
        }
        for e in unverified.into_iter().flat_map(entries) {
            if e == "none" {
                continue;
            }
            if is_address(&e) {
                list.unverified.insert(e);
            } else {
                tracing::warn!(
                    "RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS: an entry is not an exact address; ignored"
                );
            }
        }
        Some(list)
    }

    /// True when `email` may sign up. `verified` is the provider's `email_verified`.
    pub fn allows(&self, email: Option<&str>, verified: bool) -> bool {
        let Some(email) = email.map(|e| e.trim().to_ascii_lowercase()) else {
            return false;
        };
        if !is_address(&email) {
            return false;
        }
        if self.unverified.contains(&email) {
            return true;
        }
        if !verified {
            return false;
        }
        if self.emails.contains(&email) {
            return true;
        }
        match email.rsplit_once('@') {
            Some((_, domain)) => self.domains.contains(domain),
            None => false,
        }
    }

    /// The number of entries, for the start-up log (never the addresses).
    pub fn len(&self) -> usize {
        self.emails.len() + self.domains.len() + self.unverified.len()
    }

    /// True when the list allows nobody.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(v: &str, u: &str) -> EmailAllowList {
        EmailAllowList::from_settings(Some(v), Some(u)).unwrap()
    }

    #[test]
    fn no_settings_means_open() {
        assert_eq!(EmailAllowList::from_settings(None, None), None);
    }

    #[test]
    fn exact_addresses_must_be_verified() {
        let l = list("Alice@Example.com, bob@example.org", "");
        assert!(l.allows(Some("alice@example.com"), true));
        assert!(l.allows(Some("  ALICE@example.COM "), true));
        assert!(!l.allows(Some("alice@example.com"), false));
        assert!(l.allows(Some("bob@example.org"), true));
        assert!(!l.allows(Some("carol@example.com"), true));
        assert!(!l.allows(None, true));
    }

    #[test]
    fn domains_allow_verified_addresses_of_that_domain_only() {
        let l = list("@example.com", "");
        assert!(l.allows(Some("anyone@example.com"), true));
        assert!(!l.allows(Some("anyone@example.com"), false));
        assert!(!l.allows(Some("anyone@sub.example.com"), true));
        assert!(!l.allows(Some("anyone@evilexample.com"), true));
        assert!(!l.allows(Some("example.com"), true));
    }

    #[test]
    fn unverified_entries_are_exact_addresses() {
        let l = list("", "me@outlook.com @outlook.com");
        assert!(l.allows(Some("me@outlook.com"), false));
        assert!(l.allows(Some("me@outlook.com"), true));
        assert!(!l.allows(Some("other@outlook.com"), false));
        assert!(!l.allows(Some("other@outlook.com"), true));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn none_or_garbage_allows_nobody() {
        for v in ["none", "not-an-address", "@", "a@b", "@nodot", ",, ,"] {
            let l = list(v, "");
            assert!(l.is_empty(), "{v}");
            assert!(!l.allows(Some("alice@example.com"), true), "{v}");
        }
    }

    #[test]
    fn a_set_but_empty_unverified_list_still_closes_the_server() {
        let l = EmailAllowList::from_settings(None, Some("")).unwrap();
        assert!(!l.allows(Some("alice@example.com"), true));
    }
}

/// The allow-list over HTTP: the sign-in callback and `createTenant`.
#[cfg(test)]
mod http_tests {
    use super::EmailAllowList;
    use crate::routes::auth::tests::{Res, app, call};
    use crate::{ApiConfig, AppState, router};
    use axum::http::{Method, StatusCode};
    use ruum_core::identity::VerifiedLogin;
    use ruum_core::identity::memory::MemoryOidcClient;
    use serde_json::json;
    use std::sync::Arc;

    fn cookie_of(res: &Res, name: &str) -> String {
        res.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.strip_prefix(&format!("{name}=")))
            .map(|c| c.split(';').next().unwrap().to_string())
            .unwrap_or_default()
    }

    /// Runs one sign-in through the fake provider with this email and `email_verified`.
    async fn callback(
        app: &axum::Router,
        oidc: &MemoryOidcClient,
        code: &str,
        email: &str,
        verified: bool,
    ) -> Res {
        let start = call(app, Method::GET, "/v1/auth/login", &[], None).await;
        assert_eq!(start.status, StatusCode::FOUND);
        let location = start.headers["location"].to_str().unwrap().to_string();
        let state = url::Url::parse(&location)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let login_cookie = crate::routes::auth::tests::login_cookie(&start);
        oidc.add_login(
            code,
            VerifiedLogin {
                issuer: MemoryOidcClient::ISSUER.into(),
                subject: format!("sub-{email}"),
                email: Some(email.into()),
                email_verified: verified,
                name: Some("Mari".into()),
                id_token: None,
            },
        );
        call(
            app,
            Method::GET,
            &format!("/v1/auth/callback?code={code}&state={state}"),
            &[("cookie", &login_cookie)],
            None,
        )
        .await
    }

    fn closed(state: &AppState, verified: &str, unverified: Option<&str>) -> AppState {
        let list = EmailAllowList::from_settings(Some(verified), unverified).unwrap();
        AppState::new(
            state.services.clone(),
            ApiConfig {
                signup: Some(Arc::new(list)),
                ..ApiConfig::default()
            },
        )
    }

    #[tokio::test]
    async fn an_address_off_the_list_gets_no_user_and_no_session() {
        let (_, oidc, base) = app();
        let app = router(closed(&base, "mari@example.test", None));
        let res = callback(&app, &oidc, "c1", "stranger@example.test", true).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
        assert_eq!(res.body["error"]["code"], "unauthenticated");
        assert!(cookie_of(&res, "ruum_session").is_empty());
        let none = base
            .services
            .identity
            .find_verified_email("stranger@example.test")
            .await
            .unwrap();
        assert!(none.is_none(), "a refused sign-in must not create a user");
        // The listed address signs in.
        let ok = callback(&app, &oidc, "c2", "Mari@Example.test", true).await;
        assert_eq!(ok.status, StatusCode::FOUND);
        assert!(!cookie_of(&ok, "ruum_session").is_empty());
    }

    #[tokio::test]
    async fn an_unverified_email_needs_the_unverified_list() {
        let (_, oidc, base) = app();
        let app = router(closed(&base, "mari@example.test", Some("ms@example.test")));
        let res = callback(&app, &oidc, "c1", "mari@example.test", false).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
        let ok = callback(&app, &oidc, "c2", "ms@example.test", false).await;
        assert_eq!(ok.status, StatusCode::FOUND);
    }

    #[tokio::test]
    async fn an_open_server_still_lets_everyone_in() {
        let (app, oidc, _) = app();
        let ok = callback(&app, &oidc, "c1", "anyone@example.test", false).await;
        assert_eq!(ok.status, StatusCode::FOUND);
    }

    #[tokio::test]
    async fn only_listed_users_create_tenants() {
        let (_, oidc, base) = app();
        let open_for_mari = closed(&base, "mari@example.test", None);
        let app = router(open_for_mari);
        let res = callback(&app, &oidc, "c1", "mari@example.test", true).await;
        assert_eq!(res.status, StatusCode::FOUND);
        let session = cookie_of(&res, "ruum_session");
        let csrf = cookie_of(&res, "ruum_csrf");
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let headers = [("cookie", cookie.as_str()), ("ruum-csrf", csrf.as_str())];
        let made = call(
            &app,
            Method::POST,
            "/v1/tenants",
            &headers,
            Some(json!({"name": "Studio"})),
        )
        .await;
        assert_eq!(made.status, StatusCode::CREATED, "{:?}", made.body);
        // The operator takes the address off the list: the same session can no longer create.
        let app = router(closed(&base, "someone-else@example.test", None));
        let refused = call(
            &app,
            Method::POST,
            "/v1/tenants",
            &headers,
            Some(json!({"name": "Studio 2"})),
        )
        .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.body["error"]["code"], "permission_denied");
    }
}
