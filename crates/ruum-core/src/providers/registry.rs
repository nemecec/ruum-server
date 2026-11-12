//! The provider registry: which provider owns which node type.
//!
//! First-party providers are registered in code at start-up; external providers (step 6) will
//! come from the admin registration. The registry checks each descriptor against the rules of
//! the contract before it takes it:
//!
//! - the provider ID and each type name match the patterns of `ProviderDescriptor.id` and
//!   `TypeDescriptor.type`;
//! - a provider has at least one type, and no type belongs to two providers;
//! - the hooks `copied` and `purged` are required;
//! - each `child_types` entry is a type of the same provider (decision 0004-18);
//! - a first-party provider has `base_url: null`.

use super::{Feature, HookKind, Provider, ProviderDescriptor, TypeDescriptor, bad_request};
use crate::error::Result;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// The registered providers, by provider ID and by node type.
///
/// The registry is shared (`Arc`) by the node store's hooks, the delivery worker and the
/// reconciler, and areas built after it register their first-party provider into the same
/// registry ([`ProviderRegistry::register_shared`], card W-1: `ruum.webapp` after
/// `ruum.database`), so the rows sit behind a lock.
#[derive(Default)]
pub struct ProviderRegistry {
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    providers: BTreeMap<String, Registered>,
    /// node type -> provider ID
    types: BTreeMap<String, String>,
}

struct Registered {
    provider: Arc<dyn Provider>,
    descriptor: ProviderDescriptor,
}

impl Inner {
    fn add(&mut self, provider: Arc<dyn Provider>, replace: bool) -> Result<()> {
        let descriptor = provider.descriptor();
        check_descriptor(&descriptor)?;
        if descriptor.base_url.is_some() {
            return Err(bad_request(format!(
                "provider {}: a provider in the server process has base_url null",
                descriptor.id
            )));
        }
        if self.providers.contains_key(&descriptor.id) && !replace {
            return Err(bad_request(format!(
                "provider {} is already registered",
                descriptor.id
            )));
        }
        for t in &descriptor.types {
            if let Some(owner) = self.types.get(&t.type_name)
                && *owner != descriptor.id
            {
                return Err(bad_request(format!(
                    "type {} already belongs to provider {owner}",
                    t.type_name
                )));
            }
        }
        if let Some(old) = self.providers.remove(&descriptor.id) {
            for t in &old.descriptor.types {
                self.types.remove(&t.type_name);
            }
        }
        for t in &descriptor.types {
            self.types
                .insert(t.type_name.clone(), descriptor.id.clone());
        }
        self.providers.insert(
            descriptor.id.clone(),
            Registered {
                provider,
                descriptor,
            },
        );
        Ok(())
    }

    fn type_descriptor(&self, type_name: &str) -> Option<&TypeDescriptor> {
        let id = self.types.get(type_name)?;
        self.providers
            .get(id)?
            .descriptor
            .type_descriptor(type_name)
    }

    fn registered_for(&self, type_name: &str) -> Option<&Registered> {
        self.types
            .get(type_name)
            .and_then(|id| self.providers.get(id))
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a first-party provider (in the server process). Fails with `bad_request` when
    /// the descriptor breaks a rule of the contract, the provider ID is taken, or a type
    /// belongs to another provider. Nothing changes on failure.
    pub fn register(&mut self, provider: Arc<dyn Provider>) -> Result<()> {
        self.inner
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .add(provider, false)
    }

    /// Registers a first-party provider into a registry that is already shared, under the
    /// rules of [`ProviderRegistry::register`], except that a provider with the same ID is
    /// replaced (an area installed again over new stores). Nothing changes on failure.
    pub fn register_shared(&self, provider: Arc<dyn Provider>) -> Result<()> {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .add(provider, true)
    }

    /// The provider with this ID.
    pub fn provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.read().providers.get(id).map(|r| r.provider.clone())
    }

    /// The descriptor of the provider with this ID, as it was registered.
    pub fn descriptor(&self, id: &str) -> Option<ProviderDescriptor> {
        self.read().providers.get(id).map(|r| r.descriptor.clone())
    }

    /// The provider that owns a node type.
    pub fn for_type(&self, type_name: &str) -> Option<Arc<dyn Provider>> {
        self.read()
            .registered_for(type_name)
            .map(|r| r.provider.clone())
    }

    /// The ID of the provider that owns a node type.
    pub fn provider_id_for_type(&self, type_name: &str) -> Option<String> {
        self.read().types.get(type_name).cloned()
    }

    /// The type descriptor of a node type.
    pub fn type_descriptor(&self, type_name: &str) -> Option<TypeDescriptor> {
        self.read().type_descriptor(type_name).cloned()
    }

    /// The only child types a node of this type may have. Empty for an unknown type.
    pub fn child_types(&self, type_name: &str) -> Vec<String> {
        self.read()
            .type_descriptor(type_name)
            .map(|t| t.child_types.clone())
            .unwrap_or_default()
    }

    /// True when a node of type `child` may be a child of a node of type `parent`.
    pub fn allows_child(&self, parent: &str, child: &str) -> bool {
        self.read()
            .type_descriptor(parent)
            .is_some_and(|t| t.child_types.iter().any(|c| c == child))
    }

    /// True when the provider of this type wants the hook. The platform sends no delivery of
    /// a hook the provider did not ask for.
    pub fn wants_hook(&self, type_name: &str, hook: HookKind) -> bool {
        self.read()
            .registered_for(type_name)
            .is_some_and(|r| r.descriptor.wants_hook(hook))
    }

    /// True when the provider of this type has the feature.
    pub fn has_feature(&self, type_name: &str, feature: Feature) -> bool {
        self.read()
            .registered_for(type_name)
            .is_some_and(|r| r.descriptor.has_feature(feature))
    }

    /// The registered provider IDs, sorted.
    pub fn provider_ids(&self) -> std::vec::IntoIter<String> {
        self.read()
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The registered node types, sorted.
    pub fn type_names(&self) -> std::vec::IntoIter<String> {
        self.read()
            .types
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// `^[a-z][a-z0-9_.-]{1,62}$` (`ProviderDescriptor.id`).
pub fn is_provider_id(s: &str) -> bool {
    matches_pattern(s, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
    })
}

/// `^[a-z][a-z0-9_.]{1,62}$` (`TypeDescriptor.type`).
pub fn is_type_name(s: &str) -> bool {
    matches_pattern(s, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.')
    })
}

fn matches_pattern(s: &str, rest: impl Fn(char) -> bool) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let len = s.chars().count();
    first.is_ascii_lowercase() && (2..=63).contains(&len) && chars.all(rest)
}

/// Checks a descriptor against the rules of the contract (see the module docs).
pub fn check_descriptor(d: &ProviderDescriptor) -> Result<()> {
    if !is_provider_id(&d.id) {
        return Err(bad_request(format!(
            "provider id {:?} does not match ^[a-z][a-z0-9_.-]{{1,62}}$",
            d.id
        )));
    }
    if d.name.trim().is_empty() {
        return Err(bad_request(format!("provider {}: the name is empty", d.id)));
    }
    if d.types.is_empty() {
        return Err(bad_request(format!("provider {}: no types", d.id)));
    }
    for (i, t) in d.types.iter().enumerate() {
        if !is_type_name(&t.type_name) {
            return Err(bad_request(format!(
                "provider {}: type {:?} does not match ^[a-z][a-z0-9_.]{{1,62}}$",
                d.id, t.type_name
            )));
        }
        if d.types[..i].iter().any(|o| o.type_name == t.type_name) {
            return Err(bad_request(format!(
                "provider {}: type {} is listed twice",
                d.id, t.type_name
            )));
        }
        if t.manifest_schema.trim().is_empty() {
            return Err(bad_request(format!(
                "provider {}: type {} has no manifest schema",
                d.id, t.type_name
            )));
        }
    }
    for t in &d.types {
        for c in &t.child_types {
            if d.type_descriptor(c).is_none() {
                return Err(bad_request(format!(
                    "provider {}: child type {c} of {} is not a type of this provider",
                    d.id, t.type_name
                )));
            }
        }
    }
    for h in HookKind::REQUIRED {
        if !d.hooks.contains(h) {
            return Err(bad_request(format!(
                "provider {}: the hook {} is required",
                d.id,
                serde_json::to_value(h).unwrap_or_default()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::memory::{MemoryPlatform, MemoryProvider};

    fn memory() -> Arc<MemoryProvider> {
        Arc::new(MemoryProvider::new(Arc::new(MemoryPlatform::new())))
    }

    #[test]
    fn patterns() {
        assert!(is_provider_id("ruum.database"));
        assert!(is_provider_id("kadakas"));
        assert!(is_provider_id("a-b"));
        assert!(!is_provider_id("a"));
        assert!(!is_provider_id("Ruum"));
        assert!(!is_provider_id("1ruum"));
        assert!(!is_provider_id(&format!("a{}", "b".repeat(63))));
        assert!(is_provider_id(&format!("a{}", "b".repeat(62))));
        assert!(is_type_name("kadakas.drawing"));
        assert!(!is_type_name("a-b"));
        assert!(!is_type_name(""));
    }

    #[test]
    fn registers_and_finds_by_type() {
        let mut r = ProviderRegistry::new();
        r.register(memory()).unwrap();
        let id = MemoryProvider::ID;
        assert!(r.provider(id).is_some());
        assert_eq!(
            r.provider_id_for_type(MemoryProvider::TYPE).as_deref(),
            Some(id)
        );
        assert!(r.for_type(MemoryProvider::TYPE).is_some());
        assert!(r.for_type("database").is_none());
        assert!(r.allows_child(MemoryProvider::TYPE, MemoryProvider::CHILD_TYPE));
        assert!(!r.allows_child(MemoryProvider::CHILD_TYPE, MemoryProvider::TYPE));
        assert!(!r.allows_child("unknown", MemoryProvider::TYPE));
        assert!(r.wants_hook(MemoryProvider::TYPE, HookKind::Copied));
        assert!(!r.wants_hook("unknown", HookKind::Copied));
        assert!(r.has_feature(MemoryProvider::TYPE, Feature::Export));
        assert_eq!(r.provider_ids().collect::<Vec<_>>(), vec![id]);
        assert_eq!(r.type_names().count(), 2);
    }

    #[test]
    fn refuses_a_second_registration_and_a_taken_type() {
        let mut r = ProviderRegistry::new();
        r.register(memory()).unwrap();
        assert!(r.register(memory()).is_err());

        let mut other = memory().descriptor();
        other.id = "other.provider".into();
        let err = check_and_register(&mut r, other).unwrap_err();
        assert!(err.to_string().contains("already belongs"), "{err}");
    }

    /// A shared registry takes a provider later; the same ID replaces, a taken type does not.
    #[test]
    fn registers_into_a_shared_registry() {
        let r = Arc::new(ProviderRegistry::new());
        r.register_shared(memory()).unwrap();
        r.register_shared(memory()).unwrap();
        assert_eq!(r.provider_ids().count(), 1);
        assert!(r.for_type(MemoryProvider::TYPE).is_some());
        let mut other = memory().descriptor();
        other.id = "other.provider".into();
        let p = MemoryProvider::with_descriptor(Arc::new(MemoryPlatform::new()), other);
        let err = r.register_shared(Arc::new(p)).unwrap_err();
        assert!(err.to_string().contains("already belongs"), "{err}");
        assert_eq!(r.provider_ids().count(), 1);
    }

    #[test]
    fn refuses_broken_descriptors() {
        let base = memory().descriptor();
        type Change = Box<dyn Fn(&mut ProviderDescriptor)>;
        let cases: Vec<(&str, Change)> = vec![
            ("bad id", Box::new(|d| d.id = "Bad".into())),
            ("empty name", Box::new(|d| d.name = " ".into())),
            ("no types", Box::new(|d| d.types.clear())),
            (
                "bad type",
                Box::new(|d| d.types[0].type_name = "a-b".into()),
            ),
            (
                "twice",
                Box::new(|d| {
                    let t = d.types[0].clone();
                    d.types.push(t)
                }),
            ),
            (
                "no schema",
                Box::new(|d| d.types[0].manifest_schema = String::new()),
            ),
            (
                "foreign child",
                Box::new(|d| d.types[0].child_types = vec!["database".into()]),
            ),
            (
                "no copied",
                Box::new(|d| d.hooks.retain(|h| *h != HookKind::Copied)),
            ),
            (
                "no purged",
                Box::new(|d| d.hooks.retain(|h| *h != HookKind::Purged)),
            ),
            (
                "base url",
                Box::new(|d| d.base_url = Some("https://p.example".into())),
            ),
        ];
        for (name, change) in cases {
            let mut d = base.clone();
            change(&mut d);
            let mut r = ProviderRegistry::new();
            assert!(check_and_register(&mut r, d).is_err(), "{name}");
            assert_eq!(r.provider_ids().count(), 0, "{name}: nothing registered");
            assert_eq!(r.type_names().count(), 0, "{name}: no type registered");
        }
    }

    /// Registers a memory provider that answers with the given descriptor.
    fn check_and_register(r: &mut ProviderRegistry, d: ProviderDescriptor) -> Result<()> {
        let p = MemoryProvider::with_descriptor(Arc::new(MemoryPlatform::new()), d);
        r.register(Arc::new(p))
    }
}
