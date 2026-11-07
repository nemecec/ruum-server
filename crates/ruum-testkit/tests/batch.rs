//! The batch operations `moveNodes` and `attachTagToNodes` (decision 0003-7, BENCHMARK fix 5)
//! on the backend of the suites (`RUUM_SUITE_BACKEND=memory|postgres`): one result per item in
//! the order of the request, each with the status of the single operation; one item's error does
//! not stop the others; 1 to 100 items. The isolation and permission suites check one-item
//! batches for every principal; this test checks mixed batches.

use ruum_testkit::fixtures::{self, Backend, Target};
use ruum_testkit::{RequestSpec, Who, client};
use serde_json::{Value, json};

fn statuses(res: &ruum_testkit::Response) -> Vec<u64> {
    assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.text());
    res.json().unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn batches_answer_per_item() {
    let backend = Backend::start().await;
    let (state, db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    assert!(w.seeded, "the fixture world must seed");
    let app = client::app(state.clone());
    let a = &w.a;
    let t = a.id;
    let (plan, archive, private) = (
        a.node(Target::Plan),
        a.node(Target::Archive),
        a.node(Target::Private),
    );
    let unknown = uuid::Uuid::now_v7();

    // A guest with manage on Projects: the plan moves; Private (not seen) and an unknown node
    // are 404; the error of one item does not stop the next one.
    let moved = client::send_raw(
        &app,
        &RequestSpec::post("/v1/nodes/move")
            .tenant(t)
            .json(json!({ "items": [
                { "node_id": private, "parent_id": archive },
                { "node_id": plan, "parent_id": archive },
                { "node_id": unknown, "parent_id": archive },
            ]})),
        a.principal(Who::GuestManage),
    )
    .await;
    assert_eq!(statuses(&moved), vec![404, 200, 404]);
    let body = moved.json().unwrap();
    assert_eq!(body["items"][1]["node"]["parent_id"], json!(archive));
    assert!(body["items"][1]["change"]["seq"].is_number());
    assert_eq!(body["items"][0]["error"]["code"], "not_found");
    let got = client::send(
        &app,
        &RequestSpec::get(format!("/v1/nodes/{plan}")).tenant(t),
        a.principal(Who::Owner),
    )
    .await;
    assert_eq!(got.json().unwrap()["parent_id"], json!(archive));

    // A guest with edit: 403 for the move (needs manage), item by item.
    let refused = client::send_raw(
        &app,
        &RequestSpec::post("/v1/nodes/move")
            .tenant(t)
            .json(json!({ "items": [
                { "node_id": plan, "parent_id": a.node(Target::Projects) },
            ]})),
        a.principal(Who::GuestEdit),
    )
    .await;
    assert_eq!(statuses(&refused), vec![403]);

    // attachTagToNodes: edit on Archive, nothing on Private, a second attach is a no-op.
    let tagged = client::send_raw(
        &app,
        &RequestSpec::post(format!("/v1/tags/{}/nodes", a.tag))
            .tenant(t)
            .json(json!({ "node_ids": [archive, private, archive] })),
        a.principal(Who::GuestEdit),
    )
    .await;
    assert_eq!(statuses(&tagged), vec![200, 404, 200]);
    let items: Vec<Value> = tagged.json().unwrap()["items"].as_array().unwrap().clone();
    assert!(items[0]["change"]["seq"].is_number());
    assert!(
        items[2]["change"]["seq"].is_null(),
        "attaching twice is a no-op"
    );
    let tags = client::send(
        &app,
        &RequestSpec::get(format!("/v1/nodes/{archive}/tags")).tenant(t),
        a.principal(Who::Owner),
    )
    .await;
    assert!(tags.text().contains(&a.tag.0.to_string()));

    // 0 or 101 items: 422 for the whole call.
    let too_many: Vec<Value> = (0..101)
        .map(|_| json!({ "node_id": plan, "parent_id": archive }))
        .collect();
    for body in [json!({ "items": [] }), json!({ "items": too_many })] {
        let r = client::send_raw(
            &app,
            &RequestSpec::post("/v1/nodes/move").tenant(t).json(body),
            a.principal(Who::Owner),
        )
        .await;
        assert_eq!(r.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }
    let r = client::send_raw(
        &app,
        &RequestSpec::post(format!("/v1/tags/{}/nodes", a.tag))
            .tenant(t)
            .json(json!({ "node_ids": [] })),
        a.principal(Who::Owner),
    )
    .await;
    assert_eq!(r.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    if let Some(db) = db {
        db.finish().await;
    }
    backend.finish().await;
}
