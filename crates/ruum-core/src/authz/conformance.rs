//! The conformance suite of [`Authz`]: one set of checks that every engine must pass (the
//! in-memory engine and SpiceDB). It holds every assertion of `contracts/authz/schema.test.yaml`.
//!
//! Object IDs get a prefix per run, so runs against the shared SpiceDB of the laptop stack do not
//! see each other's relationships.

use super::*;

const SCHEMA_TEST: &str = include_str!("../../../../contracts/authz/schema.test.yaml");

/// The relationships and assertions of `schema.test.yaml`.
pub struct SchemaTest {
    pub relationships: Vec<String>,
    pub assert_true: Vec<String>,
    pub assert_false: Vec<String>,
}

/// Reads `schema.test.yaml`. The file uses a small, fixed part of YAML: a `relationships: |-`
/// block with one tuple per line, and `assertTrue:` / `assertFalse:` lists.
pub fn schema_test() -> SchemaTest {
    let mut t = SchemaTest {
        relationships: Vec::new(),
        assert_true: Vec::new(),
        assert_false: Vec::new(),
    };
    #[derive(PartialEq)]
    enum Sec {
        None,
        Rels,
        True,
        False,
    }
    let mut sec = Sec::None;
    for line in SCHEMA_TEST.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with("relationships:") {
            sec = Sec::Rels;
            continue;
        }
        if l == "assertions:" {
            sec = Sec::None;
            continue;
        }
        if l == "assertTrue:" {
            sec = Sec::True;
            continue;
        }
        if l == "assertFalse:" {
            sec = Sec::False;
            continue;
        }
        if !line.starts_with(' ') {
            sec = Sec::None;
            continue;
        }
        match sec {
            Sec::Rels => t.relationships.push(l.to_string()),
            Sec::True => t.assert_true.push(l.trim_start_matches("- ").to_string()),
            Sec::False => t.assert_false.push(l.trim_start_matches("- ").to_string()),
            Sec::None => {}
        }
    }
    t
}

fn prefix_object(o: &ObjectRef, p: &str) -> ObjectRef {
    if o.object_id == "*" {
        o.clone()
    } else {
        ObjectRef::new(&o.object_type, format!("{p}_{}", o.object_id))
    }
}

fn prefix_subject(s: &SubjectRef, p: &str) -> SubjectRef {
    SubjectRef {
        object: prefix_object(&s.object, p),
        relation: s.relation.clone(),
    }
}

/// Parses a tuple of the test file and puts the run prefix on every object ID.
pub fn prefixed(tuple: &str, p: &str) -> Relationship {
    let r: Relationship = tuple.parse().expect("tuple of schema.test.yaml");
    Relationship {
        object: prefix_object(&r.object, p),
        relation: r.relation,
        subject: prefix_subject(&r.subject, p),
    }
}

/// Runs the whole suite against `authz`. `p` is a prefix unique to this run.
pub async fn run(authz: &dyn Authz, p: &str) {
    schema_assertions(authz, p).await;
    writes_are_idempotent(authz, p).await;
    bulk_matches_single(authz, p).await;
    invalid_writes_fail(authz, p).await;
    editors_can_share_follows_the_tuples(authz, p).await;
}

/// Every assertion of `schema.test.yaml`.
async fn schema_assertions(authz: &dyn Authz, p: &str) {
    let t = schema_test();
    assert!(t.relationships.len() > 30, "schema.test.yaml was not read");
    assert!(!t.assert_true.is_empty() && !t.assert_false.is_empty());
    let updates = t
        .relationships
        .iter()
        .map(|r| RelationshipUpdate::Touch(prefixed(r, p)))
        .collect();
    let token = authz.write(updates).await.expect("write the test tuples");
    assert!(!token.0.is_empty());
    let mut failures = Vec::new();
    for (list, want) in [(&t.assert_true, true), (&t.assert_false, false)] {
        for a in list {
            let r = prefixed(a, p);
            let got = authz
                .check(&r.subject, &r.relation, &r.object, Some(&token))
                .await
                .expect("check");
            if got != want {
                failures.push(format!("{a}: want {want}, got {got}"));
            }
        }
    }
    assert!(failures.is_empty(), "schema.test.yaml: {failures:#?}");
}

/// Touch twice and delete twice are both fine; read shows the tuples of one object.
async fn writes_are_idempotent(authz: &dyn Authz, p: &str) {
    let r = prefixed("node:idem#viewer@user:u1", p);
    let parent = prefixed("node:idem#parent@node:idem_parent", p);
    authz
        .write(vec![
            RelationshipUpdate::Touch(r.clone()),
            RelationshipUpdate::Touch(parent.clone()),
        ])
        .await
        .unwrap();
    let t = authz
        .write(vec![RelationshipUpdate::Touch(r.clone())])
        .await
        .unwrap();
    assert!(
        authz
            .check(&r.subject, "view", &r.object, Some(&t))
            .await
            .unwrap()
    );
    let mut read = authz.read(&r.object).await.unwrap();
    read.sort_by_key(|x| x.to_string());
    let mut want = vec![r.clone(), parent.clone()];
    want.sort_by_key(|x| x.to_string());
    assert_eq!(read, want);

    let t = authz
        .write(vec![RelationshipUpdate::Delete(r.clone())])
        .await
        .unwrap();
    authz
        .write(vec![RelationshipUpdate::Delete(r.clone())])
        .await
        .unwrap();
    assert!(
        !authz
            .check(&r.subject, "view", &r.object, Some(&t))
            .await
            .unwrap()
    );
    assert_eq!(authz.read(&r.object).await.unwrap(), vec![parent]);
}

async fn bulk_matches_single(authz: &dyn Authz, p: &str) {
    let t = schema_test();
    let user = prefix_subject(&"user:jaan".parse().unwrap(), p);
    let objects: Vec<ObjectRef> = [
        "plan",
        "garden",
        "projects",
        "secret",
        "secret_doc",
        "open_doc",
        "photo",
        "nope",
    ]
    .iter()
    .map(|n| prefix_object(&ObjectRef::new("node", n), p))
    .collect();
    // The test tuples are written by schema_assertions; write them again to be order-free.
    let updates = t
        .relationships
        .iter()
        .map(|r| RelationshipUpdate::Touch(prefixed(r, p)))
        .collect();
    let token = authz.write(updates).await.unwrap();
    for perm in ["view", "edit", "share", "manage"] {
        let bulk = authz.check_bulk(&user, perm, &objects).await.unwrap();
        assert_eq!(bulk.len(), objects.len());
        for (o, b) in objects.iter().zip(bulk) {
            let single = authz.check(&user, perm, o, Some(&token)).await.unwrap();
            assert_eq!(b, single, "{o}#{perm}");
        }
    }
    assert!(
        authz
            .check_bulk(&user, "view", &[])
            .await
            .unwrap()
            .is_empty()
    );
}

/// Relationships that the schema does not allow are refused, and nothing of the batch is written.
async fn invalid_writes_fail(authz: &dyn Authz, p: &str) {
    let good = prefixed("node:inv#viewer@user:u2", p);
    for bad in [
        "node:inv#sharer@user:u2",
        "node:inv#link_viewer@user:u2",
        "node:inv#manager@share_link:l1",
        "node:inv#editors_share_off@user:u2",
    ] {
        let r = prefixed(bad, p);
        let res = authz
            .write(vec![
                RelationshipUpdate::Touch(good.clone()),
                RelationshipUpdate::Touch(r),
            ])
            .await;
        assert!(res.is_err(), "{bad} must be refused");
    }
    assert!(authz.read(&good.object).await.unwrap().is_empty());
}

/// The `editors_can_share` tuples of [`super::tuples::editors_can_share`].
async fn editors_can_share_follows_the_tuples(authz: &dyn Authz, p: &str) {
    let ed = prefix_subject(&"user:ed".parse().unwrap(), p);
    let root = prefix_object(&ObjectRef::new("node", "ecs_root"), p);
    let child = prefix_object(&ObjectRef::new("node", "ecs_child"), p);
    let base = vec![
        RelationshipUpdate::Touch(prefixed("node:ecs_child#parent@node:ecs_root", p)),
        RelationshipUpdate::Touch(prefixed("node:ecs_root#editor@user:ed", p)),
    ];
    let t = authz.write(base).await.unwrap();
    assert!(authz.check(&ed, "share", &child, Some(&t)).await.unwrap());

    let set = |o: &ObjectRef, v: Option<bool>| {
        let off = Relationship {
            object: o.clone(),
            relation: "editors_share_off".into(),
            subject: SubjectRef::any_user(),
        };
        let on = Relationship {
            object: o.clone(),
            relation: "editors_share_on".into(),
            subject: SubjectRef::any_user(),
        };
        match v {
            Some(false) => vec![
                RelationshipUpdate::Touch(off),
                RelationshipUpdate::Delete(on),
            ],
            Some(true) => vec![
                RelationshipUpdate::Delete(off),
                RelationshipUpdate::Touch(on),
            ],
            None => vec![
                RelationshipUpdate::Delete(off),
                RelationshipUpdate::Delete(on),
            ],
        }
    };
    let t = authz.write(set(&root, Some(false))).await.unwrap();
    assert!(!authz.check(&ed, "share", &child, Some(&t)).await.unwrap());
    assert!(authz.check(&ed, "edit", &child, Some(&t)).await.unwrap());
    let t = authz.write(set(&child, Some(true))).await.unwrap();
    assert!(authz.check(&ed, "share", &child, Some(&t)).await.unwrap());
    assert!(!authz.check(&ed, "share", &root, Some(&t)).await.unwrap());
    let t = authz.write(set(&child, None)).await.unwrap();
    assert!(!authz.check(&ed, "share", &child, Some(&t)).await.unwrap());
    let t = authz.write(set(&root, None)).await.unwrap();
    assert!(authz.check(&ed, "share", &child, Some(&t)).await.unwrap());
}

#[tokio::test]
async fn memory_authz_conforms() {
    let authz = memory::MemoryAuthz::default();
    run(&authz, "m").await;
}

#[test]
fn the_test_file_is_read() {
    let t = schema_test();
    assert!(
        t.relationships
            .iter()
            .any(|r| r == "node:plan#link_viewer@share_link:link1")
    );
    assert!(
        t.assert_true
            .contains(&"node:plan#share@user:jaan".to_string())
    );
    assert!(
        t.assert_false
            .contains(&"node:acme_ext#manage@user:gert".to_string())
    );
}
