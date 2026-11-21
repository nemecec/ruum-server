//! The pure rules of the runtime API's core side: sealing, the topic rule, the subject cache.
//! The service runs end to end in `crates/ruum-runtime-api/tests/` (the T-6 world).

use super::*;

#[test]
fn a_sealed_subject_opens_only_with_its_key() {
    let tk = [3u8; 32];
    let app = NodeId(Uuid::now_v7());
    let user = Uuid::now_v7();
    let k = sealing_key(&tk, app, "user");
    let s = seal(&k, user);
    assert_ne!(s, user);
    assert_eq!(open(&k, s), user);
    // Another app, another tenant key and the agent key each give another value.
    for other in [
        sealing_key(&tk, NodeId(Uuid::now_v7()), "user"),
        sealing_key(&[4u8; 32], app, "user"),
        sealing_key(&tk, app, "agent"),
    ] {
        assert_ne!(seal(&other, user), s);
        assert_ne!(open(&other, s), user);
    }
    // The value has the UUID text form of the schema.
    assert!(Uuid::parse_str(&s.to_string()).is_ok());
}

#[test]
fn a_topic_is_declared_by_an_events_api() {
    let m = serde_json::json!({
        "apis": {
            "orders": { "protocol": "events", "visibility": "internal", "spec": "a.yaml", "scopes": ["read"] },
            "rest": { "protocol": "http", "visibility": "internal", "spec": "b.yaml", "scopes": ["read"] }
        }
    });
    assert!(topic_declared(&m, "orders"));
    assert!(topic_declared(&m, "orders.created"));
    for t in [
        "ordersx",
        "orders_created",
        "rest",
        "rest.created",
        "other.orders",
    ] {
        assert!(!topic_declared(&m, t), "{t}");
    }
    assert!(!topic_declared(&serde_json::json!({}), "orders"));
}

#[test]
fn the_subject_cache_is_bounded_per_tenant() {
    let mut s = Subjects::default();
    let now = Instant::now();
    let app = NodeId(Uuid::now_v7());
    let a = TenantId(Uuid::now_v7());
    let b = TenantId(Uuid::now_v7());
    s.put(b, app, "pairwise:b", Uuid::now_v7(), now);
    for i in 0..SUBJECTS_PER_TENANT + 10 {
        s.put(
            a,
            app,
            &format!("pairwise:{i}"),
            Uuid::now_v7(),
            now + Duration::from_millis(i as u64),
        );
    }
    assert_eq!(s.by_tenant[&a].len(), SUBJECTS_PER_TENANT);
    // Tenant b keeps its entry: a's flood evicts only a's own oldest entries.
    assert!(s.get(b, app, "pairwise:b", now).is_some());
    assert!(s.get(a, app, "pairwise:0", now).is_none());
    assert_eq!(s.total, SUBJECTS_PER_TENANT + 1);
    // An entry ends after its TTL.
    assert!(s.get(b, app, "pairwise:b", now + SUBJECT_TTL).is_none());
}
