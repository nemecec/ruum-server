//! `ruum-server seed-dev <file>`: a dev-only seed for end-to-end checks on the laptop stack.
//!
//! It runs against the same services as the server (the same environment) and makes, with new
//! names on every run:
//!
//! - Alice, the owner of a new team tenant (tenant A), with an agent (`manage`, no subtree) and
//!   its API key, and a WebDAV app password;
//! - Bob, a guest of tenant A without any grant;
//! - Carol, a user with only her personal tenant (tenant B).
//!
//! The users sign in through a dev issuer, not Zitadel. It writes shell variables to `<file>`
//! (mode 0600): user access tokens (1 hour), the agent key, the app password and the IDs. The
//! server must use the same `RUUM_TOKEN_SIGNING_KEY`, or it refuses the user tokens.
//!
//! It refuses to run unless `RUUM_PUBLIC_URL` is a loopback or `.localhost` origin.

use anyhow::{Context as _, bail};
use ruum_core::Services;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::identity::{VerifiedLogin, issue_user_token};
use ruum_core::ids::NodeId;
use ruum_core::tenants::MemberRole;
use std::fmt::Write as _;

/// The issuer of the seed users.
const DEV_ISSUER: &str = "https://dev-seed.ruum.localhost";
/// How long the user tokens of the seed live.
const TOKEN_SECONDS: i64 = 3600;

/// Whether the public URL is a laptop origin.
fn is_dev_origin(public_url: &str) -> bool {
    let host = public_url
        .split("://")
        .nth(1)
        .unwrap_or(public_url)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    host == "localhost" || host == "127.0.0.1" || host.ends_with(".localhost")
}

pub async fn run(services: &Services, public_url: &str, out: &str) -> anyhow::Result<()> {
    if !is_dev_origin(public_url) {
        bail!("seed-dev runs only with a loopback or .localhost RUUM_PUBLIC_URL, not {public_url}");
    }
    let run = NodeId::new().0.simple().to_string();
    let run = &run[run.len() - 8..];
    let user = |name: &str| VerifiedLogin {
        issuer: DEV_ISSUER.into(),
        subject: format!("{name}-{run}"),
        email: Some(format!("{name}-{run}@example.test")),
        email_verified: true,
        name: Some(format!("{name} {run}")),
        id_token: None,
    };
    let alice = services.identity.upsert_from_login(&user("alice")).await?;
    let bob = services.identity.upsert_from_login(&user("bob")).await?;
    let carol = services.identity.upsert_from_login(&user("carol")).await?;

    let team = services
        .tenants
        .create_team(&Actor::user(alice.id), format!("E2E {run}"))
        .await
        .context("create the team tenant")?;
    let alice_ctx = Context {
        tenant: team.id,
        actor: Actor::user(alice.id),
        scope: None,
        idempotency_key: None,
    };
    let (_, invitation) = services
        .tenants
        .invite(&alice_ctx, bob.email.clone(), MemberRole::Guest, None)
        .await
        .context("invite Bob as a guest")?;
    services
        .tenants
        .accept_invitation(&Actor::user(bob.id), &invitation)
        .await
        .context("Bob accepts the invitation")?;

    let agent = services
        .agents
        .register(
            &alice_ctx,
            NewAgent {
                name: format!("E2E bot {run}"),
                description: Some("ruum-server seed-dev".into()),
                scope: AgentScope {
                    subtree: None,
                    max_role: Role::Manage,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .context("register the agent")?;
    let (_, agent_key) = services
        .agents
        .create_key(&alice_ctx, agent.id, Some("e2e".into()), None)
        .await
        .context("create the agent key")?;
    let (_, app_password) = services
        .identity
        .create_app_password(&alice_ctx, format!("e2e-{run}"), None)
        .await
        .context("create the app password")?;

    let tokens = services.tokens.as_ref();
    let mut env = String::new();
    let mut put = |k: &str, v: &str| {
        let _ = writeln!(env, "{k}='{v}'");
    };
    put("SEED_RUN", run);
    put("TENANT_A", &team.id.0.to_string());
    put("TENANT_B", &carol.home_tenant_id.0.to_string());
    put("ROOT_A", &team.root_node_id.0.to_string());
    put("ALICE_ID", &alice.id.0.to_string());
    put("ALICE_EMAIL", &alice.email);
    put(
        "ALICE_TOKEN",
        &issue_user_token(tokens, alice.id, TOKEN_SECONDS)?,
    );
    put("BOB_ID", &bob.id.0.to_string());
    put(
        "BOB_TOKEN",
        &issue_user_token(tokens, bob.id, TOKEN_SECONDS)?,
    );
    put("CAROL_ID", &carol.id.0.to_string());
    put(
        "CAROL_TOKEN",
        &issue_user_token(tokens, carol.id, TOKEN_SECONDS)?,
    );
    put("AGENT_ID", &agent.id.0.to_string());
    put("AGENT_KEY", &agent_key);
    put("APP_PASSWORD", &app_password);
    write_private(out, &env).with_context(|| format!("write {out}"))?;
    tracing::info!(
        run,
        file = out,
        "seed-dev: tenant A, Alice, Bob (guest), Carol, agent key"
    );
    Ok(())
}

/// Writes the file readable by its owner only.
fn write_private(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    o.open(path)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_laptop_origins_are_dev() {
        assert!(is_dev_origin("https://ruum.localhost"));
        assert!(is_dev_origin("http://127.0.0.1:8490"));
        assert!(is_dev_origin("http://localhost"));
        assert!(!is_dev_origin("https://ruum.app"));
        assert!(!is_dev_origin("https://localhost.evil.example"));
    }
}
