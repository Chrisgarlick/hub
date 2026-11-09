//! Phase 1 against a real Postgres (`HUB_TEST_DATABASE_URL`, which CI sets):
//! spaces follow a fake Clients, and documents are edited through the API.
//! Each test gets its own database.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt as _;
use hub::app::{AppState, router};
use hub::clients::{ClientInfo, Clients};
use hub::config::Config;
use hub::spaces;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

const ORIGIN: &str = "http://localhost:8160";
const SERVICE_KEY: &str = "ck_hub_service";

// ---------- A fake Clients ----------

#[derive(Default)]
struct FakeData {
    clients: Vec<ClientInfo>,
    events: Vec<Value>,
    /// token → introspection answer
    keys: HashMap<String, Value>,
    codes: Vec<String>,
    introspections: usize,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeData>>);

impl Fake {
    fn add_client(&self, id: &str, slug: &str, name: &str, event: bool) {
        let mut d = self.0.lock().unwrap();
        d.clients.push(ClientInfo {
            id: id.into(),
            slug: slug.into(),
            name: name.into(),
        });
        if event {
            let n = d.events.len() as i64 + 1;
            d.events.push(json!({ "id": n, "type": "client.created", "client_id": id, "payload": { "slug": slug, "name": name } }));
        }
    }

    fn delete_client(&self, id: &str, event: bool) {
        let mut d = self.0.lock().unwrap();
        let slug = d
            .clients
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.slug.clone())
            .unwrap();
        d.clients.retain(|c| c.id != id);
        if event {
            let n = d.events.len() as i64 + 1;
            d.events.push(json!({ "id": n, "type": "client.deleted", "client_id": id, "payload": { "slug": slug } }));
        }
    }

    fn event(&self, kind: &str, payload: Value) {
        let mut d = self.0.lock().unwrap();
        let n = d.events.len() as i64 + 1;
        d.events
            .push(json!({ "id": n, "type": kind, "client_id": null, "payload": payload }));
    }

    async fn serve(&self) -> String {
        fn authed(h: &HeaderMap) -> bool {
            h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
                == Some(&format!("Bearer {SERVICE_KEY}"))
        }
        let app =
            Router::new()
                .route(
                    "/v1/clients",
                    get(|State(f): State<Fake>, h: HeaderMap| async move {
                        if !authed(&h) {
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        Ok(Json(json!(f.0.lock().unwrap().clients)))
                    }),
                )
                .route(
                    "/v1/events",
                    get(
                        |State(f): State<Fake>,
                         h: HeaderMap,
                         Query(q): Query<HashMap<String, i64>>| async move {
                            if !authed(&h) {
                                return Err(StatusCode::UNAUTHORIZED);
                            }
                            let after = q.get("after").copied().unwrap_or(0);
                            let d = f.0.lock().unwrap();
                            Ok(Json(json!(
                                d.events
                                    .iter()
                                    .filter(|e| e["id"].as_i64().unwrap() > after)
                                    .collect::<Vec<_>>()
                            )))
                        },
                    ),
                )
                .route(
                    "/v1/auth/introspect",
                    post(
                        |State(f): State<Fake>, h: HeaderMap, Json(b): Json<Value>| async move {
                            if !authed(&h) {
                                return Err(StatusCode::UNAUTHORIZED);
                            }
                            let mut d = f.0.lock().unwrap();
                            d.introspections += 1;
                            let token = b["token"].as_str().unwrap_or_default();
                            Ok(Json(
                                d.keys
                                    .get(token)
                                    .cloned()
                                    .unwrap_or(json!({ "active": false })),
                            ))
                        },
                    ),
                )
                .route(
                    "/v1/auth/handoff",
                    post(
                        |State(f): State<Fake>, h: HeaderMap, Json(b): Json<Value>| async move {
                            if !authed(&h) {
                                return Err(StatusCode::UNAUTHORIZED);
                            }
                            let mut d = f.0.lock().unwrap();
                            let code = b["code"].as_str().unwrap_or_default().to_string();
                            let ok = b["app"] == "hub" && d.codes.contains(&code);
                            d.codes.retain(|c| *c != code);
                            Ok(Json(json!({ "active": ok })))
                        },
                    ),
                )
                .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }
}

// ---------- Hub under test ----------

struct Hub {
    state: AppState,
    fake: Fake,
    cookie: Option<String>,
    db: (String, String),
}

/// A fresh database and a Hub connected to a fake Clients, or `None` when
/// there's no test database.
async fn hub(dev_login: bool) -> Option<Hub> {
    let Ok(base) = std::env::var("HUB_TEST_DATABASE_URL") else {
        eprintln!("HUB_TEST_DATABASE_URL not set; skipping the database test");
        return None;
    };
    let name = format!(
        "hub_t_{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    );
    let admin = PgPool::connect(&base).await.expect("test database");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let url = match base.rsplit_once('/') {
        Some((server, _)) => format!("{server}/{name}"),
        None => panic!("HUB_TEST_DATABASE_URL needs a database name"),
    };
    let pool = hub::db::connect(&url).await.expect("connect and migrate");

    let fake = Fake::default();
    let clients_url = fake.serve().await;
    let config = Config::parse(&format!(
        "database_url = \"{url}\"\npublic_origin = \"{ORIGIN}\"\nclients_url = \"{clients_url}\"\n\
         clients_service_key = \"{SERVICE_KEY}\"\nclients_public_url = \"https://clients.example\"\ndev_login = {dev_login}"
    ))
    .unwrap();
    let state = AppState::new(config, pool);
    spaces::ensure_studio(&state.pool).await.unwrap();
    Some(Hub {
        state,
        fake,
        cookie: None,
        db: (base, name),
    })
}

impl Hub {
    fn clients(&self) -> &Clients {
        self.state.clients.as_ref().unwrap()
    }

    async fn reconcile(&self) -> usize {
        let list = self.clients().clients().await.unwrap();
        spaces::reconcile(&self.state.pool, &list).await.unwrap()
    }

    async fn sync(&self) -> usize {
        spaces::sync_events(&self.state.pool, self.clients())
            .await
            .unwrap()
    }

    async fn raw(&self, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let res = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// A request signed in with the session cookie (sending `Origin`).
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::ORIGIN, ORIGIN);
        if let Some(c) = &self.cookie {
            req = req.header(header::COOKIE, c);
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        };
        let (s, _, v) = self.raw(req.unwrap()).await;
        (s, v)
    }

    async fn with_key(&self, method: &str, uri: &str, key: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        let (s, _, v) = self.raw(req).await;
        (s, v)
    }

    async fn key_call(
        &self,
        method: &str,
        uri: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (s, _, v) = self.raw(req).await;
        (s, v)
    }

    /// Sign in through `/auth/clients` (dev sign-in) and keep the cookie.
    async fn sign_in(&mut self) {
        let (s, h, _) = self
            .raw(
                Request::builder()
                    .uri("/auth/clients")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert!(s.is_redirection(), "{s}");
        self.cookie = Some(cookie(&h));
    }

    async fn done(self) {
        self.state.pool.close().await;
        let admin = PgPool::connect(&self.db.0).await.unwrap();
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.db.1
        )))
        .execute(&admin)
        .await;
    }
}

fn cookie(h: &HeaderMap) -> String {
    let set = h
        .get(header::SET_COOKIE)
        .expect("a session cookie")
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

fn paths(space: &Value) -> Vec<String> {
    let mut p: Vec<String> = space["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap().to_string())
        .collect();
    p.sort();
    p
}

// ---------- Tests ----------

#[tokio::test]
async fn spaces_follow_clients() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;

    // Created in Clients: the event makes the space, with starter documents.
    h.fake.add_client("cl_1", "acme", "Acme", true);
    assert_eq!(h.sync().await, 1);
    let (s, acme) = h.call("GET", "/api/spaces/acme", None).await;
    assert_eq!(s, StatusCode::OK, "{acme}");
    assert_eq!(acme["space"]["name"], "Acme");
    assert_eq!(
        paths(&acme),
        [
            "audience.md",
            "brief.md",
            "constraints.md",
            "voice.md",
            "writing-rules.md"
        ]
    );

    // Events are idempotent: replaying the feed changes nothing.
    sqlx::query("DELETE FROM sync_state")
        .execute(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(h.sync().await, 1);
    assert_eq!(h.reconcile().await, 0);
    let (_, again) = h.call("GET", "/api/spaces/acme", None).await;
    assert_eq!(paths(&again), paths(&acme));

    // A missed event: reconcile finds the client.
    h.fake.add_client("cl_2", "zaltek", "Zaltek", false);
    assert_eq!(h.sync().await, 0);
    assert_eq!(h.reconcile().await, 1);
    let (_, list) = h.call("GET", "/api/spaces", None).await;
    let slugs: Vec<&str> = list["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        slugs,
        ["_studio", "acme", "zaltek"],
        "the studio first, then clients by name"
    );
    assert_eq!(list["spaces"][1]["documents"], 5);

    // Deleted in Clients: archived, its documents kept, and the slug free.
    h.fake.delete_client("cl_1", true);
    assert_eq!(h.sync().await, 1);
    assert_eq!(
        h.call("GET", "/api/spaces/acme", None).await.0,
        StatusCode::NOT_FOUND
    );
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM documents WHERE space_id = 'cl_1'")
        .fetch_one(&h.state.pool)
        .await
        .unwrap();
    assert_eq!(kept, 5);
    h.fake.add_client("cl_3", "acme", "Acme Ltd", true);
    assert_eq!(h.sync().await, 1);
    let (_, acme) = h.call("GET", "/api/spaces/acme", None).await;
    assert_eq!(acme["space"]["id"], "cl_3");

    // A missed delete is archived by reconcile, freeing the slug.
    h.fake.delete_client("cl_2", false);
    h.fake.add_client("cl_4", "zaltek", "Zaltek", false);
    assert_eq!(h.reconcile().await, 1);
    let (_, z) = h.call("GET", "/api/spaces/zaltek", None).await;
    assert_eq!(z["space"]["id"], "cl_4");

    // The studio has its own starter documents.
    let (_, studio) = h.call("GET", "/api/spaces/_studio", None).await;
    assert_eq!(paths(&studio), ["house-style.md", "writing-rules.md"]);
    h.done().await;
}

#[tokio::test]
async fn documents_edit_history_and_trash() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;

    let (s, d) = h
        .call(
            "POST",
            "/api/spaces/acme/documents",
            Some(json!({ "path": "research/Interviews.md", "content": "---\ntype: research\n---\n# Interviews\n\n## Guests\n\nThey want late checkout.\n" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    assert_eq!(
        (d["path"].as_str(), d["title"].as_str(), d["type"].as_str()),
        (
            Some("research/interviews.md"),
            Some("Interviews"),
            Some("research")
        )
    );
    assert_eq!(d["visibility"], "internal");
    assert_eq!(d["headings"][1]["anchor"], "guests");
    let id = d["id"].as_str().unwrap().to_string();

    // The same path again is refused.
    let (s, e) = h
        .call(
            "POST",
            "/api/spaces/acme/documents",
            Some(json!({ "path": "research/interviews.md" })),
        )
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid"))
    );

    // Edit: anchors survive an edit that keeps the heading.
    let (s, d) = h
        .call(
            "PATCH",
            &format!("/api/documents/{id}"),
            Some(json!({ "version": 1, "content": "# Interviews\n\n## Guests\n\nThey want late checkout and early check-in.\n" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["version"], 2);
    assert_eq!(d["type"], "note", "the frontmatter was removed");
    assert_eq!(d["headings"][1]["anchor"], "guests");

    // A stale version is a conflict that carries the current document.
    let (s, e) = h
        .call(
            "PATCH",
            &format!("/api/documents/{id}"),
            Some(json!({ "version": 1, "content": "x" })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(e["current"]["version"], 2);

    // Edits by the same person close together are one revision; another
    // author starts a new one.
    let revs = |v: Value| v["revisions"].as_array().unwrap().len();
    let (_, r) = h
        .call("GET", &format!("/api/documents/{id}/revisions"), None)
        .await;
    assert_eq!(revs(r.clone()), 1, "{r}");
    let first = r["revisions"][0]["id"].as_i64().unwrap();
    let mut conn = h.state.pool.acquire().await.unwrap();
    hub::documents::update(
        &mut conn,
        &id,
        serde_json::from_value(
            json!({ "version": 2, "content": "# Interviews\n\nRewritten by Claude.\n" }),
        )
        .unwrap(),
        "claude-code",
    )
    .await
    .unwrap();
    drop(conn);
    let (_, r) = h
        .call("GET", &format!("/api/documents/{id}/revisions"), None)
        .await;
    assert_eq!(revs(r.clone()), 2, "{r}");

    // Restore the first revision: a new version with the old content.
    let (s, d) = h
        .call(
            "POST",
            &format!("/api/documents/{id}/restore/{first}"),
            Some(json!({ "version": 3 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["version"], 4);
    assert!(d["content"].as_str().unwrap().contains("early check-in"));

    // Rename.
    let (_, d) = h
        .call(
            "PATCH",
            &format!("/api/documents/{id}"),
            Some(json!({ "version": 4, "path": "research/guests.md" })),
        )
        .await;
    assert_eq!(d["path"], "research/guests.md");

    // Trash, and back.
    let (s, _) = h
        .call("DELETE", &format!("/api/documents/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.call("GET", &format!("/api/documents/{id}"), None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, trash) = h.call("GET", "/api/spaces/acme/trash", None).await;
    assert_eq!(trash["documents"][0]["id"], id.as_str());
    let (_, acme) = h.call("GET", "/api/spaces/acme", None).await;
    assert!(!paths(&acme).contains(&"research/guests.md".to_string()));

    // A new document took its path meanwhile: undelete is refused.
    h.call(
        "POST",
        "/api/spaces/acme/documents",
        Some(json!({ "path": "research/guests.md" })),
    )
    .await;
    let (s, e) = h
        .call("POST", &format!("/api/documents/{id}/undelete"), None)
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("path_taken"))
    );
    let (_, acme) = h.call("GET", "/api/spaces/acme", None).await;
    let other = acme["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["path"] == "research/guests.md")
        .unwrap()["id"]
        .clone();
    h.call(
        "DELETE",
        &format!("/api/documents/{}", other.as_str().unwrap()),
        None,
    )
    .await;
    let (s, d) = h
        .call("POST", &format!("/api/documents/{id}/undelete"), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert!(d["deleted_at"].is_null());
    h.done().await;
}

#[tokio::test]
async fn who_may_call() {
    let Some(mut h) = hub(false).await else {
        return;
    };
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.fake.add_client("cl_2", "zaltek", "Zaltek", true);
    h.sync().await;
    {
        let mut d = h.fake.0.lock().unwrap();
        d.keys.insert("ck_claude".into(), json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }));
        d.keys.insert("ck_acme".into(), json!({ "active": true, "id": "k2", "key": "acme-bot", "kind": "caller", "clients": ["acme"] }));
        d.keys.insert("ck_svc".into(), json!({ "active": true, "id": "k3", "key": "motion", "kind": "service", "clients": null }));
        d.codes.push("code-1".into());
    }

    // Not signed in.
    assert_eq!(
        h.call("GET", "/api/spaces", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.with_key("GET", "/api/spaces", "ck_nope").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.with_key("GET", "/api/spaces", "ck_svc").await.0,
        StatusCode::FORBIDDEN,
        "service keys aren't callers"
    );

    // A caller key sees every space; one limited to a client sees that (and
    // the studio) only.
    let (_, me) = h.with_key("GET", "/api/me", "ck_claude").await;
    assert_eq!(me["actor"], "claude-code");
    let (_, list) = h.with_key("GET", "/api/spaces", "ck_acme").await;
    let slugs: Vec<&str> = list["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["_studio", "acme"]);
    assert_eq!(
        h.with_key("GET", "/api/spaces/zaltek", "ck_acme").await.0,
        StatusCode::NOT_FOUND
    );
    let (_, z) = h.with_key("GET", "/api/spaces/zaltek", "ck_claude").await;
    let zid = z["documents"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        h.with_key("GET", &format!("/api/documents/{zid}"), "ck_acme")
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Key checks are cached; a revoked key is dropped from the cache.
    let before = h.fake.0.lock().unwrap().introspections;
    h.with_key("GET", "/api/me", "ck_acme").await;
    assert_eq!(h.fake.0.lock().unwrap().introspections, before);
    h.fake.0.lock().unwrap().keys.remove("ck_acme");
    h.fake.event("key.revoked", json!({ "key": "k2" }));
    h.sync().await;
    assert_eq!(
        h.with_key("GET", "/api/me", "ck_acme").await.0,
        StatusCode::UNAUTHORIZED
    );

    // Sign in with Clients: /auth/clients goes to Clients, the callback swaps the
    // code once.
    let (s, hd, _) = h
        .raw(
            Request::builder()
                .uri("/auth/clients?return=/s/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert!(s.is_redirection());
    assert_eq!(
        hd[header::LOCATION],
        "https://clients.example/handoff?app=hub&return=/s/acme"
    );
    let (s, hd, _) = h
        .raw(
            Request::builder()
                .uri("/auth/callback?code=code-1&return=//evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert!(s.is_redirection());
    assert_eq!(hd[header::LOCATION], "/");
    h.cookie = Some(cookie(&hd));
    assert_eq!(h.call("GET", "/api/me", None).await.1["actor"], "user");
    let (s, _, _) = h
        .raw(
            Request::builder()
                .uri("/auth/callback?code=code-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "codes are single use");

    // Cookie writes need Hub's own Origin.
    let req = Request::builder()
        .method("POST")
        .uri("/api/spaces/acme/documents")
        .header(header::COOKIE, h.cookie.clone().unwrap())
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "path": "x.md" }).to_string()))
        .unwrap();
    assert_eq!(h.raw(req).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        h.call(
            "POST",
            "/api/spaces/acme/documents",
            Some(json!({ "path": "x.md" }))
        )
        .await
        .0,
        StatusCode::CREATED
    );

    // Signing out ends the session.
    let req = Request::builder()
        .method("POST")
        .uri("/logout")
        .header(header::COOKIE, h.cookie.clone().unwrap())
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.raw(req).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        h.call("GET", "/api/me", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    h.done().await;
}

// ---------- Passkeys ----------

/// A simulated authenticator: a real P-256 key, real CBOR and signatures.
struct Authenticator {
    key: p256::ecdsa::SigningKey,
    id: Vec<u8>,
}

fn b64(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

impl Authenticator {
    fn new(seed: u8) -> Self {
        Self {
            key: p256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap(),
            id: vec![seed; 16],
        }
    }

    fn auth_data(&self, attested: bool) -> Vec<u8> {
        use ciborium::Value as Cbor;
        use sha2::Digest as _;
        let mut d = sha2::Sha256::digest(b"localhost").to_vec();
        d.push(0x01 | 0x04 | if attested { 0x40 } else { 0 });
        d.extend_from_slice(&0u32.to_be_bytes());
        if attested {
            let point = self.key.verifying_key().to_sec1_point(false);
            let xy = point.as_bytes();
            let cose = Cbor::Map(vec![
                (Cbor::from(1), Cbor::from(2)),
                (Cbor::from(3), Cbor::from(-7)),
                (Cbor::from(-1), Cbor::from(1)),
                (Cbor::from(-2), Cbor::Bytes(xy[1..33].to_vec())),
                (Cbor::from(-3), Cbor::Bytes(xy[33..65].to_vec())),
            ]);
            d.extend_from_slice(&[0u8; 16]);
            d.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
            d.extend_from_slice(&self.id);
            ciborium::into_writer(&cose, &mut d).unwrap();
        }
        d
    }

    fn client(kind: &str, challenge: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({ "type": kind, "challenge": challenge, "origin": ORIGIN }))
            .unwrap()
    }

    fn create(&self, options: &Value) -> Value {
        use ciborium::Value as Cbor;
        let att = Cbor::Map(vec![
            (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
            (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
            (
                Cbor::Text("authData".into()),
                Cbor::Bytes(self.auth_data(true)),
            ),
        ]);
        let mut att_bytes = Vec::new();
        ciborium::into_writer(&att, &mut att_bytes).unwrap();
        let client = Self::client("webauthn.create", options["challenge"].as_str().unwrap());
        json!({ "name": "iPhone", "clientDataJSON": b64(&client), "attestationObject": b64(&att_bytes) })
    }

    fn get(&self, options: &Value) -> Value {
        use p256::ecdsa::signature::Signer as _;
        use sha2::Digest as _;
        let data = self.auth_data(false);
        let client = Self::client("webauthn.get", options["challenge"].as_str().unwrap());
        let mut msg = data.clone();
        msg.extend_from_slice(&sha2::Sha256::digest(&client));
        let sig: p256::ecdsa::Signature = self.key.sign(&msg);
        json!({ "id": b64(&self.id), "clientDataJSON": b64(&client), "authenticatorData": b64(&data),
                "signature": b64(sig.to_der().as_bytes()) })
    }
}

#[tokio::test]
async fn a_passkey_signs_you_in() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    let first = h.cookie.clone();
    let phone = Authenticator::new(3);

    // Add one, signed in.
    let (s, options) = h.call("POST", "/api/passkeys/options", None).await;
    assert_eq!(s, StatusCode::OK, "{options}");
    assert_eq!(options["rp"]["id"], "localhost");
    assert_eq!(options["rp"]["name"], "Hub");
    let (s, added) = h
        .call("POST", "/api/passkeys", Some(phone.create(&options)))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{added}");
    let (_, list) = h.call("GET", "/api/passkeys", None).await;
    assert_eq!(list["passkeys"][0]["name"], "iPhone");
    let (_, again) = h.call("POST", "/api/passkeys/options", None).await;
    assert_eq!(again["excludeCredentials"][0]["id"], b64(&phone.id));

    // Sign in with it, from a browser with no session.
    h.cookie = None;
    let (s, options) = h.call("POST", "/auth/passkey/options", None).await;
    assert_eq!(s, StatusCode::OK);
    let assertion = phone.get(&options);
    let req = Request::builder()
        .method("POST")
        .uri("/auth/passkey/login")
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(assertion.to_string()))
        .unwrap();
    let (s, hd, body) = h.raw(req).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    h.cookie = Some(cookie(&hd));
    let (_, me) = h.call("GET", "/api/me", None).await;
    assert_eq!(
        (me["actor"].as_str(), me["session"].as_bool()),
        (Some("user"), Some(true))
    );

    // The same signed response can't be used twice, and another key can't
    // claim this passkey's id.
    let (s, _) = h.call("POST", "/auth/passkey/login", Some(assertion)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, options) = h.call("POST", "/auth/passkey/options", None).await;
    let mut forged = Authenticator::new(4).get(&options);
    forged["id"] = json!(b64(&phone.id));
    assert_eq!(
        h.call("POST", "/auth/passkey/login", Some(forged)).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A removed passkey can't sign in.
    h.cookie = first;
    let id = list["passkeys"][0]["id"].as_str().unwrap();
    assert_eq!(
        h.call("DELETE", &format!("/api/passkeys/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, options) = h.call("POST", "/auth/passkey/options", None).await;
    let (s, e) = h
        .call("POST", "/auth/passkey/login", Some(phone.get(&options)))
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("unknown_passkey"))
    );
    h.done().await;
}

#[tokio::test]
async fn keys_and_other_sites_cant_use_passkeys() {
    let Some(mut h) = hub(true).await else { return };
    h.fake.0.lock().unwrap().keys.insert(
        "ck_claude".into(),
        json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }),
    );
    // A Clients key can't add a passkey (it would turn a key into a login).
    assert_eq!(
        h.with_key("POST", "/api/passkeys/options", "ck_claude")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.with_key("GET", "/api/passkeys", "ck_claude").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.call("GET", "/api/passkeys", None).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Sign-in requests must come from Hub's own pages.
    let req = Request::builder()
        .method("POST")
        .uri("/auth/passkey/options")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.raw(req).await.0, StatusCode::FORBIDDEN);

    // A registration signed for a challenge Hub never issued is refused.
    h.sign_in().await;
    let fake = Authenticator::new(5).create(&json!({ "challenge": "made-up" }));
    let (s, e) = h.call("POST", "/api/passkeys", Some(fake)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    h.done().await;
}

// ---------- Rules and decisions ----------

/// The id of the document at `path` in a space.
async fn doc_id(h: &Hub, slug: &str, path: &str) -> String {
    let (_, sp) = h.call("GET", &format!("/api/spaces/{slug}"), None).await;
    sp["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["path"] == path)
        .unwrap_or_else(|| panic!("no {path}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn write(h: &Hub, id: &str, content: &str) -> Value {
    let (_, d) = h.call("GET", &format!("/api/documents/{id}"), None).await;
    let (s, d) = h
        .call(
            "PATCH",
            &format!("/api/documents/{id}"),
            Some(json!({ "version": d["version"], "content": content })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    d
}

async fn check(h: &Hub, text: &str, target: &str) -> Value {
    let (s, v) = h
        .call(
            "POST",
            "/api/spaces/acme/check",
            Some(json!({ "text": text, "for": target })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn rule_texts(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            f["rule"]["text"]
                .as_str()
                .or(f["text"].as_str())
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn writing_rules_become_checks() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;
    h.fake.0.lock().unwrap().keys.insert(
        "ck_claude".into(),
        json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }),
    );

    // You write the rules in prose.
    let wr = doc_id(&h, "acme", "writing-rules.md").await;
    write(
        &h,
        &wr,
        "---\ntype: writing-rules\n---\n# Writing rules\n\n## Everywhere\n\n\
         - Don't mention the \"data breach\".\n- Keep it warm.\n\n\
         ## Reports and proposals\n\n- Always end with \"The Acme team\".\n- Avoid \"synergy\".\n",
    )
    .await;
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    let list = rules["rules"].as_array().unwrap();
    assert_eq!(list.len(), 4, "{rules}");
    assert_eq!(list[0]["hard"], true, "hard rules first");
    assert!(list.iter().all(|r| r["standing"] == "authoritative"));
    let breach_id = list.iter().find(|r| r["kind"] == "must_not").unwrap()["id"].clone();

    // check flags what you said not to, with where; and what's missing.
    let draft = "Dear Bob,\nAfter the data breach we promise synergy.\nThanks";
    let v = check(&h, draft, "typeset").await;
    assert_eq!(v["ok"], false);
    assert_eq!(
        rule_texts(&v, "violations"),
        [
            "Don't mention the \"data breach\".",
            "Always end with \"The Acme team\"."
        ]
    );
    let span = &v["violations"][0]["spans"][0];
    assert_eq!(
        (
            span["line"].as_i64(),
            span["column"].as_i64(),
            span["text"].as_str()
        ),
        (Some(2), Some(11), Some("data breach"))
    );
    assert_eq!(v["violations"][1]["missing"][0], "The Acme team");
    assert_eq!(rule_texts(&v, "advice"), ["Avoid \"synergy\"."]);
    assert_eq!(rule_texts(&v, "guidance"), ["Keep it warm."]);
    // Typeset's rules don't apply to a design.
    let v = check(&h, draft, "design").await;
    assert_eq!(
        rule_texts(&v, "violations"),
        ["Don't mention the \"data breach\"."]
    );

    // Editing in place keeps the rule's id.
    let d = write(
        &h,
        &wr,
        "---\ntype: writing-rules\n---\n# Writing rules\n\n## Everywhere\n\n\
         - Don't mention the \"data breach\" or a \"leak\".\n- Keep it warm.\n",
    )
    .await;
    assert_eq!(d["version"], 3);
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    assert_eq!(
        rules["rules"].as_array().unwrap().len(),
        2,
        "removed ones are retired"
    );
    assert_eq!(rules["rules"][0]["id"], breach_id);
    assert_eq!(
        rules["rules"][0]["check"]["terms"],
        json!(["data breach", "leak"])
    );

    // A rule Claude adds (through the document, or directly) waits for you.
    let (_, d) = h.call("GET", &format!("/api/documents/{wr}"), None).await;
    let content = format!("{}- Never use \"cheap\".\n", d["content"].as_str().unwrap());
    let (s, _) = h
        .key_call(
            "PATCH",
            &format!("/api/documents/{wr}"),
            "ck_claude",
            json!({ "version": d["version"], "content": content }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, direct) = h
        .key_call(
            "POST",
            "/api/spaces/acme/rules",
            "ck_claude",
            json!({ "kind": "avoid", "text": "Avoid jargon", "check": { "terms": ["leverage"] } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{direct}");
    assert_eq!(direct["standing"], "tentative");
    let v = check(&h, "It's cheap. We leverage things.", "all").await;
    assert_eq!(v["ok"], true, "proposed rules aren't enforced: {v}");
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    let cheap = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["text"] == "Never use \"cheap\".")
        .unwrap()
        .clone();
    assert_eq!(cheap["status"], "proposed");
    assert_eq!(cheap["created_by"], "claude-code");
    let path = format!("/api/rules/{}/approve", cheap["id"].as_str().unwrap());
    assert_eq!(
        h.with_key("POST", &path, "ck_claude").await.0,
        StatusCode::FORBIDDEN,
        "keys can't approve"
    );
    assert_eq!(h.call("POST", &path, None).await.0, StatusCode::OK);
    let path = format!("/api/rules/{}/approve", direct["id"].as_str().unwrap());
    assert_eq!(h.call("POST", &path, None).await.0, StatusCode::OK);
    let v = check(&h, "It's cheap. We leverage things.", "all").await;
    assert_eq!(rule_texts(&v, "violations"), ["Never use \"cheap\"."]);
    assert_eq!(rule_texts(&v, "advice"), ["Avoid jargon"]);

    // Document rules are changed in the document.
    let (s, e) = h
        .call(
            "DELETE",
            &format!("/api/rules/{}", cheap["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("from_document"))
    );

    // Studio rules apply to every client, unless switched off for one.
    let studio = doc_id(&h, "_studio", "writing-rules.md").await;
    write(
        &h,
        &studio,
        "---\ntype: writing-rules\n---\n## Everywhere\n\n- Don't say \"solutions\".\n",
    )
    .await;
    let v = check(&h, "We sell solutions.", "all").await;
    assert_eq!(rule_texts(&v, "violations"), ["Don't say \"solutions\"."]);
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    let sol = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["scope"] == "studio")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/spaces/acme/rules/{sol}/exception");
    let (s, _) = h
        .call(
            "PUT",
            &path,
            Some(json!({ "reason": "It's their product name" })),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(check(&h, "We sell solutions.", "all").await["ok"], true);
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    let off = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == sol.as_str())
        .unwrap()
        .clone();
    assert_eq!(off["excepted"]["reason"], "It's their product name");
    h.call("DELETE", &path, None).await;
    assert_eq!(check(&h, "We sell solutions.", "all").await["ok"], false);

    // A Writing rules document in the trash has no rules in force.
    h.call("DELETE", &format!("/api/documents/{wr}"), None)
        .await;
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    assert!(
        rules["rules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["scope"] == "studio" || r["source_doc"].is_null())
    );
    h.done().await;
}

#[tokio::test]
async fn decisions_override_sections() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;
    h.fake.0.lock().unwrap().keys.insert(
        "ck_claude".into(),
        json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }),
    );
    let constraints = doc_id(&h, "acme", "constraints.md").await;
    write(
        &h,
        &constraints,
        "# Constraints\n\n## Accounts\n\nEveryone needs an account to buy.\n",
    )
    .await;

    // A section that isn't there is refused.
    let (s, e) = h
        .call(
            "POST",
            "/api/spaces/acme/decisions",
            Some(json!({ "statement": "x", "reason": "y", "contradicts": [{ "doc": "constraints.md", "anchor": "nope" }] })),
        )
        .await;
    assert_eq!(
        (s, e["path"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("contradicts[0]"))
    );

    // You record a decision: it's approved, and the section shows it.
    let (s, d) = h
        .call(
            "POST",
            "/api/spaces/acme/decisions",
            Some(json!({
                "statement": "Guests can buy without an account.",
                "reason": "Two thirds of orders are gifts from one-off buyers.",
                "contradicts": [{ "doc": "constraints.md", "anchor": "#accounts" }],
                "applies_to": ["board", "design"],
                "evidence": [{ "app": "board", "url": "https://board.kritano.com/boards/01ABC", "item": "card1" }],
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    assert_eq!(
        (d["status"].as_str(), d["standing"].as_str()),
        (Some("approved"), Some("authoritative"))
    );
    let first = d["id"].as_str().unwrap().to_string();
    let (_, doc) = h
        .call("GET", &format!("/api/documents/{constraints}"), None)
        .await;
    assert_eq!(doc["overridden_by"][0]["anchor"], "accounts");
    assert_eq!(doc["overridden_by"][0]["decision"]["id"], first.as_str());

    // check hands decisions for the target to Claude.
    assert_eq!(
        check(&h, "hello", "design").await["decisions"][0]["id"],
        first.as_str()
    );
    assert_eq!(check(&h, "hello", "typeset").await["decisions"], json!([]));

    // Claude proposes a decision that supersedes it: it waits for you.
    let (s, p) = h
        .key_call(
            "POST",
            "/api/spaces/acme/decisions",
            "ck_claude",
            json!({
                "statement": "Guests can buy, but are asked to make an account after.",
                "reason": "Repeat orders doubled when accounts were offered after checkout.",
                "contradicts": [{ "doc": constraints.as_str(), "anchor": "accounts" }],
                "supersedes": first.as_str(),
            }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["standing"], "tentative");
    let second = p["id"].as_str().unwrap().to_string();
    let (_, doc) = h
        .call("GET", &format!("/api/documents/{constraints}"), None)
        .await;
    assert_eq!(
        doc["overridden_by"].as_array().unwrap().len(),
        1,
        "only approved decisions override"
    );
    let approve = format!("/api/decisions/{second}/approve");
    assert_eq!(
        h.with_key("POST", &approve, "ck_claude").await.0,
        StatusCode::FORBIDDEN
    );
    let (s, _) = h.call("POST", &approve, None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, old) = h
        .call("GET", &format!("/api/decisions/{first}"), None)
        .await;
    assert_eq!(
        (old["status"].as_str(), old["standing"].as_str()),
        (Some("superseded"), Some("outdated"))
    );
    assert_eq!(old["superseded_by"], second.as_str());
    let (_, doc) = h
        .call("GET", &format!("/api/documents/{constraints}"), None)
        .await;
    assert_eq!(doc["overridden_by"][0]["decision"]["id"], second.as_str());

    // A rejected proposal, and the generated Decisions document.
    let (_, r) = h
        .key_call(
            "POST",
            "/api/spaces/acme/decisions",
            "ck_claude",
            json!({ "statement": "Drop the blog.", "reason": "Nobody reads it." }),
        )
        .await;
    let (s, r) = h
        .call(
            "POST",
            &format!("/api/decisions/{}/reject", r["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(
        (s, r["status"].as_str()),
        (StatusCode::OK, Some("rejected"))
    );
    let (_, md) = h.call("GET", "/api/spaces/acme/decisions.md", None).await;
    let md = md["markdown"].as_str().unwrap();
    assert!(md.starts_with("# Decisions: Acme"), "{md}");
    let in_force = md.find("## In force").unwrap();
    let gone = md.find("## No longer in force").unwrap();
    assert!(md[in_force..gone].contains("asked to make an account after"));
    assert!(md[gone..].contains("Drop the blog."));
    assert!(md.contains("Overrides `constraints.md#accounts`"));
    h.done().await;
}
