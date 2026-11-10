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
                    })
                    .post(|State(f): State<Fake>, h: HeaderMap, Json(b): Json<Value>| async move {
                        if !authed(&h) {
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        let slug = b["slug"].as_str().unwrap_or_default().to_string();
                        let name = b["name"].as_str().unwrap_or_default().to_string();
                        if f.0.lock().unwrap().clients.iter().any(|c| c.slug == slug) {
                            return Err(StatusCode::CONFLICT);
                        }
                        let id = format!("cl_{slug}");
                        f.add_client(&id, &slug, &name, true);
                        Ok((StatusCode::CREATED, Json(json!({ "id": id, "slug": slug, "name": name }))))
                    }),
                )
                .route(
                    "/v1/clients/{slug}",
                    axum::routing::patch(
                        |State(f): State<Fake>, h: HeaderMap, axum::extract::Path(slug): axum::extract::Path<String>, Json(b): Json<Value>| async move {
                            if !authed(&h) {
                                return Err(StatusCode::UNAUTHORIZED);
                            }
                            let mut d = f.0.lock().unwrap();
                            let name = b["name"].as_str().unwrap_or_default().to_string();
                            let c = d.clients.iter_mut().find(|c| c.slug == slug).ok_or(StatusCode::NOT_FOUND)?;
                            c.name = name.clone();
                            let id = c.id.clone();
                            let n = d.events.len() as i64 + 1;
                            d.events.push(json!({ "id": n, "type": "client.updated", "client_id": id, "payload": { "slug": slug, "name": name } }));
                            Ok(Json(json!({ "id": id, "slug": slug, "name": name })))
                        },
                    )
                    .delete(
                        |State(f): State<Fake>, h: HeaderMap, axum::extract::Path(slug): axum::extract::Path<String>| async move {
                            if !authed(&h) {
                                return Err(StatusCode::UNAUTHORIZED);
                            }
                            let id = f.0.lock().unwrap().clients.iter().find(|c| c.slug == slug).map(|c| c.id.clone());
                            let id = id.ok_or(StatusCode::NOT_FOUND)?;
                            f.delete_client(&id, true);
                            Ok(StatusCode::NO_CONTENT)
                        },
                    ),
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
    hub_with(dev_login, "").await
}

/// As [`hub`], with extra lines for `hub.toml`.
async fn hub_with(dev_login: bool, extra: &str) -> Option<Hub> {
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
         clients_service_key = \"{SERVICE_KEY}\"\nclients_public_url = \"https://clients.example\"\ndev_login = {dev_login}\n{extra}"
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

/// Claude (ck_claude) proposes something for acme.
async fn propose(h: &Hub, kind: &str, change: Value, rationale: &str) -> Value {
    let (s, p) = h
        .key_call(
            "POST",
            "/api/spaces/acme/proposals",
            "ck_claude",
            json!({ "kind": kind, "change": change, "rationale": rationale }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    p
}

/// Make these kinds of Claude's changes wait for you, for every client.
async fn wait_for(h: &Hub, kinds: &[&str]) {
    let policies: Vec<Value> = kinds
        .iter()
        .map(|k| json!({ "space": "*", "kind": k, "mode": "review" }))
        .collect();
    let (s, v) = h
        .call(
            "PATCH",
            "/api/settings/approvals",
            Some(json!({ "policies": policies })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// You approve a proposal in the UI.
async fn approve(h: &Hub, p: &Value) -> Value {
    let (s, v) = h
        .call(
            "POST",
            &format!("/api/proposals/{}/approve", p["id"].as_str().unwrap()),
            None,
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

    // A rule Claude adds (through the document, or directly) waits for you,
    // when you've set those kinds to wait.
    wait_for(&h, &["doc_edit", "rule"]).await;
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
    assert_eq!(s, StatusCode::FORBIDDEN, "keys propose; they don't write");
    let edit = propose(
        &h,
        "doc_edit",
        json!({ "document": "writing-rules.md", "version": d["version"], "content": content }),
        "The client asked us not to say cheap.",
    )
    .await;
    assert_eq!(edit["status"], "pending");
    assert!(
        edit["diff"]
            .as_str()
            .unwrap()
            .contains("+- Never use \"cheap\"."),
        "{edit}"
    );
    let direct = propose(
        &h,
        "rule",
        json!({ "kind": "avoid", "text": "Avoid jargon", "check": { "terms": ["leverage"] } }),
        "From the kick-off call.",
    )
    .await;
    let v = check(&h, "It's cheap. We leverage things.", "all").await;
    assert_eq!(v["ok"], true, "nothing applies until you approve: {v}");
    let path = format!("/api/proposals/{}/approve", edit["id"].as_str().unwrap());
    assert_eq!(
        h.with_key("POST", &path, "ck_claude").await.0,
        StatusCode::FORBIDDEN,
        "keys can't approve over the API"
    );
    assert_eq!(approve(&h, &edit).await["status"], "applied");
    assert_eq!(approve(&h, &direct).await["status"], "applied");
    let v = check(&h, "It's cheap. We leverage things.", "all").await;
    assert_eq!(rule_texts(&v, "violations"), ["Never use \"cheap\"."]);
    assert_eq!(rule_texts(&v, "advice"), ["Avoid jargon"]);
    let (_, rules) = h.call("GET", "/api/spaces/acme/rules", None).await;
    let cheap = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["text"] == "Never use \"cheap\".")
        .unwrap()
        .clone();
    assert_eq!(
        (cheap["status"].as_str(), cheap["standing"].as_str()),
        (Some("approved"), Some("authoritative")),
        "you approved it"
    );

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

    // Claude proposes a decision that supersedes it: set to wait, it waits.
    wait_for(&h, &["decision"]).await;
    let p = propose(
        &h,
        "decision",
        json!({
            "statement": "Guests can buy, but are asked to make an account after.",
            "reason": "Repeat orders doubled when accounts were offered after checkout.",
            "contradicts": [{ "doc": constraints.as_str(), "anchor": "accounts" }],
            "supersedes": first.as_str(),
        }),
        "Repeat orders doubled.",
    )
    .await;
    assert_eq!(p["status"], "pending");
    let (_, doc) = h
        .call("GET", &format!("/api/documents/{constraints}"), None)
        .await;
    assert_eq!(
        doc["overridden_by"].as_array().unwrap().len(),
        1,
        "only approved decisions override"
    );
    let done = approve(&h, &p).await;
    let second = done["result"]["decision"].as_str().unwrap().to_string();
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
    let r = propose(
        &h,
        "decision",
        json!({ "statement": "Drop the blog.", "reason": "Nobody reads it." }),
        "Nobody reads it.",
    )
    .await;
    let (s, r) = h
        .call(
            "POST",
            &format!("/api/proposals/{}/reject", r["id"].as_str().unwrap()),
            Some(json!({ "note": "We need it for SEO" })),
        )
        .await;
    assert_eq!(
        (s, r["status"].as_str(), r["note"].as_str()),
        (StatusCode::OK, Some("rejected"), Some("We need it for SEO"))
    );
    let (_, md) = h.call("GET", "/api/spaces/acme/decisions.md", None).await;
    let md = md["markdown"].as_str().unwrap();
    assert!(md.starts_with("# Decisions: Acme"), "{md}");
    let in_force = md.find("## In force").unwrap();
    let gone = md.find("## No longer in force").unwrap();
    assert!(md[in_force..gone].contains("asked to make an account after"));
    assert!(
        !md.contains("Drop the blog."),
        "a rejected proposal never became a decision"
    );
    assert!(md.contains("Overrides `constraints.md#accounts`"));
    h.done().await;
}

// ---------- Search ----------

async fn index(h: &Hub) {
    hub::index::run_once(&h.state.pool, h.state.embedder.as_ref())
        .await
        .unwrap();
}

async fn find(h: &Hub, query: &str) -> Vec<Value> {
    let q: String = query
        .chars()
        .map(|c| if c == ' ' { '+' } else { c })
        .collect();
    let q = q.replace('?', "%3F");
    let (s, v) = h
        .call("GET", &format!("/api/search?q={q}&space=acme"), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["results"].as_array().unwrap().clone()
}

#[tokio::test]
async fn search_finds_sections_and_what_overrides_them() {
    let Some(mut h) = hub_with(true, "embedding_provider = \"fake\"").await else {
        return;
    };
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
        "# Constraints\n\n## Accounts\n\nGuests must make an account before they can check out.\n\n\
         ## Deadlines\n\nThe new site is live by March.\n",
    )
    .await;
    let (s, _) = h
        .call(
            "POST",
            "/api/spaces/acme/documents",
            Some(json!({ "path": "notes/discounts.md", "content": "---\nvisibility: secret\n---\n# Discounts\n\nThe staff code is ZEBRA42.\n" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    index(&h).await;

    // Found by meaning and words, with where it came from.
    let r = find(&h, "can guests check out?").await;
    let top = &r[0];
    assert_eq!(top["heading_path"], "Constraints › Accounts", "{r:#?}");
    assert_eq!(
        (top["document"]["path"].as_str(), top["anchor"].as_str()),
        (Some("constraints.md"), Some("accounts"))
    );
    assert_eq!(top["standing"], "authoritative");
    assert_eq!(top["matched"], json!({ "words": true, "meaning": true }));

    // An approved decision overrides it: the decision comes first, the
    // section is outdated and says what overrides it.
    let (s, d) = h
        .call(
            "POST",
            "/api/spaces/acme/decisions",
            Some(json!({
                "statement": "Guests can check out without an account.",
                "reason": "Most orders are gifts from one-off buyers.",
                "contradicts": [{ "doc": "constraints.md", "anchor": "accounts" }],
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    index(&h).await;
    let r = find(&h, "can guests check out?").await;
    let pos = |src: &str| {
        r.iter()
            .position(|x| x["source"] == src && (src != "document" || x["anchor"] == "accounts"))
            .unwrap_or_else(|| panic!("no {src} in {r:#?}"))
    };
    assert!(pos("decision") < pos("document"), "{r:#?}");
    let section = &r[pos("document")];
    assert_eq!(section["standing"], "outdated");
    assert_eq!(section["overridden_by"][0]["id"], d["id"]);
    assert_eq!(r[pos("decision")]["standing"], "authoritative");
    // Outdated results sink below everything current.
    let first_outdated = r.iter().position(|x| x["standing"] == "outdated").unwrap();
    assert!(
        r[first_outdated..]
            .iter()
            .all(|x| x["standing"] == "outdated"),
        "{r:#?}"
    );

    // Secret documents: found by their words, never embedded.
    let r = find(&h, "ZEBRA42").await;
    assert_eq!(r[0]["document"]["path"], "notes/discounts.md");
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chunks c JOIN documents d ON d.id = c.doc_id
         WHERE d.path = 'notes/discounts.md' AND (c.embedding IS NOT NULL OR NOT c.secret)",
    )
    .fetch_one(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);

    // Claude edits one section: only that one is tentative, and only it
    // needs a new embedding.
    let (_, cur) = h
        .call("GET", &format!("/api/documents/{constraints}"), None)
        .await;
    let content = cur["content"]
        .as_str()
        .unwrap()
        .replace("live by March", "live by April");
    // Claude's edit applies at once, marked as not reviewed.
    let (s, p) = h
        .key_call(
            "POST",
            "/api/spaces/acme/proposals",
            "ck_claude",
            json!({ "kind": "doc_edit", "rationale": "The launch moved.",
                    "change": { "document": constraints.as_str(), "version": cur["version"], "content": content } }),
        )
        .await;
    assert_eq!(
        (s, p["status"].as_str(), p["decided_by"].as_str()),
        (StatusCode::CREATED, Some("applied"), Some("auto"))
    );
    hub::index::run_once(&h.state.pool, None).await.unwrap(); // rebuild, without embedding
    let unembedded: Vec<String> = sqlx::query_scalar(
        "SELECT heading_path FROM chunks WHERE doc_id = $1 AND embedding IS NULL",
    )
    .bind(&constraints)
    .fetch_all(&h.state.pool)
    .await
    .unwrap();
    assert_eq!(
        unembedded,
        ["Constraints › Deadlines"],
        "unchanged chunks keep their embedding"
    );
    index(&h).await;
    let r = find(&h, "when is the site live").await;
    let deadlines = r
        .iter()
        .find(|x| x["heading_path"] == "Constraints › Deadlines")
        .unwrap();
    assert_eq!(
        deadlines["standing"], "tentative",
        "Claude wrote it and you haven't"
    );
    let accounts = r
        .iter()
        .find(|x| x["heading_path"] == "Constraints › Accounts");
    assert!(accounts.is_none_or(|a| a["standing"] == "outdated"));

    // Filters: rules only, and for one app.
    let wr = doc_id(&h, "acme", "writing-rules.md").await;
    write(&h, &wr, "---\ntype: writing-rules\n---\n## Reports and proposals\n\n- Never promise a launch date.\n").await;
    index(&h).await;
    let (_, v) = h
        .call(
            "GET",
            "/api/search?q=launch+date&space=acme&source=rule",
            None,
        )
        .await;
    assert_eq!(v["results"][0]["source"], "rule", "{v}");
    assert!(
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["source"] == "rule")
    );
    let (_, v) = h
        .call(
            "GET",
            "/api/search?q=launch+date&space=acme&source=rule&for=design",
            None,
        )
        .await;
    assert_eq!(
        v["results"],
        json!([]),
        "a Typeset rule doesn't apply to Design"
    );

    // The trash takes a document out of search.
    h.call("DELETE", &format!("/api/documents/{constraints}"), None)
        .await;
    index(&h).await;
    assert!(
        find(&h, "guests account")
            .await
            .iter()
            .all(|x| x["document"]["id"] != constraints.as_str())
    );

    let (_, st) = h.call("GET", "/api/index", None).await;
    assert_eq!(
        (st["embeddings"].as_str(), st["queued"].as_i64()),
        (Some("fake/words"), Some(0))
    );
    h.done().await;
}

#[tokio::test]
async fn search_by_words_without_an_embedder() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.fake.add_client("cl_2", "zaltek", "Zaltek", true);
    h.sync().await;
    h.fake.0.lock().unwrap().keys.insert(
        "ck_acme".into(),
        json!({ "active": true, "id": "k2", "key": "acme-bot", "kind": "caller", "clients": ["acme"] }),
    );
    for slug in ["acme", "zaltek"] {
        let brief = doc_id(&h, slug, "brief.md").await;
        write(
            &h,
            &brief,
            &format!("# Brief\n\n## Goals\n\n{slug} wants more bookings.\n"),
        )
        .await;
    }
    index(&h).await;
    let (_, v) = h.call("GET", "/api/search?q=bookings", None).await;
    assert_eq!(v["meaning"], false);
    let slugs: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["space"]["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"acme") && slugs.contains(&"zaltek"), "{v}");
    // A key limited to one client searches only that client (and the studio).
    let (_, v) = h.with_key("GET", "/api/search?q=bookings", "ck_acme").await;
    assert!(
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["space"]["slug"] != "zaltek"),
        "{v}"
    );
    assert_eq!(
        h.with_key("GET", "/api/search?q=bookings&space=zaltek", "ck_acme")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    h.done().await;
}

// ---------- Proposals ----------

#[tokio::test]
async fn proposals_wait_apply_and_revert() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;
    h.fake.0.lock().unwrap().keys.insert(
        "ck_claude".into(),
        json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }),
    );
    wait_for(&h, &["doc_edit"]).await;
    let brief = doc_id(&h, "acme", "brief.md").await;
    write(
        &h,
        &brief,
        "# Brief\n\n## Goals\n\nMore bookings.\n\n## Scope\n\nA new site.\n",
    )
    .await;
    let (_, d) = h
        .call("GET", &format!("/api/documents/{brief}"), None)
        .await;
    let v = d["version"].clone();

    // Edits must find their text exactly once.
    let (s, e) = h
        .key_call(
            "POST",
            "/api/spaces/acme/proposals",
            "ck_claude",
            json!({ "kind": "doc_edit", "rationale": "x",
                    "change": { "document": "brief.md", "version": v, "edits": [{ "find": "o", "replace": "0" }] } }),
        )
        .await;
    assert_eq!(
        (s, e["path"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("change.edits[0].find")
        )
    );
    // A proposal against an old version is refused at once.
    let (s, e) = h
        .key_call(
            "POST",
            "/api/spaces/acme/proposals",
            "ck_claude",
            json!({ "kind": "doc_edit", "rationale": "x",
                    "change": { "document": "brief.md", "version": 1, "content": "# Brief\n" } }),
        )
        .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );

    let p = propose(
        &h,
        "doc_edit",
        json!({ "document": "brief.md", "version": v, "edits": [{ "find": "More bookings.", "replace": "Twice the bookings by June." }] }),
        "The kick-off call set a target.",
    )
    .await;
    assert!(
        p["diff"]
            .as_str()
            .unwrap()
            .contains("-More bookings.\n+Twice the bookings by June.")
    );
    // You edit the document meanwhile: the proposal is stale, not applied.
    write(
        &h,
        &brief,
        "# Brief\n\n## Goals\n\nMore bookings, and fewer calls.\n\n## Scope\n\nA new site.\n",
    )
    .await;
    let done = approve(&h, &p).await;
    assert_eq!(done["status"], "stale");
    let (_, d) = h
        .call("GET", &format!("/api/documents/{brief}"), None)
        .await;
    assert!(d["content"].as_str().unwrap().contains("fewer calls"));

    // Approved: applied, recorded as Claude's and approved by you; then reverted.
    let p = propose(
        &h,
        "doc_edit",
        json!({ "document": "brief.md", "version": d["version"], "edits": [{ "find": "A new site.", "replace": "A new site and a booking flow." }] }),
        "Scope grew.",
    )
    .await;
    let done = approve(&h, &p).await;
    assert_eq!(done["status"], "applied");
    let (_, d) = h
        .call("GET", &format!("/api/documents/{brief}"), None)
        .await;
    assert_eq!(d["updated_by"], "claude-code, approved by you");
    let (s, r) = h
        .call(
            "POST",
            &format!("/api/proposals/{}/revert", p["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(
        (s, r["status"].as_str()),
        (StatusCode::OK, Some("reverted")),
        "{r}"
    );
    let (_, d) = h
        .call("GET", &format!("/api/documents/{brief}"), None)
        .await;
    assert!(!d["content"].as_str().unwrap().contains("booking flow"));

    // Claude's creates apply at once by default, recorded as "auto".
    let link = json!({ "app": "board", "kind": "board", "title": "Booking site plan", "url": "https://board.kritano.com/boards/01ABC" });
    let applied = propose(&h, "artefact_link", link.clone(), "Planned it").await;
    assert_eq!(
        (applied["status"].as_str(), applied["decided_by"].as_str()),
        (Some("applied"), Some("auto"))
    );
    let (_, a) = h.call("GET", "/api/spaces/acme/artefacts", None).await;
    assert_eq!(a["artefacts"][0]["title"], "Booking site plan");

    // Removals always wait, and can't be set otherwise.
    let unlink = propose(
        &h,
        "artefact_unlink",
        json!({ "artefact": a["artefacts"][0]["id"] }),
        "Wrong client",
    )
    .await;
    assert_eq!(
        (unlink["status"].as_str(), unlink["stays_manual"].as_str()),
        (Some("pending"), Some("it removes something"))
    );
    let (s, _) = h
        .call(
            "PATCH",
            "/api/settings/approvals",
            Some(json!({ "policies": [{ "space": "*", "kind": "artefact_unlink", "mode": "auto" }] })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(approve(&h, &unlink).await["status"], "applied");
    let (_, a) = h.call("GET", "/api/spaces/acme/artefacts", None).await;
    assert_eq!(a["artefacts"].as_array().unwrap().len(), 0);
    let (_, r) = h
        .call(
            "POST",
            &format!("/api/proposals/{}/revert", unlink["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(r["status"], "reverted");
    let (_, a) = h.call("GET", "/api/spaces/acme/artefacts", None).await;
    assert_eq!(
        a["artefacts"].as_array().unwrap().len(),
        1,
        "unlinking undone"
    );

    // A document clients can see waits too.
    let client_doc = propose(
        &h,
        "doc_create",
        json!({ "path": "welcome.md", "content": "---\nvisibility: client\n---\n# Welcome\n" }),
        "For the portal",
    )
    .await;
    assert_eq!(client_doc["status"], "pending");
    assert_eq!(
        client_doc["stays_manual"],
        "it changes a document clients can see"
    );

    // You can make a kind wait, for every client or just one.
    wait_for(&h, &["rule", "doc_create"]).await;
    let rule = propose(
        &h,
        "rule",
        json!({ "kind": "avoid", "text": "Avoid jargon" }),
        "x",
    )
    .await;
    assert_eq!(rule["status"], "pending");
    h.call(
        "PATCH",
        "/api/settings/approvals",
        Some(json!({ "policies": [{ "space": "cl_1", "kind": "rule", "mode": "auto" }] })),
    )
    .await;
    let rule2 = propose(
        &h,
        "rule",
        json!({ "kind": "avoid", "text": "Avoid buzzwords" }),
        "x",
    )
    .await;
    assert_eq!(rule2["status"], "applied", "acme lets rules through");
    let notes = propose(
        &h,
        "doc_create",
        json!({ "path": "notes/call.md", "content": "# Call\n" }),
        "Notes",
    )
    .await;
    assert_eq!(notes["status"], "pending");

    // Withdraw is the proposer's; reject and approve are yours.
    let path = format!("/api/proposals/{}/withdraw", notes["id"].as_str().unwrap());
    assert_eq!(
        h.call("POST", &path, None).await.0,
        StatusCode::FORBIDDEN,
        "you reject; only Claude withdraws"
    );
    let (s, w) = h.with_key("POST", &path, "ck_claude").await;
    assert_eq!(
        (s, w["status"].as_str()),
        (StatusCode::OK, Some("withdrawn"))
    );
    let (_, list) = h.call("GET", "/api/proposals?status=pending", None).await;
    assert_eq!(
        list["pending"], 2,
        "the client document and the first rule: {list}"
    );
    assert_eq!(list["proposals"][0]["space"]["slug"], "acme");
    h.done().await;
}

// ---------- MCP ----------

struct Mcp<'a> {
    #[allow(dead_code)]
    h: &'a Hub,
    /// One router for the conversation: MCP sessions live in it.
    app: Router,
    key: &'static str,
    session: Option<String>,
    next: u64,
}

impl<'a> Mcp<'a> {
    async fn connect(h: &'a Hub, key: &'static str) -> Mcp<'a> {
        let mut m = Mcp {
            h,
            app: router(h.state.clone()),
            key,
            session: None,
            next: 1,
        };
        let (status, body, session) = m
            .raw(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
                         "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                                     "clientInfo": { "name": "test", "version": "1" } } }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["result"]["serverInfo"]["name"], "hub");
        assert!(
            body["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("client_context")
        );
        m.session = session;
        m.raw(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        m
    }

    async fn raw(&self, message: Value) -> (StatusCode, Value, Option<String>) {
        let mut req = Request::post("/mcp")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.key))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18");
        if let Some(s) = &self.session {
            req = req.header("mcp-session-id", s);
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::from(message.to_string())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let session = res
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        let body = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .find(|v| v.get("jsonrpc").is_some())
            .or_else(|| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        (status, body, session)
    }

    async fn call(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        self.next += 1;
        let (_, body, _) = self
            .raw(
                json!({ "jsonrpc": "2.0", "id": self.next, "method": "tools/call",
                         "params": { "name": name, "arguments": arguments } }),
            )
            .await;
        let result = &body["result"];
        assert!(result.is_object(), "no result for {name}: {body}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap_or(Value::String(text.into())),
        )
    }

    async fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let (err, v) = self.call(name, arguments).await;
        assert!(!err, "{name} failed: {v}");
        v
    }
}

#[tokio::test]
async fn claude_works_through_the_mcp() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;
    h.fake.0.lock().unwrap().keys.insert(
        "ck_claude".into(),
        json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }),
    );
    let wr = doc_id(&h, "acme", "writing-rules.md").await;
    write(
        &h,
        &wr,
        "---\ntype: writing-rules\n---\n## Everywhere\n\n- Never say \"cheap\".\n",
    )
    .await;
    let brief = doc_id(&h, "acme", "brief.md").await;
    write(
        &h,
        &brief,
        "---\ntype: brief\n---\n# Brief\n\nA booking site for a small hotel.\n",
    )
    .await;

    let mut m = Mcp::connect(&h, "ck_claude").await;
    let (st, tools, _) = m
        .raw(json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/list" }))
        .await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{st} {tools}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "client_context",
        "search",
        "check",
        "get_document",
        "edit_document",
        "create_document",
        "delete_document",
        "record_decision",
        "update_decision",
        "retire_decision",
        "create_rule",
        "update_rule",
        "retire_rule",
        "create_client",
        "update_client",
        "delete_client",
        "approve_proposal",
        "link_artefact",
        "unlink_artefact",
        "library_delete",
    ] {
        assert!(names.contains(&t), "{t} missing from {names:?}");
    }

    let clients = m.ok("list_clients", json!({})).await;
    assert!(
        clients["clients"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["client"] == "acme")
    );
    let ctx = m.ok("client_context", json!({ "client": "acme" })).await;
    assert!(
        ctx["brief"]["text"]
            .as_str()
            .unwrap()
            .contains("small hotel"),
        "{ctx}"
    );
    assert_eq!(ctx["rules"][0]["text"], "Never say \"cheap\".");
    assert_eq!(ctx["rules"][0]["hard"], true);
    assert_eq!(ctx["brand"]["slug"], "acme");

    let v = m
        .ok(
            "check",
            json!({ "client": "acme", "text": "Rooms are cheap!", "for": "typeset" }),
        )
        .await;
    assert_eq!(v["ok"], false);
    assert_eq!(v["violations"][0]["spans"][0]["text"], "cheap");

    // Claude edits and renames directly; it's recorded, and not reviewed.
    let doc = m
        .ok(
            "get_document",
            json!({ "client": "acme", "document": "brief.md" }),
        )
        .await;
    let p = m
        .ok(
            "edit_document",
            json!({ "client": "acme", "document": "brief.md", "version": doc["version"],
                    "edits": [{ "find": "small hotel.", "replace": "small hotel in Bath." }],
                    "path": "brief/overview.md", "rationale": "From the client's email." }),
        )
        .await;
    assert_eq!(
        (p["status"].as_str(), p["decided_by"].as_str()),
        (Some("applied"), Some("auto")),
        "{p}"
    );
    assert!(p["what_happened"].as_str().unwrap().starts_with("Done"));
    let (_, d) = h
        .call("GET", &format!("/api/documents/{brief}"), None)
        .await;
    assert!(d["content"].as_str().unwrap().contains("Bath"));
    assert_eq!(
        (d["path"].as_str(), d["updated_by"].as_str()),
        (Some("brief/overview.md"), Some("claude-code, auto"))
    );
    let made = m
        .ok("create_document", json!({ "client": "acme", "path": "notes/call.md", "content": "# Call\n\nThey want a spa page.\n" }))
        .await;
    assert_eq!(made["status"], "applied");

    // Deleting waits for you, in Hub: Claude can't approve it, whatever you said.
    let del = m.ok("delete_document", json!({ "client": "acme", "document": "notes/call.md", "reason": "Merged into the brief" })).await;
    assert_eq!(del["status"], "pending");
    assert!(
        del["what_happened"]
            .as_str()
            .unwrap()
            .contains("removals always do")
    );
    let (err, e) = m
        .call(
            "approve_proposal",
            json!({ "proposal_id": del["id"], "user_said": "yes delete it" }),
        )
        .await;
    assert!(err && e["error"] == "approve_in_hub", "{e}");
    assert_eq!(approve(&h, &del).await["status"], "applied");

    // A change you've set to wait can be approved by Claude, with your words.
    wait_for(&h, &["doc_create"]).await;
    let w = m
        .ok(
            "create_document",
            json!({ "client": "acme", "path": "notes/later.md", "content": "# Later\n" }),
        )
        .await;
    assert_eq!(w["status"], "pending");
    let (err, e) = m
        .call(
            "approve_proposal",
            json!({ "proposal_id": w["id"], "user_said": "" }),
        )
        .await;
    assert!(err, "{e}");
    let done = m
        .ok(
            "approve_proposal",
            json!({ "proposal_id": w["id"], "user_said": "yes add that" }),
        )
        .await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["decided_by"].as_str(),
            done["note"].as_str()
        ),
        (
            Some("applied"),
            Some("user via claude-code"),
            Some("yes add that")
        )
    );
    let docs = m.ok("list_documents", json!({ "client": "acme" })).await;
    assert!(
        docs["documents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["path"] != "notes/call.md")
    );

    // A client-visible document has to be approved in Hub itself.
    let p = m
        .ok(
            "create_document",
            json!({ "client": "acme", "path": "welcome.md", "content": "---\nvisibility: client\n---\n# Welcome\n" }),
        )
        .await;
    assert_eq!(p["status"], "pending");
    let (err, e) = m
        .call(
            "approve_proposal",
            json!({ "proposal_id": p["id"], "user_said": "approve it" }),
        )
        .await;
    assert!(err && e["error"] == "approve_in_hub", "{e}");

    // Rules and decisions: create and update directly, retire with approval.
    let r = m.ok("create_rule", json!({ "client": "acme", "kind": "avoid", "text": "Avoid jargon", "check": { "terms": ["synergy"] } })).await;
    let rule_id = r["result"]["rule"].as_str().unwrap().to_string();
    let u = m
        .ok(
            "update_rule",
            json!({ "client": "acme", "rule": rule_id, "text": "Avoid jargon and buzzwords" }),
        )
        .await;
    assert_eq!(u["status"], "applied", "{u}");
    let rules = m.ok("get_rules", json!({ "client": "acme" })).await;
    let mine = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == rule_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (mine["text"].as_str(), mine["check"]["terms"][0].as_str()),
        (Some("Avoid jargon and buzzwords"), Some("synergy"))
    );
    let (err, e) = m
        .call(
            "update_rule",
            json!({ "client": "acme", "rule": ctx["rules"][0]["id"], "text": "x" }),
        )
        .await;
    assert!(
        err && e["message"].as_str().unwrap().contains("Writing rules"),
        "a document's rule is edited there: {e}"
    );
    let ret = m
        .ok(
            "retire_rule",
            json!({ "client": "acme", "id": rule_id, "reason": "Too fussy" }),
        )
        .await;
    assert_eq!(ret["status"], "pending");
    approve(&h, &ret).await;
    let (_, rl) = h.call("GET", &format!("/api/rules/{rule_id}"), None).await;
    assert_eq!(rl["status"], "retired");
    let (_, back) = h
        .call(
            "POST",
            &format!("/api/proposals/{}/revert", ret["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(back["status"], "reverted");
    let (_, rl) = h.call("GET", &format!("/api/rules/{rule_id}"), None).await;
    assert_eq!(rl["status"], "approved", "retiring undone");

    let dec = m
        .ok(
            "record_decision",
            json!({ "client": "acme", "statement": "No live chat.", "reason": "Nobody to answer it.", "applies_to": ["design"] }),
        )
        .await;
    assert_eq!(
        (dec["kind"].as_str(), dec["status"].as_str()),
        (Some("decision"), Some("applied"))
    );
    let dec_id = dec["result"]["decision"].as_str().unwrap().to_string();
    let (_, dd) = h
        .call("GET", &format!("/api/decisions/{dec_id}"), None)
        .await;
    assert_eq!(
        dd["standing"], "tentative",
        "in force, but you haven't reviewed it"
    );
    m.ok(
        "update_decision",
        json!({ "client": "acme", "decision": dec_id, "reason": "Nobody to answer it after 6pm." }),
    )
    .await;
    let (_, dd) = h
        .call("GET", &format!("/api/decisions/{dec_id}"), None)
        .await;
    assert_eq!(dd["reason"], "Nobody to answer it after 6pm.");
    let ret = m
        .ok(
            "retire_decision",
            json!({ "client": "acme", "id": dec_id, "reason": "They hired someone" }),
        )
        .await;
    assert_eq!(ret["status"], "pending");

    // Clients: add and rename directly; deleting waits.
    let c = m
        .ok(
            "create_client",
            json!({ "name": "Zaltek", "slug": "zaltek" }),
        )
        .await;
    assert_eq!(c["status"], "applied", "{c}");
    let (_, z) = h.call("GET", "/api/spaces/zaltek", None).await;
    assert_eq!(z["space"]["name"], "Zaltek");
    assert_eq!(
        z["documents"].as_array().unwrap().len(),
        5,
        "with starter documents"
    );
    let (err, e) = m
        .call(
            "create_client",
            json!({ "name": "Again", "slug": "zaltek" }),
        )
        .await;
    assert!(err && e["path"] == "change.slug", "{e}");
    m.ok(
        "update_client",
        json!({ "client": "zaltek", "name": "Zaltek Ltd" }),
    )
    .await;
    let (_, z) = h.call("GET", "/api/spaces/zaltek", None).await;
    assert_eq!(z["space"]["name"], "Zaltek Ltd");
    assert_eq!(
        h.fake
            .0
            .lock()
            .unwrap()
            .clients
            .iter()
            .find(|c| c.slug == "zaltek")
            .unwrap()
            .name,
        "Zaltek Ltd"
    );
    let gone = m
        .ok(
            "delete_client",
            json!({ "client": "zaltek", "reason": "Lost the pitch" }),
        )
        .await;
    assert_eq!(gone["status"], "pending");
    let (err, e) = m
        .call(
            "approve_proposal",
            json!({ "proposal_id": gone["id"], "user_said": "delete it" }),
        )
        .await;
    assert!(err && e["error"] == "approve_in_hub", "{e}");
    approve(&h, &gone).await;
    assert_eq!(
        h.call("GET", "/api/spaces/zaltek", None).await.0,
        StatusCode::NOT_FOUND,
        "archived"
    );
    let (_, list) = h.call("GET", "/api/proposals?status=applied", None).await;
    let shown = list["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "client_delete")
        .unwrap()
        .clone();
    assert_eq!(
        shown["space"]["name"], "Zaltek Ltd (deleted)",
        "a deleted client's changes still show"
    );
    assert!(
        h.fake
            .0
            .lock()
            .unwrap()
            .clients
            .iter()
            .all(|c| c.slug != "zaltek"),
        "deleted in Clients"
    );

    // Guides: an app's, in full, when the job is for that app; the client's
    // own replaces the studio's of the same name.
    for (scope, name, body) in [
        (
            "_studio",
            "reel-cards",
            "# Reel cards\n> Studio way\n\nOne idea per card.",
        ),
        (
            "_studio",
            "carousels",
            "# Carousels\n> Five slides at most\n",
        ),
        (
            "acme",
            "reel-cards",
            "# Reel cards for Acme\n> Slower\n\nCalm, never flashy.",
        ),
    ] {
        let r = m.ok("library_put", json!({ "kind": "motion-guide", "name": name, "scope": scope, "body": body, "rationale": "Guide" })).await;
        assert_eq!(r["status"], "applied", "{r}");
    }
    m.ok("library_put", json!({ "kind": "typeset-guide", "name": "reports", "body": "# Reports\n", "rationale": "Guide" })).await;
    let ctx = m
        .ok(
            "client_context",
            json!({ "client": "acme", "for": "motion" }),
        )
        .await;
    let g: Vec<(&str, &str)> = ctx["guides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["name"].as_str().unwrap(), g["scope"].as_str().unwrap()))
        .collect();
    assert_eq!(
        g,
        [("reel-cards", "acme"), ("carousels", "_studio")],
        "{ctx}"
    );
    assert!(
        ctx["guides"][0]["body"]
            .as_str()
            .unwrap()
            .contains("never flashy")
    );
    let ctx = m.ok("client_context", json!({ "client": "acme" })).await;
    assert_eq!(
        ctx["guides"].as_array().unwrap().len(),
        3,
        "names of every app's guides"
    );
    assert!(
        ctx["guides"][0].get("body").is_none(),
        "no bodies without `for`"
    );

    // Errors say what's wrong.
    let (err, e) = m
        .call("link_artefact", json!({ "client": "acme", "app": "figma", "kind": "design", "title": "x", "url": "https://x" }))
        .await;
    assert!(err && e["path"] == "change.app", "{e}");
    let r = m
        .ok("search", json!({ "client": "acme", "query": "hotel Bath" }))
        .await;
    assert!(r["results"].is_array());
    h.done().await;
}

// ---------- Library ----------

const STYLE: &str = "# ElevenLabs — Style Reference\n> Calm, editorial, generous spacing\n\n## Typography\n\nBig serif display type, tight leading.\n";

/// Design's service key, acting for someone.
async fn as_design(
    h: &Hub,
    method: &str,
    uri: &str,
    who: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer ck_design");
    if let Some(w) = who {
        req = req.header("x-hub-on-behalf-of", w);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    };
    let (s, _, v) = h.raw(req.unwrap()).await;
    (s, v)
}

#[tokio::test]
async fn the_library_keeps_styles_for_everyone() {
    let Some(mut h) = hub(true).await else { return };
    h.sign_in().await;
    h.fake.add_client("cl_1", "acme", "Acme", true);
    h.sync().await;
    {
        let mut d = h.fake.0.lock().unwrap();
        d.keys.insert("ck_claude".into(), json!({ "active": true, "id": "k1", "key": "claude-code", "kind": "caller", "clients": null }));
        d.keys.insert("ck_design".into(), json!({ "active": true, "id": "k2", "key": "design", "kind": "service", "clients": null }));
    }
    let url = "/api/library/design-style/_studio/elevenlabs";

    // You save a style: title and summary come from the file; saving the
    // same again makes no new version.
    let (s, v) = h
        .call(
            "PUT",
            url,
            Some(
                json!({ "body": STYLE, "tags": ["Editorial", "dark"], "source": "Refero Styles" }),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let item = &v["item"];
    assert_eq!(
        (item["title"].as_str(), item["summary"].as_str()),
        (
            Some("ElevenLabs — Style Reference"),
            Some("Calm, editorial, generous spacing")
        )
    );
    assert_eq!(
        (item["version"].as_i64(), item["tags"].clone()),
        (Some(1), json!(["editorial", "dark"]))
    );
    let (_, v) = h.call("PUT", url, Some(json!({ "body": STYLE }))).await;
    assert_eq!(v["item"]["version"], 1);
    let (_, v) = h
        .call(
            "PUT",
            url,
            Some(json!({ "body": format!("{STYLE}\n## Do\n\nWhite space.\n"), "version": 1 })),
        )
        .await;
    assert_eq!(v["item"]["version"], 2);
    let (s, _) = h
        .call("PUT", url, Some(json!({ "body": "# x\n", "version": 1 })))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "a change against an old version");
    let (_, revs) = h.call("GET", &format!("{url}/revisions"), None).await;
    assert_eq!(revs["revisions"].as_array().unwrap().len(), 2);

    // Names are unique per kind and scope: a client can have its own.
    let (s, _) = h
        .call(
            "PUT",
            "/api/library/design-style/acme/elevenlabs",
            Some(json!({ "body": "# Acme's take\n" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, own) = h
        .call("GET", "/api/library/design-style/acme/elevenlabs", None)
        .await;
    assert_eq!(own["title"], "Acme's take");
    let (s, _) = h
        .call("GET", "/api/library/design-style/acme/linear", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .call(
            "PUT",
            "/api/library/design-style/_studio/Bad%20Name",
            Some(json!({ "body": "# x" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    h.call("DELETE", "/api/library/design-style/acme/elevenlabs", None)
        .await;
    let (_, fell_back) = h
        .call(
            "GET",
            "/api/library/design-style/acme/elevenlabs?fallback=1",
            None,
        )
        .await;
    assert_eq!(fell_back["scope"], "_studio", "falls back to the studio's");

    // (Claude's library saves set to wait, to see them as proposals.)
    wait_for(&h, &["library_put"]).await;
    // Design, acting for you (its Styles panel), saves directly; acting for
    // Claude, it proposes; without saying, it's refused; and its key can't
    // reach anything but the library.
    let (s, v) = as_design(
        &h,
        "PUT",
        "/api/library/design-style/_studio/linear",
        Some("user"),
        Some(json!({ "body": "# Linear\n> Precise\n" })),
    )
    .await;
    assert_eq!(
        (s, v["item"]["updated_by"].as_str()),
        (StatusCode::OK, Some("user")),
        "{v}"
    );
    let (s, v) = as_design(
        &h,
        "PUT",
        "/api/library/design-style/_studio/stripe",
        Some("claude"),
        Some(json!({ "body": "# Stripe\n" })),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        (
            v["proposal"]["status"].as_str(),
            v["proposal"]["proposed_by"].as_str()
        ),
        (Some("pending"), Some("claude-in-design"))
    );
    let (s, _) = as_design(&h, "GET", "/api/library", None, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = as_design(&h, "GET", "/api/spaces", Some("user"), None).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a service key is for the library only"
    );
    let (s, list) = as_design(
        &h,
        "GET",
        "/api/library?kind=design-style",
        Some("user"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["elevenlabs", "linear"],
        "the proposal isn't there yet"
    );
    assert!(
        list["items"][0].get("body").is_none(),
        "lists leave the body out"
    );
    let stripe = v["proposal"].clone();
    approve(&h, &stripe).await;
    let (_, s) = h
        .call("GET", "/api/library/design-style/_studio/stripe", None)
        .await;
    assert_eq!(s["updated_by"], "claude-in-design, approved by you");

    // Claude's change to an item you then edit is stale.
    let (_, p) = h
        .key_call(
            "PUT",
            url,
            "ck_claude",
            json!({ "body": "# ElevenLabs\n> Louder\n", "version": 2, "rationale": "Brighter" }),
        )
        .await;
    assert_eq!(p["proposal"]["status"], "pending");
    h.call(
        "PUT",
        url,
        Some(json!({ "body": format!("{STYLE}\n## Don't\n\nShadows.\n") })),
    )
    .await;
    assert_eq!(approve(&h, &p["proposal"]).await["status"], "stale");

    // Pins: the client's library and client_context list what it uses.
    let (_, el) = h.call("GET", url, None).await;
    let (s, _) = h
        .call(
            "PUT",
            &format!("/api/spaces/acme/pins/{}", el["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, uses) = h.call("GET", "/api/spaces/acme/library", None).await;
    assert_eq!(uses["items"][0]["name"], "elevenlabs");
    let (_, el) = h.call("GET", url, None).await;
    assert_eq!(el["pinned_to"][0]["slug"], "acme");
    let linear = h
        .call("GET", "/api/library/design-style/_studio/linear", None)
        .await
        .1;
    let p = propose(
        &h,
        "library_pin",
        json!({ "item": linear["id"] }),
        "Acme likes it",
    )
    .await;
    assert_eq!(p["status"], "applied", "pins apply at once");
    let (_, uses) = h.call("GET", "/api/spaces/acme/library", None).await;
    assert_eq!(uses["items"].as_array().unwrap().len(), 2);

    // Found by search, filtered to the library and a kind.
    index(&h).await;
    let (_, r) = h
        .call("GET", "/api/library?q=serif+display+type", None)
        .await;
    assert_eq!(r["results"][0]["library"]["name"], "elevenlabs", "{r}");
    let (_, r) = h
        .call(
            "GET",
            "/api/search?q=serif+display&source=library&type=design-style",
            None,
        )
        .await;
    assert!(
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["source"] == "library")
    );

    // Importing Design's styles is safe to repeat.
    let styles = json!([{ "name": "elevenlabs", "markdown": format!("{STYLE}\n## Don't\n\nShadows.\n"), "source": null },
                        { "name": "notion", "markdown": "# Notion\n> Plain\n", "source": "Refero Styles" }]);
    let n = hub::library::import_styles(&h.state.pool, &styles.to_string())
        .await
        .unwrap();
    assert_eq!(n, (1, 0, 1));
    let n = hub::library::import_styles(&h.state.pool, &styles.to_string())
        .await
        .unwrap();
    assert_eq!(n, (0, 0, 2));
    assert_eq!(
        hub::library::import_styles(&h.state.pool, "")
            .await
            .unwrap(),
        (0, 0, 0)
    );
    h.done().await;
}
