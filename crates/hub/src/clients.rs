//! Clients' API (its spec section 5): the client list, the event feed,
//! "Sign in with Clients" codes, and key checks. Hub calls it with its own
//! service key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

/// How long a key check is trusted (Clients' own guidance: 60 seconds).
const KEY_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientInfo {
    /// Never changes; slugs can be reused after a client is deleted.
    pub id: String,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub client_id: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

impl Event {
    pub fn slug(&self) -> Option<&str> {
        self.payload["slug"].as_str()
    }
}

/// Who a `ck_…` key belongs to.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Caller {
    pub id: String,
    /// The key's name, e.g. `claude-code`.
    pub key: String,
    pub kind: String,
    /// The client slugs this key may use; `None` means all.
    pub clients: Option<Vec<String>>,
}

/// Key checks by token hash: when checked, and the answer.
type KeyCache = Arc<Mutex<HashMap<Vec<u8>, (Instant, Option<Caller>)>>>;

#[derive(Clone)]
pub struct Clients {
    url: String,
    service_key: String,
    http: reqwest::Client,
    keys: KeyCache,
}

impl Clients {
    pub fn new(url: &str, service_key: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            service_key: service_key.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("an HTTP client"),
            keys: Default::default(),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        let res = req
            .bearer_auth(&self.service_key)
            .send()
            .await
            .map_err(|e| AppError::Unavailable(format!("Clients can't be reached: {e}")))?;
        let status = res.status();
        if !status.is_success() {
            return Err(AppError::Unavailable(format!("Clients said {status}")));
        }
        res.json()
            .await
            .map_err(|e| AppError::Unavailable(format!("Clients' reply: {e}")))
    }

    /// Every active client.
    pub async fn clients(&self) -> AppResult<Vec<ClientInfo>> {
        let v = self
            .send(self.http.get(format!("{}/v1/clients", self.url)))
            .await?;
        serde_json::from_value(v).map_err(|e| AppError::Unavailable(format!("Clients' list: {e}")))
    }

    /// Events after a cursor, oldest first (up to 100).
    pub async fn events(&self, after: i64) -> AppResult<Vec<Event>> {
        let v = self
            .send(
                self.http
                    .get(format!("{}/v1/events?after={after}&limit=100", self.url)),
            )
            .await?;
        serde_json::from_value(v)
            .map_err(|e| AppError::Unavailable(format!("Clients' events: {e}")))
    }

    /// Add a client in Clients (Hub's key must be in its `CLIENTS_MANAGERS`).
    pub async fn create_client(&self, slug: &str, name: &str) -> AppResult<ClientInfo> {
        let v = self
            .manage(
                self.http
                    .post(format!("{}/v1/clients", self.url))
                    .json(&json!({ "slug": slug, "name": name })),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| AppError::Unavailable(format!("Clients' reply: {e}")))
    }

    /// Rename a client in Clients.
    pub async fn rename_client(&self, slug: &str, name: &str) -> AppResult<ClientInfo> {
        let v = self
            .manage(
                self.http
                    .patch(format!("{}/v1/clients/{slug}", self.url))
                    .json(&json!({ "name": name })),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| AppError::Unavailable(format!("Clients' reply: {e}")))
    }

    /// Delete a client in Clients (its brand, versions and library go too).
    pub async fn delete_client(&self, slug: &str) -> AppResult<()> {
        self.manage(self.http.delete(format!("{}/v1/clients/{slug}", self.url)))
            .await?;
        Ok(())
    }

    /// A managing call: Clients' refusals become errors Claude can act on.
    async fn manage(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        let res = req
            .bearer_auth(&self.service_key)
            .send()
            .await
            .map_err(|e| AppError::Unavailable(format!("Clients can't be reached: {e}")))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        let detail = body["detail"]
            .as_str()
            .unwrap_or("Clients refused it")
            .to_string();
        match status.as_u16() {
            200..=299 => Ok(body),
            403 => Err(AppError::Unavailable(format!(
                "Clients won't let Hub manage clients yet: add hub to CLIENTS_MANAGERS ({detail})"
            ))),
            404 => Err(AppError::NotFound("client")),
            409 => Err(AppError::invalid("change.slug", detail)),
            422 => Err(AppError::invalid("change", detail)),
            _ => Err(AppError::Unavailable(format!(
                "Clients said {status}: {detail}"
            ))),
        }
    }

    /// Swap a one-time sign-in code for "it's you" (60 seconds, single use).
    pub async fn handoff(&self, code: &str) -> AppResult<bool> {
        let v = self
            .send(
                self.http
                    .post(format!("{}/v1/auth/handoff", self.url))
                    .json(&json!({ "code": code, "app": "hub" })),
            )
            .await?;
        Ok(v["active"] == true)
    }

    /// Who a key belongs to (`None` if unknown or revoked). Cached 60s.
    pub async fn introspect(&self, token: &str) -> AppResult<Option<Caller>> {
        let h = Sha256::digest(token.as_bytes()).to_vec();
        if let Some((at, c)) = self.keys.lock().unwrap().get(&h)
            && at.elapsed() < KEY_TTL
        {
            return Ok(c.clone());
        }
        let v = self
            .send(
                self.http
                    .post(format!("{}/v1/auth/introspect", self.url))
                    .json(&json!({ "token": token })),
            )
            .await?;
        let caller = if v["active"] == true {
            serde_json::from_value(v).ok()
        } else {
            None
        };
        self.keys
            .lock()
            .unwrap()
            .insert(h, (Instant::now(), caller.clone()));
        Ok(caller)
    }

    /// Forget cached answers for a revoked key (the `key.revoked` event).
    pub fn forget_key(&self, key_id: &str) {
        self.keys
            .lock()
            .unwrap()
            .retain(|_, (_, c)| c.as_ref().is_none_or(|c| c.id != key_id));
    }
}
