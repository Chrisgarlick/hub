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

    /// Sign in through `/login` (dev sign-in) and keep the cookie.
    async fn sign_in(&mut self) {
        let (s, h, _) = self
            .raw(
                Request::builder()
                    .uri("/login")
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

    // Sign in with Clients: /login goes to Clients, the callback swaps the
    // code once.
    let (s, hd, _) = h
        .raw(
            Request::builder()
                .uri("/login?return=/s/acme")
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
