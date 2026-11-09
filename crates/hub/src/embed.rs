//! Embeddings for search by meaning (spec section 6.2): Voyage, or a local
//! fake for tests. The provider and model are recorded on every chunk, so
//! changing them means `hub reindex`.
//!
//! One enum rather than a trait object: there are two providers, and adding
//! one is a new arm here.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use crate::config::Config;

/// The width of the `chunks.embedding` column.
pub const DIMENSIONS: usize = 1024;
/// Texts per request, at most.
pub const BATCH: usize = 64;

/// The provider's rate limits, shared by every clone. Document batches wait
/// for a slot and leave one free each minute; a query takes a slot only if
/// one is free now, so a search never waits (it falls back to words).
#[derive(Clone)]
pub struct Limits {
    per_minute: usize,
    /// Characters per request: the token limit, at about 4 characters a
    /// token, with room to spare.
    pub chars_per_request: usize,
    sent: Arc<Mutex<VecDeque<Instant>>>,
}

impl Limits {
    pub fn new(requests_per_minute: u32, tokens_per_minute: u32) -> Self {
        Self {
            per_minute: requests_per_minute as usize,
            chars_per_request: (tokens_per_minute as usize / requests_per_minute.max(1) as usize)
                * 3,
            sent: Default::default(),
        }
    }

    fn unlimited() -> Self {
        Self {
            per_minute: usize::MAX,
            chars_per_request: usize::MAX,
            sent: Default::default(),
        }
    }

    /// Take a slot now if fewer than `allowed` were used in the last minute;
    /// otherwise how long until one frees up.
    fn take(&self, allowed: usize) -> Result<(), Duration> {
        let mut sent = self.sent.lock().unwrap();
        let now = Instant::now();
        while sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            sent.pop_front();
        }
        if sent.len() < allowed {
            sent.push_back(now);
            return Ok(());
        }
        Err(
            Duration::from_secs(60).saturating_sub(now.duration_since(sent[0]))
                + Duration::from_millis(50),
        )
    }

    /// Slots documents may use: all but one, so a search can get in.
    fn for_documents(&self) -> usize {
        if self.per_minute > 1 {
            self.per_minute - 1
        } else {
            1
        }
    }
}

/// Documents and queries are embedded differently (Voyage's `input_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Document,
    Query,
}

#[derive(Clone)]
pub enum Embedder {
    Voyage {
        key: String,
        model: String,
        http: reqwest::Client,
        limits: Limits,
    },
    /// Words hashed into the vector: similar words, similar vectors. No
    /// network, the same answer every time.
    Fake,
}

impl Embedder {
    pub fn from_config(c: &Config) -> Option<Self> {
        match c.embedding_provider.as_deref()? {
            "voyage" => Some(Self::Voyage {
                key: c.embedding_key.clone().unwrap_or_default(),
                model: c
                    .embedding_model
                    .clone()
                    .unwrap_or_else(|| "voyage-3.5".into()),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .build()
                    .expect("an HTTP client"),
                limits: Limits::new(
                    c.embedding_requests_per_minute,
                    c.embedding_tokens_per_minute,
                ),
            }),
            "fake" => Some(Self::Fake),
            _ => None,
        }
    }

    /// `provider/model`, recorded on each chunk.
    pub fn model(&self) -> String {
        match self {
            Self::Voyage { model, .. } => format!("voyage/{model}"),
            Self::Fake => "fake/words".into(),
        }
    }

    pub fn limits(&self) -> Limits {
        match self {
            Self::Voyage { limits, .. } => limits.clone(),
            Self::Fake => Limits::unlimited(),
        }
    }

    /// Embed documents, waiting for the rate limit if need be.
    pub async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let limits = self.limits();
        loop {
            match limits.take(limits.for_documents()) {
                Ok(()) => return self.embed(texts, Input::Document).await,
                Err(wait) => tokio::time::sleep(wait).await,
            }
        }
    }

    /// Embed a query now, or `None` if the rate limit says not yet.
    pub async fn embed_query(&self, query: &str) -> Option<Result<Vec<f32>>> {
        let limits = self.limits();
        limits.take(limits.per_minute).ok()?;
        Some(
            self.embed(&[query.to_string()], Input::Query)
                .await
                .map(|mut v| v.remove(0)),
        )
    }

    async fn embed(&self, texts: &[String], input: Input) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        match self {
            Self::Fake => Ok(texts.iter().map(|t| fake(t)).collect()),
            Self::Voyage {
                key, model, http, ..
            } => {
                #[derive(Deserialize)]
                struct Item {
                    index: usize,
                    embedding: Vec<f32>,
                }
                #[derive(Deserialize)]
                struct Reply {
                    data: Vec<Item>,
                }
                let res = http
                    .post("https://api.voyageai.com/v1/embeddings")
                    .bearer_auth(key)
                    .json(&json!({
                        "input": texts,
                        "model": model,
                        "input_type": if input == Input::Query { "query" } else { "document" },
                        "output_dimension": DIMENSIONS,
                    }))
                    .send()
                    .await
                    .context("reaching Voyage")?;
                let status = res.status();
                if !status.is_success() {
                    let body = res.text().await.unwrap_or_default();
                    bail!(
                        "Voyage said {status}: {}",
                        body.chars().take(300).collect::<String>()
                    );
                }
                let mut reply: Reply = res.json().await.context("Voyage's reply")?;
                reply.data.sort_by_key(|i| i.index);
                if reply.data.len() != texts.len()
                    || reply.data.iter().any(|i| i.embedding.len() != DIMENSIONS)
                {
                    bail!(
                        "Voyage returned {} embeddings for {} texts",
                        reply.data.len(),
                        texts.len()
                    );
                }
                Ok(reply.data.into_iter().map(|i| i.embedding).collect())
            }
        }
    }
}

/// The fake: each word (lightly stemmed) and each pair of words lands in a
/// bucket; the vector is normalised.
fn fake(text: &str) -> Vec<f32> {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(stem)
        .collect();
    let mut v = vec![0f32; DIMENSIONS];
    for w in &words {
        v[bucket(w)] += 1.0;
    }
    for pair in words.windows(2) {
        v[bucket(&format!("{} {}", pair[0], pair[1]))] += 0.5;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    } else {
        v[0] = 1.0;
    }
    v
}

fn stem(w: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(s) = w.strip_suffix(suffix)
            && s.len() > 2
        {
            return s.to_string();
        }
    }
    w.to_string()
}

/// FNV-1a, so buckets don't change between builds.
fn bucket(s: &str) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % DIMENSIONS as u64) as usize
}

/// A vector as pgvector's text form, for binding as `$n::vector`.
pub fn literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cos(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn the_fake_is_stable_and_roughly_semantic() {
        let a = fake("Guests must make an account before checking out");
        assert_eq!(a, fake("Guests must make an account before checking out"));
        assert_eq!(a.len(), DIMENSIONS);
        let q = fake("can guests check out?");
        let other = fake("The logo is green and the font is Outfit");
        assert!(cos(&a, &q) > cos(&other, &q));
        assert_eq!(literal(&[0.5, -1.0]), "[0.5,-1]");
    }

    #[test]
    fn limits_keep_a_slot_for_searches() {
        let l = Limits::new(3, 10_000);
        assert_eq!(l.chars_per_request, 9_999);
        assert!(l.take(l.for_documents()).is_ok());
        assert!(l.take(l.for_documents()).is_ok());
        let wait = l.take(l.for_documents()).unwrap_err();
        assert!(
            wait > Duration::from_secs(59),
            "documents wait for the minute"
        );
        assert!(l.take(3).is_ok(), "a search still gets in");
        assert!(l.take(3).is_err());
    }
}
