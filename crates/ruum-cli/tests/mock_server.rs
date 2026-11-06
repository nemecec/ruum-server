//! The file commands of the CLI against a test server that plays the contract
//! (`contracts/openapi.yaml`) with fixed answers, and checks the requests the CLI sends.

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "ruum_agent_testkey";
const ROOT: &str = "00000000-0000-7000-8000-000000000001";
const PROJECTS: &str = "00000000-0000-7000-8000-000000000002";
const PLAN: &str = "00000000-0000-7000-8000-000000000003";
const NEW: &str = "00000000-0000-7000-8000-000000000004";

fn node(id: &str, parent: Option<&str>, name: &str, kind: &str) -> Value {
    json!({
        "id": id, "tenant_id": "00000000-0000-7000-8000-0000000000aa", "parent_id": parent,
        "name": name, "kind": kind, "type": null, "link_kind": null, "link_target": null,
        "head_version": if kind == "file" { json!("00000000-0000-7000-8000-0000000000v1") } else { Value::Null },
        "owner_id": "00000000-0000-7000-8000-0000000000bb", "size_bytes": 14, "revision": 1,
        "trashed_at": null, "created_at": "2026-11-06T10:00:00Z", "updated_at": "2026-11-06T10:00:00Z",
        "created_by": {}, "updated_by": {}
    })
}

fn not_found() -> ResponseTemplate {
    ResponseTemplate::new(404).set_body_json(json!({"error": {
        "code": "not_found", "message": "node not found", "hint": "check the path"}}))
}

struct Env {
    server: MockServer,
    dir: tempfile::TempDir,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok", "token_type": "Bearer", "expires_in": 900,
            "issued_token_type": "urn:ietf:params:oauth:token-type:access_token"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/root"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(node(ROOT, None, "root", "folder")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(node(
            PROJECTS,
            Some(ROOT),
            "Projects",
            "folder",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects/plan.md"))
        .respond_with(ResponseTemplate::new(200).set_body_json(node(
            PLAN,
            Some(PROJECTS),
            "plan.md",
            "file",
        )))
        .mount(&server)
        .await;
    Env {
        server,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    async fn ruum(&self, args: &[&str]) -> anyhow::Result<String> {
        let creds = self.dir.path().join("creds.json").display().to_string();
        let url = self.server.uri();
        let mut all = vec!["--credentials", &creds, "--url", &url, "--api-key", KEY];
        all.extend_from_slice(args);
        let mut out = Vec::new();
        ruum_cli::run_args(all, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    async fn requests(&self, m: &str, p: &str) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == m && r.url.path() == p)
            .collect()
    }
}

#[tokio::test]
async fn ls_reads_every_page() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/nodes/{PROJECTS}/children")))
        .and(query_param("cursor", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [node(NEW, Some(PROJECTS), "second.txt", "file")], "next_cursor": null})))
        .with_priority(1)
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/nodes/{PROJECTS}/children")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [node(PLAN, Some(PROJECTS), "plan.md", "file"),
                      node(ROOT, Some(PROJECTS), "Garden", "folder")],
            "next_cursor": "c2"})))
        .with_priority(5)
        .mount(&e.server)
        .await;
    let out = e.ruum(&["ls", "/Projects"]).await.unwrap();
    assert!(out.contains("plan.md"), "{out}");
    assert!(out.contains("Garden/"), "{out}");
    assert!(out.contains("second.txt"), "{out}");
    // The token exchange sent the key as an RFC 8693 subject token.
    let tok = e.requests("POST", "/v1/oauth/token").await;
    let body = String::from_utf8(tok[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange"));
    assert!(body.contains("subject_token=ruum_agent_testkey"));
    assert!(body.contains("subject_token_type=urn%3Aruum%3Aparams%3Aoauth%3Atoken-type%3Aapi-key"));
}

#[tokio::test]
async fn errors_show_the_ruum_code_and_hint() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Private"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": "scope_exceeded", "message": "the node is outside the agent scope",
            "hint": "ask your user to widen the scope"}})))
        .mount(&e.server)
        .await;
    let err = e.ruum(&["ls", "/Private"]).await.unwrap_err();
    let text = ruum_cli::report(&err);
    assert_eq!(
        text,
        "error: scope_exceeded: the node is outside the agent scope\nhint: ask your user to widen the scope"
    );
}

#[tokio::test]
async fn mkdir_makes_missing_parents() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects/Garden"))
        .respond_with(not_found())
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects/Garden/Beds"))
        .respond_with(not_found())
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/nodes"))
        .and(header_exists("idempotency-key"))
        .respond_with(|r: &Request| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            let name = b["name"].as_str().unwrap().to_string();
            ResponseTemplate::new(201).set_body_json(json!({
                "node": node(NEW, b["parent_id"].as_str(), &name, "folder"), "change": {}}))
        })
        .mount(&e.server)
        .await;
    let err = e
        .ruum(&["mkdir", "/Projects/Garden/Beds"])
        .await
        .unwrap_err();
    assert!(ruum_cli::report(&err).contains("mkdir -p"));
    let out = e
        .ruum(&["mkdir", "-p", "/Projects/Garden/Beds"])
        .await
        .unwrap();
    assert!(out.contains("created /Projects/Garden/Beds/"), "{out}");
    let posts = e.requests("POST", "/v1/nodes").await;
    assert_eq!(posts.len(), 2);
    let first: Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(
        first,
        json!({"parent_id": PROJECTS, "name": "Garden", "kind": "folder"})
    );
}

#[tokio::test]
async fn upload_sends_only_missing_chunks() {
    let e = env().await;
    // 200 kB with the small parameter set gives several chunks.
    let data: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
        .collect();
    let local = e.dir.path().join("photo.bin");
    std::fs::write(&local, &data).unwrap();

    Mock::given(method("GET"))
        .and(path("/v1/uploads/params"))
        .and(query_param("size", "200000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fastcdc-2020-16k-32k-64k", "algorithm": "fastcdc-2020",
            "min_size": 16384, "avg_size": 32768, "max_size": 65536})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uploads"))
        .and(body_partial_json(
            json!({"target": "new_file", "parent_id": PROJECTS,
            "name": "photo.bin", "chunking": "fastcdc-2020-16k-32k-64k", "size": 200000}),
        ))
        .respond_with(|r: &Request| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            // The server already has the first chunk.
            let missing: Vec<Value> = b["chunks"].as_array().unwrap()[1..]
                .iter()
                .map(|c| c["hash"].clone())
                .collect();
            ResponseTemplate::new(201).set_body_json(json!({"id": "up1", "missing": missing}))
        })
        .mount(&e.server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uploads/up1/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "node": node(NEW, Some(PROJECTS), "photo.bin", "file"),
            "version": {"id": "v-new"}, "change": {}})))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects/photo.bin"))
        .respond_with(not_found())
        .mount(&e.server)
        .await;

    let out = e
        .ruum(&["upload", &local.display().to_string(), "/Projects"])
        .await
        .unwrap();
    assert!(out.contains("version v-new"), "{out}");

    let create = &e.requests("POST", "/v1/uploads").await[0];
    let b: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(b["content_hash"], blake3::hash(&data).to_hex().to_string());
    let chunks = b["chunks"].as_array().unwrap();
    assert!(chunks.len() > 2);
    let total: u64 = chunks.iter().map(|c| c["size"].as_u64().unwrap()).sum();
    assert_eq!(total, 200_000);

    let puts: Vec<Request> = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect();
    assert_eq!(puts.len(), chunks.len() - 1, "the first chunk is not sent");
    for p in &puts {
        let hash = p.url.path().rsplit('/').next().unwrap();
        assert_eq!(blake3::hash(&p.body).to_hex().to_string(), hash);
        assert!(p.headers.contains_key("idempotency-key"));
    }
}

#[tokio::test]
async fn upload_onto_a_file_makes_a_new_version() {
    let e = env().await;
    let local = e.dir.path().join("plan.md");
    std::fs::write(&local, b"# v2\n").unwrap();
    Mock::given(method("GET"))
        .and(path("/v1/uploads/params"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fastcdc-2020-16k-32k-64k", "algorithm": "fastcdc-2020",
            "min_size": 16384, "avg_size": 32768, "max_size": 65536})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uploads"))
        .and(body_partial_json(
            json!({"target": "new_version", "node_id": PLAN,
            "base_version": "00000000-0000-7000-8000-0000000000v1"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "up2", "missing": []})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uploads/up2/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "node": node(PLAN, Some(PROJECTS), "plan.md", "file"),
            "version": {"id": "v2"}, "change": {}})))
        .mount(&e.server)
        .await;
    let out = e
        .ruum(&["upload", &local.display().to_string(), "/Projects/plan.md"])
        .await
        .unwrap();
    assert!(out.contains("0 of 1 chunks sent"), "{out}");
}

#[tokio::test]
async fn download_writes_the_file() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/nodes/{PLAN}/content")))
        .and(query_param("version", "v1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"# Garden plan\n".to_vec()))
        .mount(&e.server)
        .await;
    let target = e.dir.path().join("out.md");
    e.ruum(&[
        "download",
        "/Projects/plan.md",
        &target.display().to_string(),
        "--version",
        "v1",
    ])
    .await
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"# Garden plan\n");
    let out = e
        .ruum(&["download", "/Projects/plan.md", "-", "--version", "v1"])
        .await
        .unwrap();
    assert_eq!(out, "# Garden plan\n");
}

#[tokio::test]
async fn mv_renames_in_place_or_moves() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/Projects/plan-v2.md"))
        .respond_with(not_found())
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/plan.md"))
        .respond_with(not_found())
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"node": {}, "change": {}})))
        .mount(&e.server)
        .await;
    e.ruum(&["mv", "/Projects/plan.md", "/Projects/plan-v2.md"])
        .await
        .unwrap();
    let r = &e
        .requests("POST", &format!("/v1/nodes/{PLAN}/rename"))
        .await[0];
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        json!({"name": "plan-v2.md"})
    );
    e.ruum(&["mv", "/Projects/plan.md", "/plan.md"])
        .await
        .unwrap();
    let r = &e.requests("POST", &format!("/v1/nodes/{PLAN}/move")).await[0];
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        json!({"parent_id": ROOT, "name": "plan.md"})
    );
    // Into an existing folder.
    e.ruum(&["mv", "/Projects/plan.md", "/Projects"])
        .await
        .unwrap();
    let r = &e.requests("POST", &format!("/v1/nodes/{PLAN}/move")).await[1];
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        json!({"parent_id": PROJECTS})
    );
}

#[tokio::test]
async fn rm_trashes_and_versions_lists() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/nodes/{PLAN}/trash")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"node": {}, "change": {}})))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/nodes/{PLAN}/versions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "00000000-0000-7000-8000-0000000000v1", "size_bytes": 14,
             "created_at": "2026-11-06T10:00:00Z", "label": "first",
             "actor": {"kind": "agent", "id": "a1"}}], "next_cursor": null})))
        .mount(&e.server)
        .await;
    let out = e.ruum(&["rm", "/Projects/plan.md"]).await.unwrap();
    assert!(out.contains("trash"), "{out}");
    let out = e.ruum(&["versions", "/Projects/plan.md"]).await.unwrap();
    assert!(out.starts_with("* "), "the head version is marked: {out}");
    assert!(
        out.contains("agent:a1") && out.contains("\"first\""),
        "{out}"
    );
    let err = e.ruum(&["rm", "/"]).await.unwrap_err();
    assert!(ruum_cli::report(&err).contains("root"));
}

#[tokio::test]
async fn share_with_a_person_or_a_link() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/nodes/{PROJECTS}/grants")))
        .and(body_partial_json(
            json!({"grantee": {"type": "email", "email": "juhan@example.test"},
            "role": "edit"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"grant": {}, "change": {}})))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/nodes/{PROJECTS}/share-links")))
        .and(body_partial_json(json!({"role": "view"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "share_link": {"role": "view", "url": "https://ruum.localhost/s/abc"}, "change": {}})))
        .mount(&e.server)
        .await;
    let out = e
        .ruum(&[
            "share",
            "/Projects",
            "--email",
            "juhan@example.test",
            "--role",
            "edit",
        ])
        .await
        .unwrap();
    assert!(out.contains("shared /Projects (edit)"), "{out}");
    let out = e.ruum(&["share", "/Projects", "--link"]).await.unwrap();
    assert!(out.contains("https://ruum.localhost/s/abc"), "{out}");
}

#[tokio::test]
async fn a_disabled_agent_shows_the_ruum_code_of_the_token_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "the agent is disabled",
            "ruum_code": "agent_disabled"})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json").display().to_string();
    let url = server.uri();
    let mut out = Vec::new();
    let err = ruum_cli::run_args(
        [
            "--credentials",
            &creds,
            "--url",
            &url,
            "--api-key",
            KEY,
            "whoami",
        ],
        &mut out,
    )
    .await
    .unwrap_err();
    let api = err
        .downcast_ref::<ruum_cli::client::ApiError>()
        .expect("an API error");
    assert_eq!(api.code, "agent_disabled");
    assert!(api.message.contains("invalid_grant"), "{}", api.message);
    let text = ruum_cli::report(&err);
    assert!(
        text.starts_with("error: agent_disabled: the agent is disabled"),
        "{text}"
    );

    // Without ruum_code the OAuth error is the code.
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant", "error_description": "unknown key"})))
        .mount(&server)
        .await;
    let err = ruum_cli::run_args(
        [
            "--credentials",
            &creds,
            "--url",
            &url,
            "--api-key",
            KEY,
            "whoami",
        ],
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(
        ruum_cli::report(&err).starts_with("error: invalid_grant: unknown key"),
        "{err:#}"
    );
}

#[tokio::test]
async fn the_saved_key_is_never_sent_to_another_url() {
    let e = env().await;
    let other = MockServer::start().await;
    let creds_path = e.dir.path().join("saved.json");
    let creds = creds_path.display().to_string();
    let url = e.server.uri();
    let mut out = Vec::new();
    ruum_cli::run_args(
        [
            "--credentials",
            &creds,
            "--url",
            &url,
            "login",
            "--key",
            KEY,
        ],
        &mut out,
    )
    .await
    .ok(); // /v1/me is not mocked; the key and URL are saved before it.
    let saved = std::fs::read_to_string(&creds_path).expect("saved credentials");

    let other_url = other.uri();
    let err = ruum_cli::run_args(
        ["--credentials", &creds, "--url", &other_url, "ls"],
        &mut out,
    )
    .await
    .unwrap_err();
    let text = ruum_cli::report(&err);
    assert!(text.contains("the saved key belongs to"), "{text}");
    assert!(text.contains("ruum login"), "{text}");
    assert!(
        other.received_requests().await.unwrap().is_empty(),
        "nothing may reach the other host"
    );
    assert_eq!(
        std::fs::read_to_string(&creds_path).unwrap(),
        saved,
        "the credentials file is unchanged"
    );

    // With an explicit key the other URL is used, and the file still names the first URL.
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok2", "token_type": "Bearer", "expires_in": 900})))
        .mount(&other)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer tok2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "actor": {"kind": "agent"}, "user": {"email": "mari@example.test"},
            "agent": {"id": "a1", "name": "Other bot"}})))
        .mount(&other)
        .await;
    let mut out = Vec::new();
    ruum_cli::run_args(
        [
            "--credentials",
            &creds,
            "--url",
            &other_url,
            "--api-key",
            KEY,
            "whoami",
        ],
        &mut out,
    )
    .await
    .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("Other bot"));
    assert_eq!(std::fs::read_to_string(&creds_path).unwrap(), saved);
}

#[tokio::test]
async fn plain_http_is_refused_for_hosts_that_are_not_local() {
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json").display().to_string();
    for url in [
        "http://ruum.example.com",
        "http://10.0.0.5:8080",
        "ftp://ruum.localhost",
    ] {
        let mut out = Vec::new();
        let err = ruum_cli::run_args(
            [
                "--credentials",
                &creds,
                "--url",
                url,
                "--api-key",
                KEY,
                "whoami",
            ],
            &mut out,
        )
        .await
        .unwrap_err();
        let text = ruum_cli::report(&err);
        assert!(
            !text.contains("connect to"),
            "{url}: refused before any request: {text}"
        );
        assert!(text.contains(url), "{url}: {text}");
    }
    for url in [
        "http://127.0.0.1:9",
        "http://[::1]:9",
        "http://localhost:9",
        "http://ruum.localhost:9",
        "https://ruum.example.com",
    ] {
        ruum_cli::client::check_url(url).unwrap_or_else(|e| panic!("{url}: {e:#}"));
    }
}
