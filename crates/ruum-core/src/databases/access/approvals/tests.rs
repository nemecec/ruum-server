//! The pure rules (the link, the hashes, `planned_columns`, the take), one conformance suite of
//! the store on memory and Postgres (with the concurrency cases of backlog 2026-11-08 T-3), the
//! service rules on the in-memory services, and the Postgres tenant filter (`#[sqlx::test]`).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{Agent, AgentScope, Budget, NewAgent};
use crate::authz::memory::MemoryAuthz;
use crate::databases::CreateDatabase;
use crate::databases::access::grants::{GrantStore, PgGrantStore};
use crate::databases::tests::{World, ctx, settled, world};
use crate::journal::EventType;
use crate::nodes::OnConflict;
use crate::privacy::PseudonymKey;
use crate::providers::ProviderState;
use crate::sharing::Grantee;

fn code<T: std::fmt::Debug>(r: Result<T>) -> (u16, String) {
    match r {
        Err(Error::Api(e)) => (e.code.http_status(), e.code.as_str().to_string()),
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn agent_actor_ctx(tenant: TenantId, agent: AgentId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.0.into(),
            },
            on_behalf_of: Some(user),
        },
        scope: None,
        idempotency_key: None,
    }
}

// ---------------------------------------------------------------------------- pure rules

#[test]
fn review_url_holds_ids_only() {
    let (t, n, a) = (TenantId::new(), NodeId::new(), Uuid::now_v7());
    assert_eq!(
        review_url("https://ruum.localhost/", t, n, a),
        format!("https://ruum.localhost/t/{t}/databases/{n}/access?request={a}")
    );
}

#[test]
fn hashes_bind_the_statement_the_parameters_the_database_and_the_key() {
    let k = PseudonymKey::from_bytes([7; 32]);
    let n = NodeId::new();
    let h = statement_hash(&k, n, "delete from t", &[json!("a")]);
    assert_eq!(h, statement_hash(&k, n, "delete from t", &[json!("a")]));
    assert_ne!(h, statement_hash(&k, n, "delete from t", &[json!("b")]));
    assert_ne!(h, statement_hash(&k, n, "delete from t", &[]));
    assert_ne!(h, statement_hash(&k, n, "delete from u", &[json!("a")]));
    assert_ne!(
        h,
        statement_hash(&k, NodeId::new(), "delete from t", &[json!("a")])
    );
    assert_ne!(
        h,
        statement_hash(
            &PseudonymKey::from_bytes([8; 32]),
            n,
            "delete from t",
            &[json!("a")]
        )
    );
    // A keyed hash: not the plain BLAKE3 of the text.
    assert_ne!(&h, blake3::hash(b"delete from t").as_bytes());
    let m = migration_hash(&k, n, "0002_a", "create table a (id int)");
    assert_ne!(
        m,
        migration_hash(&k, n, "0002_b", "create table a (id int)")
    );
    assert_ne!(
        m,
        migration_hash(&k, n, "0002_a", "create table b (id int)")
    );
    assert_eq!(
        grant_hash(GrantKind::ReadPersonal, 60),
        grant_hash(GrantKind::ReadPersonal, 60)
    );
    assert_ne!(
        grant_hash(GrantKind::ReadPersonal, 60),
        grant_hash(GrantKind::WriteLive, 60)
    );
}

#[test]
fn migration_names_follow_the_contract_pattern() {
    for ok in ["0002_add_beds", "0001_a", "9999_x_1_y"] {
        assert!(valid_migration_name(ok), "{ok}");
    }
    for bad in [
        "002_a", "0002_", "0002-a", "0002_A", "abcd_x", "0002_a b", "",
    ] {
        assert!(!valid_migration_name(bad), "{bad}");
    }
}

fn cols(v: &[PlannedColumn]) -> Vec<(String, ColumnChange, Option<String>)> {
    v.iter()
        .map(|c| {
            (
                format!("{}.{}.{}", c.schema, c.table, c.column),
                c.change,
                c.data_type.as_ref().map(|t| t.to_lowercase()),
            )
        })
        .collect()
}

#[test]
fn planned_columns_of_a_postgres_migration() {
    let classes = HashMap::from([
        ("public.beds.owner".to_string(), DataClass::Personal),
        ("public.plants.remarks".to_string(), DataClass::Normal),
    ]);
    let up = "create table beds (id integer primary key, owner text); \
              alter table plants add column notes text; \
              alter table plants rename column notes to remarks; \
              alter table plants alter column height type numeric(5,2); \
              create index plants_name on plants (name); \
              insert into beds (id, owner) values (1, 'x'); \
              alter table \"Plants\" add column \"Big\" text";
    let p = planned_columns(EngineKind::Postgres, up, &classes).unwrap();
    assert_eq!(
        cols(&p),
        vec![
            (
                "public.beds.id".into(),
                ColumnChange::Added,
                Some("integer".into())
            ),
            (
                "public.beds.owner".into(),
                ColumnChange::Added,
                Some("text".into())
            ),
            (
                "public.plants.notes".into(),
                ColumnChange::Added,
                Some("text".into())
            ),
            ("public.plants.remarks".into(), ColumnChange::Renamed, None),
            (
                "public.plants.height".into(),
                ColumnChange::Retyped,
                Some("numeric(5,2)".into())
            ),
            (
                "public.Plants.Big".into(),
                ColumnChange::Added,
                Some("text".into())
            ),
        ]
    );
    assert_eq!(p[1].suggested_class, Some(DataClass::Personal));
    assert_eq!(p[3].suggested_class, Some(DataClass::Normal));
    assert_eq!(p[0].suggested_class, None);
    assert!(p.iter().all(|c| c.decided_class.is_none()));
    // A statement that makes no column gives none.
    assert!(
        planned_columns(
            EngineKind::Postgres,
            "create index i on plants (name); delete from plants where id = 1",
            &HashMap::new()
        )
        .unwrap()
        .is_empty()
    );
    // An `up` that does not parse is refused; nothing runs.
    assert_eq!(
        code(planned_columns(
            EngineKind::Postgres,
            "create tabel x (",
            &HashMap::new()
        ))
        .1,
        "statement_not_allowed"
    );
}

#[test]
fn planned_columns_of_a_sqlite_migration() {
    let classes = HashMap::from([("main.beds.owner".to_string(), DataClass::Secret)]);
    let up = "create table beds (id integer primary key, owner text); \
              alter table plants add column notes text; \
              alter table plants rename column notes to remarks; \
              create index i on plants (latin_name)";
    let p = planned_columns(EngineKind::Sqlite, up, &classes).unwrap();
    assert_eq!(
        cols(&p),
        vec![
            (
                "main.beds.id".into(),
                ColumnChange::Added,
                Some("integer".into())
            ),
            (
                "main.beds.owner".into(),
                ColumnChange::Added,
                Some("text".into())
            ),
            (
                "main.plants.notes".into(),
                ColumnChange::Added,
                Some("text".into())
            ),
            ("main.plants.remarks".into(), ColumnChange::Renamed, None),
        ]
    );
    assert_eq!(p[1].suggested_class, Some(DataClass::Secret));
    assert!(
        planned_columns(
            EngineKind::Sqlite,
            "create index j on plants (latin_name); update plants set a = 1 where id = 2",
            &HashMap::new()
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn classes_of_a_decision_go_to_the_planned_columns_they_name() {
    let mut p = planned_columns(
        EngineKind::Postgres,
        "alter table plants add column notes text",
        &HashMap::new(),
    )
    .unwrap();
    let n = apply_classes(
        &mut p,
        &[
            DecidedClass {
                schema: "public".into(),
                table: "plants".into(),
                column: "notes".into(),
                class: DataClass::Normal,
            },
            DecidedClass {
                schema: "public".into(),
                table: "plants".into(),
                column: "other".into(),
                class: DataClass::Secret,
            },
        ],
    );
    assert_eq!(n, 1);
    assert_eq!(p[0].decided_class, Some(DataClass::Normal));
    // The contract's view does not show the decided class.
    assert!(p[0].view().get("decided_class").is_none());
}

fn sample(
    subject: ApprovalSubject,
    status: ApprovalStatus,
    at: OffsetDateTime,
) -> DatabaseApproval {
    let agent = AgentId::new();
    DatabaseApproval {
        id: Uuid::now_v7(),
        tenant_id: TenantId::new(),
        node_id: NodeId::new(),
        subject,
        status,
        migration_name: None,
        statement: Some("delete from t".into()),
        planned_columns: Vec::new(),
        grant_kind: None,
        grant_minutes: None,
        grant_reason: None,
        grant_id: None,
        statement_hash: [1; 32],
        params: vec![json!({ "$ruum": "hidden", "class": "personal" })],
        reasons: vec![ApprovalReason::DeleteWithoutWhere],
        estimated_rows: Some(2),
        requested_by: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.0.into(),
            },
            on_behalf_of: Some(UserId::new()),
        },
        decided_by: None,
        decided_by_display_name: None,
        decided_at: None,
        comment: None,
        expires_at: at + time::Duration::minutes(USE_WITHIN_MINUTES),
        used_at: None,
        created_at: at,
    }
}

#[test]
fn the_take_rules() {
    let now = crate::now();
    let a = sample(ApprovalSubject::Statement, ApprovalStatus::Approved, now);
    let agent = a.agent().unwrap();
    let take = |hash: [u8; 32]| Take {
        agent,
        subject: ApprovalSubject::Statement,
        statement_hash: hash,
    };
    assert!(check_take(&a, &take([1; 32]), now).is_ok());
    // Another statement or other parameters.
    assert_eq!(code(check_take(&a, &take([2; 32]), now)).0, 409);
    // Another agent: 404, it learns nothing of the request.
    let other = Take {
        agent: AgentId::new(),
        ..take([1; 32])
    };
    assert_eq!(code(check_take(&a, &other, now)).0, 404);
    // Another subject.
    let mig = Take {
        subject: ApprovalSubject::Migration,
        ..take([1; 32])
    };
    assert_eq!(code(check_take(&a, &mig, now)).0, 409);
    // Older than 15 minutes after the decision.
    assert_eq!(
        code(check_take(
            &a,
            &take([1; 32]),
            now + time::Duration::minutes(USE_WITHIN_MINUTES)
        ))
        .0,
        409
    );
    // Used, pending, denied.
    for s in [
        ApprovalStatus::Used,
        ApprovalStatus::Pending,
        ApprovalStatus::Denied,
        ApprovalStatus::Expired,
    ] {
        let mut b = a.clone();
        b.status = s;
        assert_eq!(code(check_take(&b, &take([1; 32]), now)).0, 409, "{s:?}");
    }
}

#[test]
fn status_reads_expired_past_its_time_except_an_approved_grant() {
    let now = crate::now();
    let later = now + time::Duration::hours(25);
    let p = sample(ApprovalSubject::Statement, ApprovalStatus::Pending, now);
    assert_eq!(p.status_at(now), ApprovalStatus::Pending);
    assert_eq!(p.status_at(later), ApprovalStatus::Expired);
    let g = sample(ApprovalSubject::Grant, ApprovalStatus::Approved, now);
    assert_eq!(g.status_at(later), ApprovalStatus::Approved);
    let d = sample(ApprovalSubject::Statement, ApprovalStatus::Denied, now);
    assert_eq!(d.status_at(later), ApprovalStatus::Denied);
}

#[test]
fn the_view_shows_markers_and_no_hash() {
    let now = crate::now();
    let mut a = sample(ApprovalSubject::Statement, ApprovalStatus::Pending, now);
    // Even a raw value that reached the row is shown as a marker.
    a.params = vec![json!("alice@example.test"), Value::Null];
    let v = a.view("https://ruum.localhost", now);
    assert_eq!(
        v["params"],
        json!([{ "$ruum": "hidden", "class": "personal" }, null])
    );
    let text = v.to_string();
    assert!(!text.contains("alice@example.test"));
    assert!(v.get("statement_hash").is_none());
    assert!(
        v["review_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("?request={}", a.id))
    );
    assert_eq!(v["grant_request"], Value::Null);
    assert_eq!(v["status"], "pending");
}

// ---------------------------------------------------------------------------- conformance

/// What the conformance suite works on: a ready database `node` of `tenant`, its user `user`
/// (who may give grants), an agent of that user, and another tenant.
struct Setup {
    tenant: TenantId,
    user: UserId,
    node: NodeId,
    agent: AgentId,
    other: TenantId,
}

fn new_request(s: &Setup, subject: ApprovalSubject, hash: u8) -> NewApproval {
    NewApproval {
        node: s.node,
        subject,
        migration_name: (subject == ApprovalSubject::Migration).then(|| "0002_add_beds".into()),
        statement: (subject != ApprovalSubject::Grant).then(|| "delete from t".into()),
        planned_columns: Vec::new(),
        grant_kind: None,
        grant_minutes: None,
        grant_reason: None,
        statement_hash: [hash; 32],
        params: QueryLogEntry::hide_params(&[json!("v")]),
        reasons: vec![match subject {
            ApprovalSubject::Migration => ApprovalReason::Migration,
            _ => ApprovalReason::DeleteWithoutWhere,
        }],
        estimated_rows: Some(2),
    }
}

fn grant_request(s: &Setup, kind: GrantKind, minutes: i32) -> NewApproval {
    NewApproval {
        statement: None,
        params: Vec::new(),
        grant_kind: Some(kind),
        grant_minutes: Some(minutes),
        grant_reason: Some("conformance".into()),
        statement_hash: grant_hash(kind, i64::from(minutes)),
        reasons: vec![ApprovalReason::Grant],
        estimated_rows: None,
        ..new_request(s, ApprovalSubject::Grant, 0)
    }
}

fn approve() -> Decision {
    Decision {
        approve: true,
        comment: Some("ok".into()),
        classes: Vec::new(),
        grant: None,
    }
}

fn the_grant(s: &Setup, kind: GrantKind, at: OffsetDateTime) -> NewGrant {
    NewGrant {
        node: s.node,
        kind,
        agent: s.agent,
        reason: Some("conformance".into()),
        starts_at: at,
        expires_at: at + time::Duration::minutes(60),
        restore_point: None,
    }
}

/// One conformance suite for [`MemoryApprovalStore`] and [`PgApprovalStore`]. `grants` is the
/// grant store that the approval store writes the grants of approved grant requests to.
async fn conformance(
    store: &dyn ApprovalStore,
    grants: &dyn GrantStore,
    authz: &dyn Authz,
    s: Setup,
) {
    let agent = agent_actor_ctx(s.tenant, s.agent, s.user);
    let user = ctx(s.tenant, s.user);
    let t0 = micros(crate::now());

    // A grant request; the same request again is the same row; another kind is another row.
    let (g1, created) = store
        .request(&agent, grant_request(&s, GrantKind::ReadPersonal, 60), t0)
        .await
        .unwrap();
    assert!(created);
    assert_eq!(g1.status_at(t0), ApprovalStatus::Pending);
    assert_eq!(g1.subject, ApprovalSubject::Grant);
    assert_eq!(g1.requested_by, agent.actor);
    assert_eq!(g1.expires_at, t0 + time::Duration::hours(PENDING_HOURS));
    let (again, created) = store
        .request(&agent, grant_request(&s, GrantKind::ReadPersonal, 30), t0)
        .await
        .unwrap();
    assert!(!created, "one pending request per agent, database and kind");
    assert_eq!(again.id, g1.id);
    assert_eq!(
        store.get(s.tenant, s.node, g1.id).await.unwrap(),
        Some(g1.clone())
    );

    // Two racing grant requests of one agent for one database and kind make one row.
    let (r1, r2) = tokio::join!(
        store.request(&agent, grant_request(&s, GrantKind::WriteLive, 60), t0),
        store.request(&agent, grant_request(&s, GrantKind::WriteLive, 60), t0),
    );
    let (r1, r2) = (r1.unwrap(), r2.unwrap());
    assert_eq!(r1.0.id, r2.0.id);
    assert!(
        r1.1 ^ r2.1,
        "exactly one of two racing requests made the row"
    );
    let write_live = store
        .list(
            s.tenant,
            ApprovalFilter {
                node: Some(s.node),
                requested_by: Some(s.agent),
                status: Some(ApprovalStatus::Pending),
            },
            PageRequest::default(),
            t0,
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|a| a.grant_kind == Some(GrantKind::WriteLive))
        .count();
    assert_eq!(write_live, 1);

    // The decision of a grant request writes the grant in its transaction. A decision that the
    // row refuses (a comment over 500 characters breaks a constraint of the row) writes no
    // grant and leaves the request pending.
    let refused = store
        .decide(
            &user,
            authz,
            s.node,
            g1.id,
            Decision {
                comment: Some("x".repeat(501)),
                grant: Some(the_grant(&s, GrantKind::ReadPersonal, t0)),
                ..approve()
            },
            t0,
        )
        .await;
    assert!(
        refused.is_err(),
        "the row refuses a comment over 500 characters"
    );
    let live = grants
        .live(s.tenant, s.node, s.agent, GrantKind::ReadPersonal, t0)
        .await
        .unwrap();
    assert!(live.is_none(), "a refused decision wrote a grant: {live:?}");
    assert_eq!(
        store
            .get(s.tenant, s.node, g1.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Pending
    );
    let w = store
        .decide(
            &user,
            authz,
            s.node,
            g1.id,
            Decision {
                grant: Some(the_grant(&s, GrantKind::ReadPersonal, t0)),
                ..approve()
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(w.value.status, ApprovalStatus::Approved);
    assert_eq!(w.value.decided_by, Some(s.user));
    assert_eq!(w.value.comment.as_deref(), Some("ok"));
    let grant_id = w.value.grant_id.expect("the approval names its grant");
    let g = grants
        .get(s.tenant, s.node, grant_id)
        .await
        .unwrap()
        .expect("the grant exists");
    assert_eq!(
        (g.kind, g.agent_id, g.granted_by),
        (GrantKind::ReadPersonal, s.agent, s.user)
    );
    // An approved grant request is final: it does not expire.
    assert_eq!(
        w.value.status_at(t0 + time::Duration::days(2)),
        ApprovalStatus::Approved
    );
    // A second decision: 409.
    assert_eq!(
        code(
            store
                .decide(&user, authz, s.node, g1.id, approve(), t0)
                .await
        )
        .0,
        409
    );

    // A statement: approve, then take once with the same hash, by the same agent.
    let (st, _) = store
        .request(&agent, new_request(&s, ApprovalSubject::Statement, 1), t0)
        .await
        .unwrap();
    assert_eq!(st.params, QueryLogEntry::hide_params(&[json!("v")]));
    let take = |hash: u8, agent: AgentId, subject| Take {
        agent,
        subject,
        statement_hash: [hash; 32],
    };
    // Not decided yet.
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    st.id,
                    &take(1, s.agent, ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        409
    );
    let d = store
        .decide(&user, authz, s.node, st.id, approve(), t0)
        .await
        .unwrap();
    assert_eq!(
        d.value.expires_at,
        t0 + time::Duration::minutes(USE_WITHIN_MINUTES)
    );
    assert_eq!(d.value.grant_id, None);
    // Another statement or other parameters; another agent; another subject.
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    st.id,
                    &take(2, s.agent, ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        409
    );
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    st.id,
                    &take(1, AgentId::new(), ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        404
    );
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    st.id,
                    &take(1, s.agent, ApprovalSubject::Migration),
                    t0
                )
                .await
        )
        .0,
        409
    );
    // Another tenant does not find it.
    assert_eq!(
        code(
            store
                .take(
                    s.other,
                    s.node,
                    st.id,
                    &take(1, s.agent, ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        404
    );
    let used = store
        .take(
            s.tenant,
            s.node,
            st.id,
            &take(1, s.agent, ApprovalSubject::Statement),
            t0,
        )
        .await
        .unwrap();
    assert_eq!(used.status, ApprovalStatus::Used);
    assert_eq!(used.used_at, Some(t0));
    // A used approval is spent.
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    st.id,
                    &take(1, s.agent, ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        409
    );

    // Two racing takes of one approval: exactly one succeeds.
    let (race, _) = store
        .request(&agent, new_request(&s, ApprovalSubject::Statement, 3), t0)
        .await
        .unwrap();
    store
        .decide(&user, authz, s.node, race.id, approve(), t0)
        .await
        .unwrap();
    let tk = take(3, s.agent, ApprovalSubject::Statement);
    let (a, b) = tokio::join!(
        store.take(s.tenant, s.node, race.id, &tk, t0),
        store.take(s.tenant, s.node, race.id, &tk, t0),
    );
    assert!(
        a.is_ok() ^ b.is_ok(),
        "exactly one of two racing takes: {a:?} {b:?}"
    );

    // Older than 15 minutes after the decision: refused, and it reads as expired.
    let (late, _) = store
        .request(&agent, new_request(&s, ApprovalSubject::Migration, 4), t0)
        .await
        .unwrap();
    store
        .decide(&user, authz, s.node, late.id, approve(), t0)
        .await
        .unwrap();
    let t16 = t0 + time::Duration::minutes(USE_WITHIN_MINUTES + 1);
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    late.id,
                    &take(4, s.agent, ApprovalSubject::Migration),
                    t16
                )
                .await
        )
        .0,
        409
    );
    let expired = store
        .list(
            s.tenant,
            ApprovalFilter {
                status: Some(ApprovalStatus::Expired),
                ..ApprovalFilter::default()
            },
            PageRequest::default(),
            t16,
        )
        .await
        .unwrap();
    assert!(expired.items.iter().any(|a| a.id == late.id));

    // A denied request cannot be taken; the classes of a denial are not stored.
    let (den, _) = store
        .request(&agent, new_request(&s, ApprovalSubject::Statement, 5), t0)
        .await
        .unwrap();
    let dw = store
        .decide(
            &user,
            authz,
            s.node,
            den.id,
            Decision {
                approve: false,
                comment: None,
                classes: Vec::new(),
                grant: None,
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(dw.value.status, ApprovalStatus::Denied);
    assert_eq!(
        code(
            store
                .take(
                    s.tenant,
                    s.node,
                    den.id,
                    &take(5, s.agent, ApprovalSubject::Statement),
                    t0
                )
                .await
        )
        .0,
        409
    );

    // A pending request past 24 hours is expired; the same request then makes a new row and
    // the old one is marked.
    let t25 = t0 + time::Duration::hours(PENDING_HOURS + 1);
    let (old, _) = store
        .request(&agent, new_request(&s, ApprovalSubject::Statement, 6), t0)
        .await
        .unwrap();
    let (new, created) = store
        .request(&agent, new_request(&s, ApprovalSubject::Statement, 6), t25)
        .await
        .unwrap();
    assert!(created);
    assert_ne!(new.id, old.id);
    assert_eq!(
        store
            .get(s.tenant, s.node, old.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        code(
            store
                .decide(&user, authz, s.node, old.id, approve(), t25)
                .await
        )
        .0,
        409
    );

    // Lists: newest first, paged, filtered; another tenant sees nothing.
    let all = store
        .list(
            s.tenant,
            ApprovalFilter::default(),
            PageRequest::default(),
            t0,
        )
        .await
        .unwrap();
    let ids: Vec<Uuid> = all.items.iter().map(|a| a.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_by_key(|x| std::cmp::Reverse(*x));
    assert_eq!(ids, sorted);
    let first = store
        .list(
            s.tenant,
            ApprovalFilter::default(),
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let second = store
        .list(
            s.tenant,
            ApprovalFilter::default(),
            PageRequest {
                cursor: first.next_cursor.clone(),
                limit: Some(2),
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(second.items[0].id, ids[2]);
    let other_agent = store
        .list(
            s.tenant,
            ApprovalFilter {
                requested_by: Some(AgentId::new()),
                ..ApprovalFilter::default()
            },
            PageRequest::default(),
            t0,
        )
        .await
        .unwrap();
    assert!(other_agent.items.is_empty());
    let used_only = store
        .list(
            s.tenant,
            ApprovalFilter {
                status: Some(ApprovalStatus::Used),
                ..ApprovalFilter::default()
            },
            PageRequest::default(),
            t0,
        )
        .await
        .unwrap();
    assert!(
        used_only
            .items
            .iter()
            .all(|a| a.status == ApprovalStatus::Used)
    );
    assert!(used_only.items.iter().any(|a| a.id == st.id));
    assert!(
        store
            .list(
                s.other,
                ApprovalFilter::default(),
                PageRequest::default(),
                t0
            )
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(store.get(s.other, s.node, g1.id).await.unwrap(), None);
    assert_eq!(
        code(
            store
                .decide(&ctx(s.other, s.user), authz, s.node, new.id, approve(), t25)
                .await
        )
        .0,
        404
    );
}

async fn agent_of(w: &World, user: UserId, max_role: Role, subtree: Option<NodeId>) -> Agent {
    w.s.agents
        .register(
            &ctx(w.team.id, user),
            NewAgent {
                name: format!("agent {}", Uuid::now_v7().simple()),
                description: None,
                scope: AgentScope {
                    subtree,
                    max_role,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap()
}

fn agent_ctx(a: &Agent) -> Context {
    Context {
        scope: Some(a.scope.as_scope()),
        ..agent_actor_ctx(a.tenant_id, a.id, a.owner_id)
    }
}

async fn database(w: &World, name: &str) -> NodeId {
    let a = ctx(w.team.id, w.alice);
    let id =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: name.into(),
                    manifest: json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    assert_eq!(
        settled(&w.s, &a, id).await.status.state,
        ProviderState::Ready
    );
    id
}

#[tokio::test]
async fn memory_store_conformance() {
    let w = world().await;
    let node = database(&w, "garden").await;
    let agent = agent_of(&w, w.alice, Role::Edit, None).await;
    let d = w.s.databases.deps();
    conformance(
        &*d.approvals,
        &*d.grants,
        &*d.authz,
        Setup {
            tenant: w.team.id,
            user: w.alice,
            node,
            agent: agent.id,
            other: TenantId::new(),
        },
    )
    .await;
}

// ---------------------------------------------------------------------------- the service

fn ask(kind: GrantKind, minutes: i64) -> RequestGrant {
    RequestGrant {
        kind,
        duration_minutes: minutes,
        reason: Some("match names".into()),
    }
}

fn decide_req(decision: DecisionKind, classes: Option<Vec<ConfirmItem>>) -> DecideRequest {
    DecideRequest {
        decision,
        comment: Some("fine".into()),
        classes,
    }
}

/// `requestDatabaseAccessGrant` and its decision on the in-memory services: Bob's agent asks;
/// Alice (a manager, not the agent's user) cannot decide; the agent cannot; Bob approves and
/// the grant exists, with `database.grant_created` and `database.approval_decided`.
#[tokio::test]
async fn a_grant_request_is_decided_by_the_agents_user_only() {
    let w = world().await;
    let node = database(&w, "garden").await;
    let s = &w.s;
    let alice = ctx(w.team.id, w.alice);
    let bob = ctx(w.team.id, w.bob);
    let bobs = agent_of(&w, w.bob, Role::View, None).await;
    let ac = agent_ctx(&bobs);
    let db = &s.databases;

    // A person asking: 403. Out of limits: 422. write_live with a view scope: 403.
    assert_eq!(
        code(
            db.request_grant(&alice, &*s.agents, node, ask(GrantKind::ReadPersonal, 60))
                .await
        ),
        (403, "permission_denied".into())
    );
    assert_eq!(
        code(
            db.request_grant(&ac, &*s.agents, node, ask(GrantKind::ReadPersonal, 481))
                .await
        )
        .0,
        422
    );
    assert_eq!(
        code(
            db.request_grant(&ac, &*s.agents, node, ask(GrantKind::WriteLive, 60))
                .await
        )
        .0,
        403
    );
    let (a, created) = db
        .request_grant(&ac, &*s.agents, node, ask(GrantKind::ReadPersonal, 60))
        .await
        .unwrap();
    assert!(created);
    let (again, created) = db
        .request_grant(&ac, &*s.agents, node, ask(GrantKind::ReadPersonal, 60))
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(again.id, a.id);
    let view = a.view("https://ruum.localhost", crate::now());
    assert_eq!(view["subject"], "grant");
    assert_eq!(view["grant_request"]["kind"], "read_personal");
    assert_eq!(view["grant_request"]["duration_minutes"], 60);

    // Alice manages the database but is not the agent's user: 403. The agent: 403
    // user_required. Nothing changed.
    assert_eq!(
        code(
            db.decide_approval(
                &alice,
                &*s.agents,
                node,
                a.id,
                decide_req(DecisionKind::Approve, None)
            )
            .await
        ),
        (403, "permission_denied".into())
    );
    assert_eq!(
        code(
            db.decide_approval(
                &ac,
                &*s.agents,
                node,
                a.id,
                decide_req(DecisionKind::Approve, None)
            )
            .await
        ),
        (403, "user_required".into())
    );
    // Classes go with migrations only.
    let classes = vec![ConfirmItem {
        schema: "main".into(),
        table: "plants".into(),
        column: "notes".into(),
        class: DataClass::Normal,
    }];
    assert_eq!(
        code(
            db.decide_approval(
                &bob,
                &*s.agents,
                node,
                a.id,
                decide_req(DecisionKind::Approve, Some(classes))
            )
            .await
        )
        .0,
        422
    );
    assert_eq!(
        db.get_approval(&bob, node, a.id).await.unwrap().status,
        ApprovalStatus::Pending
    );
    assert!(
        db.active_grant(w.team.id, node, bobs.id, GrantKind::ReadPersonal)
            .await
            .unwrap()
            .is_none()
    );

    // Bob, the agent's user, approves: the grant exists and holds.
    let w2 = db
        .decide_approval(
            &bob,
            &*s.agents,
            node,
            a.id,
            decide_req(DecisionKind::Approve, None),
        )
        .await
        .unwrap();
    assert_eq!(w2.value.status, ApprovalStatus::Approved);
    assert_eq!(w2.value.decided_by, Some(w.bob));
    let grant = db
        .active_grant(w.team.id, node, bobs.id, GrantKind::ReadPersonal)
        .await
        .unwrap()
        .expect("the approved request made a live grant");
    assert_eq!(Some(grant.id), w2.value.grant_id);
    assert_eq!(grant.granted_by, w.bob);
    let entries = w.handles.journal.entries(w.team.id);
    let decided = entries
        .iter()
        .find(|e| e.event == EventType::DatabaseApprovalDecided)
        .expect("database.approval_decided");
    assert_eq!(
        decided.data,
        json!({
            "approval_id": a.id, "subject": "grant", "decision": "approve",
            "reasons": ["grant"], "classes_confirmed": 0,
        })
    );
    assert!(entries.iter().any(
        |e| e.event == EventType::DatabaseGrantCreated && e.data["grant_id"] == json!(grant.id)
    ));
}

/// Statements and migrations on the in-memory services: who decides, hidden parameters, one
/// use with the same statement and parameters, classes in a migration decision.
#[tokio::test]
async fn statements_and_migrations_wait_for_a_person() {
    let w = world().await;
    let node = database(&w, "garden").await;
    let s = &w.s;
    let alice = ctx(w.team.id, w.alice);
    let bob = ctx(w.team.id, w.bob);
    let alices = agent_of(&w, w.alice, Role::Edit, None).await;
    let bobs = agent_of(&w, w.bob, Role::Edit, None).await;
    let (ac, bc) = (agent_ctx(&alices), agent_ctx(&bobs));
    let db = &s.databases;
    let row = db.deps().store.get(w.team.id, node).await.unwrap().unwrap();
    let sql = "update plants set notes = ?";
    let secret = json!("alice@example.test");

    // A person needs no approval.
    assert_eq!(
        code(
            db.request_statement(
                &alice,
                &row,
                sql,
                std::slice::from_ref(&secret),
                &[ApprovalReason::UpdateWithoutWhere],
                Some(3)
            )
            .await
        )
        .0,
        403
    );
    let (a, created) = db
        .request_statement(
            &ac,
            &row,
            sql,
            std::slice::from_ref(&secret),
            &[ApprovalReason::UpdateWithoutWhere],
            Some(3),
        )
        .await
        .unwrap();
    assert!(created);
    // Idempotent while pending; other parameters are another request.
    let (same, created) = db
        .request_statement(
            &ac,
            &row,
            sql,
            std::slice::from_ref(&secret),
            &[ApprovalReason::UpdateWithoutWhere],
            Some(3),
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(same.id, a.id);
    let (other, created) = db
        .request_statement(
            &ac,
            &row,
            sql,
            &[json!("bob@example.test")],
            &[ApprovalReason::UpdateWithoutWhere],
            Some(3),
        )
        .await
        .unwrap();
    assert!(created);
    assert_ne!(other.id, a.id);
    // Parameters are stored and shown as hidden markers only (decision 0004-24).
    let stored = db
        .deps()
        .approvals
        .get(w.team.id, node, a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.params,
        vec![json!({ "$ruum": "hidden", "class": "personal" })]
    );
    assert_eq!(stored.estimated_rows, Some(3));
    assert_eq!(stored.statement.as_deref(), Some(sql));
    let shown = db.get_approval(&ac, node, a.id).await.unwrap();
    assert!(
        !shown
            .view("https://x", crate::now())
            .to_string()
            .contains("alice@example.test")
    );
    // Another agent does not read it.
    assert_eq!(code(db.get_approval(&bc, node, a.id).await).0, 404);

    // Bob edits the database but is a stranger to Alice's agent: 403. Alice decides.
    assert_eq!(
        code(
            db.decide_approval(
                &bob,
                &*s.agents,
                node,
                a.id,
                decide_req(DecisionKind::Approve, None)
            )
            .await
        ),
        (403, "permission_denied".into())
    );
    db.decide_approval(
        &alice,
        &*s.agents,
        node,
        a.id,
        decide_req(DecisionKind::Approve, None),
    )
    .await
    .unwrap();
    // The take: other parameters, another statement, another agent: refused; then once.
    assert_eq!(
        code(
            db.take_statement(&ac, &row, a.id, sql, &[json!("bob@example.test")])
                .await
        )
        .0,
        409
    );
    assert_eq!(
        code(
            db.take_statement(
                &ac,
                &row,
                a.id,
                "update plants set notes =  ?",
                std::slice::from_ref(&secret)
            )
            .await
        )
        .0,
        409
    );
    assert_eq!(
        code(
            db.take_statement(&bc, &row, a.id, sql, std::slice::from_ref(&secret))
                .await
        )
        .0,
        404
    );
    let used = db
        .take_statement(&ac, &row, a.id, sql, std::slice::from_ref(&secret))
        .await
        .unwrap();
    assert_eq!(used.status, ApprovalStatus::Used);
    assert_eq!(
        code(
            db.take_statement(&ac, &row, a.id, sql, std::slice::from_ref(&secret))
                .await
        )
        .0,
        409
    );

    // A manager decides the statement of another user's agent; Bob (edit) decides his agent's
    // DELETE but not its DROP (a schema change needs manage).
    let (bd, _) = db
        .request_statement(
            &bc,
            &row,
            "delete from plants",
            &[],
            &[ApprovalReason::DeleteWithoutWhere],
            Some(1),
        )
        .await
        .unwrap();
    let (bdrop, _) = db
        .request_statement(
            &bc,
            &row,
            "drop table plants",
            &[],
            &[ApprovalReason::Drop],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        code(
            db.decide_approval(
                &bob,
                &*s.agents,
                node,
                bdrop.id,
                decide_req(DecisionKind::Approve, None)
            )
            .await
        ),
        (403, "permission_denied".into())
    );
    db.decide_approval(
        &bob,
        &*s.agents,
        node,
        bd.id,
        decide_req(DecisionKind::Deny, None),
    )
    .await
    .unwrap();
    db.decide_approval(
        &alice,
        &*s.agents,
        node,
        bdrop.id,
        decide_req(DecisionKind::Approve, None),
    )
    .await
    .unwrap();

    // A migration: planned columns from parsing up, the author's classes as suggestions; the
    // decision's classes need manage and are stored on the approval only.
    let up = "alter table plants add column notes2 text";
    let suggested = HashMap::from([("main.plants.notes2".to_string(), DataClass::Normal)]);
    let (m, _) = db
        .request_migration(&ac, &row, "0002_add_notes", up, &suggested)
        .await
        .unwrap();
    assert_eq!(m.planned_columns.len(), 1);
    assert_eq!(
        m.planned_columns[0].suggested_class,
        Some(DataClass::Normal)
    );
    assert_eq!(m.planned_columns[0].decided_class, None);
    assert_eq!(
        code(
            db.request_migration(&ac, &row, "bad name", up, &suggested)
                .await
        )
        .0,
        422
    );
    let classes = vec![ConfirmItem {
        schema: "main".into(),
        table: "plants".into(),
        column: "notes2".into(),
        class: DataClass::Personal,
    }];
    let dm = db
        .decide_approval(
            &alice,
            &*s.agents,
            node,
            m.id,
            decide_req(DecisionKind::Approve, Some(classes)),
        )
        .await
        .unwrap();
    assert_eq!(
        dm.value.planned_columns[0].decided_class,
        Some(DataClass::Personal)
    );
    let entry = w
        .handles
        .journal
        .entries(w.team.id)
        .into_iter()
        .rfind(|e| e.event == EventType::DatabaseApprovalDecided)
        .unwrap();
    assert_eq!(entry.data["classes_confirmed"], 1);
    assert!(
        !entry.data.to_string().contains(up),
        "the event holds no statement"
    );
    assert_eq!(
        code(
            db.take_migration(
                &ac,
                &row,
                m.id,
                "0002_add_notes",
                "alter table plants add column notes3 text"
            )
            .await
        )
        .0,
        409
    );
    let took = db
        .take_migration(&ac, &row, m.id, "0002_add_notes", up)
        .await
        .unwrap();
    assert_eq!(
        took.planned_columns[0].decided_class,
        Some(DataClass::Personal)
    );
}

/// `listPendingDatabaseApprovals` and `listDatabaseApprovals` on the in-memory services.
#[tokio::test]
async fn the_waiting_list_shows_what_the_caller_may_decide() {
    let w = world().await;
    let node = database(&w, "garden").await;
    let s = &w.s;
    let alice = ctx(w.team.id, w.alice);
    let bob = ctx(w.team.id, w.bob);
    let alices = agent_of(&w, w.alice, Role::Edit, None).await;
    let bobs = agent_of(&w, w.bob, Role::View, None).await;
    let (ac, bc) = (agent_ctx(&alices), agent_ctx(&bobs));
    let db = &s.databases;
    let row = db.deps().store.get(w.team.id, node).await.unwrap().unwrap();
    let (ga, _) = db
        .request_grant(&bc, &*s.agents, node, ask(GrantKind::ReadPersonal, 60))
        .await
        .unwrap();
    let (st, _) = db
        .request_statement(
            &ac,
            &row,
            "delete from plants",
            &[],
            &[ApprovalReason::DeleteWithoutWhere],
            None,
        )
        .await
        .unwrap();
    let ids = |p: Page<DatabaseApproval>| p.items.into_iter().map(|a| a.id).collect::<Vec<_>>();
    let page = PageRequest::default();
    // Alice: her agent's statement; not Bob's agent's grant request (only Bob decides it).
    assert_eq!(
        ids(db
            .list_pending_approvals(&alice, None, page.clone())
            .await
            .unwrap()),
        vec![st.id]
    );
    // Bob: his agent's grant request; not the statement of Alice's agent (a stranger with edit).
    assert_eq!(
        ids(db
            .list_pending_approvals(&bob, None, page.clone())
            .await
            .unwrap()),
        vec![ga.id]
    );
    // Agents: their own.
    assert_eq!(
        ids(db
            .list_pending_approvals(&ac, None, page.clone())
            .await
            .unwrap()),
        vec![st.id]
    );
    assert_eq!(
        ids(db
            .list_pending_approvals(&bc, None, page.clone())
            .await
            .unwrap()),
        vec![ga.id]
    );
    // Paging over a filtered list.
    let (st2, _) = db
        .request_statement(
            &ac,
            &row,
            "delete from beds",
            &[],
            &[ApprovalReason::DeleteWithoutWhere],
            None,
        )
        .await
        .unwrap();
    let p1 = db
        .list_pending_approvals(
            &alice,
            None,
            PageRequest {
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(p1.items[0].id, st2.id);
    let p2 = db
        .list_pending_approvals(
            &alice,
            None,
            PageRequest {
                cursor: p1.next_cursor.clone(),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(p2), vec![st.id]);
    // A database the caller cannot see is left out: Bob loses his edit grant on the folder.
    let grants = s
        .sharing
        .list_grants(&alice, w.folder, PageRequest::default())
        .await
        .unwrap()
        .items;
    for g in grants
        .into_iter()
        .filter(|g| g.grantee == Grantee::User { id: w.bob })
    {
        s.sharing
            .remove_grant(&alice, w.folder, g.id)
            .await
            .unwrap();
    }
    assert!(
        db.list_pending_approvals(&bob, None, page.clone())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        db.list_pending_approvals(&bc, None, page.clone())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // listDatabaseApprovals: view; an agent its own.
    assert_eq!(
        db.list_approvals(&alice, node, None, page.clone())
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    assert_eq!(
        ids(db
            .list_approvals(&ac, node, None, page.clone())
            .await
            .unwrap()),
        vec![st2.id, st.id]
    );
    assert_eq!(
        code(db.list_approvals(&bob, node, None, page.clone()).await).0,
        404
    );
}

// ---------------------------------------------------------------------------- Postgres

/// A tenant with a user, a ready database node (its `databases` row) and an agent of the
/// user, as the system writes them (as the grant tests of card A-1 do).
async fn seed_pg(pool: &sqlx::PgPool) -> (TenantId, UserId, NodeId, AgentId) {
    use crate::databases::DatabaseStore as _;
    let (t, u, root, node, v, agent) = (
        TenantId::new(),
        UserId::new(),
        NodeId::new(),
        NodeId::new(),
        crate::ids::VersionId::new(),
        AgentId::new(),
    );
    let manifest = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
    let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
    let mut tx = pool.begin().await.unwrap();
    for sql in [
        "set local ruum.system = 'on'",
        "set constraints all deferred",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
         values ($1, 't', 'team', 'team', $2, 'user', $3)",
    )
    .bind(t)
    .bind(root)
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into users (id, tenant_id, email, email_verified, display_name) \
         values ($1, $2, $3, true, 'x')",
    )
    .bind(u.0)
    .bind(t)
    .bind(format!("{}@example.test", u.0))
    .execute(&mut *tx)
    .await
    .unwrap();
    for (id, parent, kind, ty, head) in [
        (root, None, "folder", None, None),
        (
            node,
            Some(root),
            "typed",
            Some(crate::databases::NODE_TYPE),
            Some(v),
        ),
    ] {
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
             owner_id, created_kind, created_by, updated_kind, updated_by) \
             values ($1, $2, $3, $4, $5::node_kind, $6, $7, $8, 'user', $8, 'user', $8)",
        )
        .bind(id)
        .bind(t)
        .bind(parent)
        .bind(id.to_string())
        .bind(kind)
        .bind(ty)
        .bind(head)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
         content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, \
         $6, 'user', $7)",
    )
    .bind(v)
    .bind(t)
    .bind(node)
    .bind(&bytes)
    .bind(bytes.len() as i64)
    .bind(hash.to_vec())
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    crate::providers::postgres::insert_typed_node(
        &mut tx,
        &crate::providers::store::NewTypedNode {
            tenant_id: t,
            node_id: node,
            provider: crate::databases::PROVIDER_ID.into(),
            type_name: crate::databases::NODE_TYPE.into(),
            desired_version: v,
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "insert into agents (id, tenant_id, owner_id, name, scope_max_role, created_kind, \
         created_by) values ($1, $2, $3, 'agent', 'edit', 'user', $3)",
    )
    .bind(agent.0)
    .bind(t)
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    crate::databases::PgDatabaseStore::new(pool.clone())
        .insert(
            &crate::databases::DatabaseRow {
                tenant_id: t,
                node_id: node,
                engine: EngineKind::Sqlite,
                engine_version: "3".into(),
                cluster: "memory".into(),
                engine_name: format!("d_{}", &Uuid::now_v7().simple().to_string()[16..]),
                branch_of: None,
                branch_data: None,
                key_node_id: None,
                extensions: Vec::new(),
                size_bytes: 4096,
            },
            &[],
        )
        .await
        .unwrap();
    (t, u, node, agent)
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_store_conformance(pool: sqlx::PgPool) {
    let (t, u, node, agent) = seed_pg(&pool).await;
    let (other, ..) = seed_pg(&pool).await;
    conformance(
        &PgApprovalStore::new(pool.clone()),
        &PgGrantStore::new(pool.clone()),
        &MemoryAuthz::default(),
        Setup {
            tenant: t,
            user: u,
            node,
            agent,
            other,
        },
    )
    .await;
}

/// The Postgres store keeps every row with its tenant, with row-level security off, so only
/// the tenant filters of its statements hold (the mutation
/// `databases-approvals-tenant-filter-postgres` breaks one).
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_approval_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
    sqlx::query("alter table database_approvals no force row level security")
        .execute(&pool)
        .await
        .unwrap();
    let store = PgApprovalStore::new(pool.clone());
    let (a, u, node, agent) = seed_pg(&pool).await;
    let (b, ub, ..) = seed_pg(&pool).await;
    let now = micros(crate::now());
    let s = Setup {
        tenant: a,
        user: u,
        node,
        agent,
        other: b,
    };
    let (st, _) = store
        .request(
            &agent_actor_ctx(a, agent, u),
            new_request(&s, ApprovalSubject::Statement, 9),
            now,
        )
        .await
        .unwrap();
    assert_eq!(store.get(b, node, st.id).await.unwrap(), None);
    assert!(
        store
            .list(b, ApprovalFilter::default(), PageRequest::default(), now)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let take = Take {
        agent,
        subject: ApprovalSubject::Statement,
        statement_hash: [9; 32],
    };
    assert_eq!(code(store.take(b, node, st.id, &take, now).await).0, 404);
    assert_eq!(
        code(
            store
                .decide(
                    &ctx(b, ub),
                    &MemoryAuthz::default(),
                    node,
                    st.id,
                    approve(),
                    now
                )
                .await
        )
        .0,
        404
    );
    assert_eq!(
        store.get(a, node, st.id).await.unwrap().unwrap().status,
        ApprovalStatus::Pending
    );
}
