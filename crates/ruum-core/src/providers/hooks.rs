//! The [`TypedHooks`] of the node stores over the provider registry (card P-3): which provider
//! takes which lifecycle hook, and the platform rules for a new typed node that a copy makes.

use super::{HookKind, ProviderRegistry};
use crate::actor::Context;
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::nodes::TypedHooks;
use crate::tenants::{MemberRole, TenantStore};
use async_trait::async_trait;
use std::sync::Arc;

/// The hooks of the registered providers.
pub struct RegistryHooks {
    registry: Arc<ProviderRegistry>,
    tenants: Arc<dyn TenantStore>,
}

impl RegistryHooks {
    pub fn new(registry: Arc<ProviderRegistry>, tenants: Arc<dyn TenantStore>) -> Self {
        Self { registry, tenants }
    }
}

/// `403 permission_denied` with the hint `guests_create_resources` (decision 0002-2): a guest,
/// or an agent of a guest, makes no typed node while the tenant policy is false.
pub fn guests_may_not_create(type_name: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            format!("guests may not create {type_name} nodes in this space"),
        )
        .with_hint("guests_create_resources"),
    )
}

#[async_trait]
impl TypedHooks for RegistryHooks {
    fn provider_for(&self, type_name: &str, hook: HookKind) -> Option<String> {
        if !self.registry.wants_hook(type_name, hook) {
            return None;
        }
        self.registry
            .provider_id_for_type(type_name)
            .map(str::to_string)
    }

    async fn check_new_typed(&self, ctx: &Context, type_name: &str) -> Result<()> {
        let role = match ctx.actor.effective_user() {
            Some(user) => self.tenants.role_of(ctx.tenant, user).await?,
            None => None,
        };
        if matches!(
            role,
            Some(MemberRole::Member | MemberRole::Admin | MemberRole::Owner)
        ) {
            return Ok(());
        }
        let tenant = self.tenants.find(ctx.tenant).await?;
        if tenant.policy.guests_create_resources {
            Ok(())
        } else {
            Err(guests_may_not_create(type_name))
        }
    }
}
