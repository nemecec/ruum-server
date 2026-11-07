//! Names of actors in answers (contract need C-7, decision 0003-5), on the backend of the
//! suites (`RUUM_SUITE_BACKEND=memory|postgres`): every answer type that names a principal
//! carries `display_name`, agents carry the name of their user, a person who left keeps a
//! name, share-link bearers and other tenants learn nothing, and `listChanges` filters by
//! `actor` and `since`.

use ruum_testkit::fixtures::{self, Backend, Target};
use ruum_testkit::{RequestSpec, Who, client};
use serde_json::{Value, json};
use std::collections::HashMap;

fn ok(res: &ruum_testkit::Response, what: &str) -> Value {
    assert!(
        res.status.is_success(),
        "{what}: {} {}",
        res.status,
        res.text()
    );
    res.json().unwrap()
}

#[tokio::test]
async fn answers_name_their_actors() {
    let backend = Backend::start().await;
    let (state, db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    assert!(w.seeded, "the fixture world must seed");
    let app = client::app(state.clone());
    let a = &w.a;
    let t = a.id;
    let as_ = |who: Who| a.principal(who);

    // The names of the people of A, from listMembers.
    let members = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/tenants/{t}/members?limit=100")),
            as_(Who::Owner),
        )
        .await,
        "listMembers",
    );
    let names: HashMap<String, String> = members["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user_id"].as_str().unwrap().to_string(),
                m["display_name"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let name_of = |who: Who| names[&a.user_id(who).0.to_string()].clone();
    let member_name = name_of(Who::Member);

    // An agent of the member, renamed exactly like the member (agent names come from the
    // client), writes a folder.
    let agent_id = a.agent_id(Who::AgentOut);
    ok(
        &client::send(
            &app,
            &RequestSpec::new(
                axum::http::Method::PATCH,
                format!("/v1/agents/{}", agent_id.0),
            )
            .tenant(t)
            .json(json!({ "name": member_name })),
            as_(Who::Member),
        )
        .await,
        "updateAgent",
    );
    let made = ok(
        &client::send(
            &app,
            &RequestSpec::post("/v1/nodes").tenant(t).json(json!({
                "parent_id": a.node(Target::Private), "name": "By the agent", "kind": "folder"
            })),
            as_(Who::AgentOut),
        )
        .await,
        "createNode by the agent",
    );
    let by_agent = made["node"]["id"].as_str().unwrap().to_string();

    // getNode: created_by names the agent and its user.
    let node = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/nodes/{by_agent}")).tenant(t),
            as_(Who::Owner),
        )
        .await,
        "getNode",
    );
    let cb = &node["created_by"];
    assert_eq!(cb["actor"]["kind"], "agent", "{node}");
    assert_eq!(cb["actor"]["display_name"], member_name.as_str(), "{node}");
    assert_eq!(
        cb["on_behalf_of"],
        json!(a.user_id(Who::Member).0),
        "{node}"
    );
    assert_eq!(
        cb["on_behalf_of_display_name"],
        member_name.as_str(),
        "{node}"
    );

    // listChanges: every user or agent actor has a name; agents also their user's name.
    let changes = ok(
        &client::send(
            &app,
            &RequestSpec::get("/v1/changes?limit=500").tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listChanges",
    );
    let items = changes["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for e in items {
        if matches!(e["actor"]["kind"].as_str(), Some("user" | "agent")) {
            assert!(e["actor"]["display_name"].is_string(), "{e}");
        }
        if !e["on_behalf_of"].is_null() {
            assert!(e["on_behalf_of_display_name"].is_string(), "{e}");
        }
    }

    // listChanges?actor= keeps only that actor; since= in the future keeps nothing.
    let only = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/changes?limit=500&actor={}", agent_id.0)).tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listChanges?actor",
    );
    let only = only["items"].as_array().unwrap();
    assert!(!only.is_empty());
    assert!(only.iter().all(|e| e["actor"]["id"] == json!(agent_id.0)));
    assert!(
        only.iter()
            .any(|e| e["node_id"] == json!(by_agent) && e["type"] == "node.created")
    );
    let later = ok(
        &client::send(
            &app,
            &RequestSpec::get("/v1/changes?limit=500&since=2999-01-01T00:00:00Z").tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listChanges?since",
    );
    assert!(later["items"].as_array().unwrap().is_empty());
    let bad = client::send(
        &app,
        &RequestSpec::get("/v1/changes?since=yesterday").tenant(t),
        as_(Who::Owner),
    )
    .await;
    assert_eq!(bad.status.as_u16(), 400, "{}", bad.text());

    // listVersions: each version names its actor.
    let versions = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/nodes/{}/versions", a.node(Target::Plan))).tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listVersions",
    );
    for v in versions["items"].as_array().unwrap() {
        assert!(v["actor"]["display_name"].is_string(), "{v}");
    }

    // listGrants: a member who may share sees the grantee's name and email; a guest who may
    // share (manage on Projects) sees the name only.
    let grants_uri = format!("/v1/nodes/{}/grants", a.node(Target::Projects));
    let by_owner = ok(
        &client::send(
            &app,
            &RequestSpec::get(&grants_uri).tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listGrants as owner",
    );
    let gv = a.user_id(Who::GuestView).0.to_string();
    let find = |page: &Value| -> Value {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["grantee"]["id"] == json!(gv))
            .cloned()
            .unwrap_or_else(|| panic!("no grant to guest_view in {page}"))
    };
    let g = find(&by_owner);
    assert_eq!(
        g["grantee"]["display_name"],
        name_of(Who::GuestView).as_str()
    );
    assert_eq!(
        g["grantee"]["email"].as_str(),
        as_(Who::GuestView).email.as_deref()
    );
    assert!(g["created_by"]["actor"]["display_name"].is_string(), "{g}");
    let by_guest = ok(
        &client::send(
            &app,
            &RequestSpec::get(&grants_uri).tenant(t),
            as_(Who::GuestManage),
        )
        .await,
        "listGrants as guest_manage",
    );
    let g = find(&by_guest);
    assert_eq!(
        g["grantee"]["display_name"],
        name_of(Who::GuestView).as_str()
    );
    assert!(g["grantee"].get("email").is_none(), "{g}");

    // Shared with me (no tenant header): the sharer is named in the item's tenant.
    let shared = ok(
        &client::send(
            &app,
            &RequestSpec::get("/v1/shared-with-me"),
            as_(Who::GuestView),
        )
        .await,
        "listSharedWithMe",
    );
    let item = shared["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["node"]["id"] == json!(a.node(Target::Projects)))
        .cloned()
        .unwrap_or_else(|| panic!("Projects not shared with guest_view: {shared}"));
    assert!(
        item["shared_by"]["actor"]["display_name"].is_string(),
        "{item}"
    );
    // C-10 (decision 0003-6): the item names its space, where guest_view is only a guest.
    assert_eq!(item["tenant_name"], json!(a.name), "{item}");
    assert!(
        shared["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["tenant_name"].is_string()),
        "{shared}"
    );

    // The audit log names its actors.
    let audit = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/tenants/{t}/audit?limit=200")),
            as_(Who::Owner),
        )
        .await,
        "listAudit",
    );
    let rows = audit["items"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["actor"]["kind"] == "user" && r["actor"]["display_name"].is_string()),
        "{audit}"
    );

    // A share-link bearer gets no names.
    let by_link = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/nodes/{}", a.node(Target::Projects))).tenant(t),
            as_(Who::ShareLink),
        )
        .await,
        "getNode by the share link",
    );
    assert!(
        by_link["created_by"]["actor"].get("display_name").is_none(),
        "{by_link}"
    );

    // Tenant B never learns A's names: the bridge-free view of B's own feed names only B's
    // people, and A's member ID asked in B's feed filter gives nothing.
    let b_changes = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!(
                "/v1/changes?limit=500&actor={}",
                a.user_id(Who::Member).0
            ))
            .tenant(w.b.id),
            w.b.principal(Who::Owner),
        )
        .await,
        "listChanges in B",
    );
    assert!(b_changes["items"].as_array().unwrap().is_empty());
    assert!(!b_changes.to_string().contains(&member_name));

    // The member is removed: their entries keep the name (the tenant's snapshot), and the
    // agent keeps its own name.
    let removed = client::send(
        &app,
        &RequestSpec::new(
            axum::http::Method::DELETE,
            format!("/v1/tenants/{t}/members/{}", a.user_id(Who::Member).0),
        ),
        as_(Who::Owner),
    )
    .await;
    assert!(removed.status.is_success(), "{}", removed.text());
    let after = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!(
                "/v1/changes?limit=500&actor={}",
                a.user_id(Who::Member).0
            ))
            .tenant(t),
            as_(Who::Owner),
        )
        .await,
        "listChanges after the removal",
    );
    let after = after["items"].as_array().unwrap();
    assert!(!after.is_empty());
    for e in after {
        assert_eq!(e["actor"]["display_name"], member_name.as_str(), "{e}");
    }
    let node = ok(
        &client::send(
            &app,
            &RequestSpec::get(format!("/v1/nodes/{by_agent}")).tenant(t),
            as_(Who::Owner),
        )
        .await,
        "getNode after the removal",
    );
    assert_eq!(
        node["created_by"]["actor"]["display_name"],
        member_name.as_str()
    );
    assert_eq!(
        node["created_by"]["on_behalf_of_display_name"],
        member_name.as_str()
    );

    if let Some(db) = db {
        db.finish().await;
    }
    backend.finish().await;
}

/// C-9 (decision 0003-6) through the router, without sign-in: `previewInvitation` names the
/// space and the inviter (C-7), masks the email and never sends it in clear; tenant B's
/// token names only tenant B.
#[tokio::test]
async fn invitation_previews_name_the_space_and_the_inviter() {
    let backend = Backend::start().await;
    let (state, _db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    assert!(w.seeded, "the fixture world must seed");
    let app = client::app(state.clone());
    // The fixture names each user "<prefix> <who>".
    let owner_name = |t: &fixtures::TenantFixture| format!("{} {}", t.prefix, Who::Owner.name());
    for t in [&w.a, &w.b] {
        let p = ok(
            &client::send(
                &app,
                &RequestSpec::post("/v1/invitations/preview")
                    .json(json!({ "token": t.invitation_token })),
                t.principal(Who::Anonymous),
            )
            .await,
            "previewInvitation",
        );
        assert_eq!(p["state"], "pending", "{p}");
        assert_eq!(p["tenant_name"], json!(t.name), "{p}");
        assert_eq!(p["tenant_kind"], "team", "{p}");
        assert_eq!(p["role"], "member", "{p}");
        assert_eq!(p["content_role"], "manage", "{p}");
        assert_eq!(p["inviter_display_name"], json!(owner_name(t)), "{p}");
        let text = p.to_string();
        assert!(!text.contains(&t.invitation_email), "{p}");
        assert!(p["email_masked"].as_str().unwrap().contains("***"), "{p}");
        let other = if t.id == w.a.id { &w.b } else { &w.a };
        assert!(!text.contains(&other.name), "{p}");
    }
}
