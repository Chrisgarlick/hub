//! Search (spec section 6.3): by words and by meaning at once, fused by
//! rank, with how far to rely on each result and what overrides it.

use std::collections::HashMap;

use hub_model::Standing;
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::decisions::{self, Decision};
use crate::embed::{self, Embedder};
use crate::error::AppResult;
use crate::rules::by_you;

/// Candidates from each side, and results returned at most.
const CANDIDATES: i64 = 50;
pub const MAX_LIMIT: usize = 25;
/// The usual constant for reciprocal rank fusion.
const K: f64 = 60.0;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    /// Space ids to search (a client's search includes the studio).
    pub spaces: Vec<String>,
    /// A document type (`brief`, `constraints`, …), or `rule` / `decision`.
    pub doc_type: Option<String>,
    /// `document`, `rule` or `decision`.
    pub source: Option<String>,
    /// An app: only what applies to it.
    pub target: Option<String>,
    /// Only results with one of these standings.
    pub standing: Option<Vec<Standing>>,
    pub limit: usize,
}

/// Reciprocal rank fusion: each list adds 1/(K + rank) for each id in it.
/// Returns ids by fused score, highest first, with the score scaled to 0–1
/// (1 = first in every list).
pub fn fuse(lists: &[Vec<i64>]) -> Vec<(i64, f64)> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    let mut first_seen: HashMap<i64, usize> = HashMap::new();
    let mut n = 0;
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *score.entry(*id).or_default() += 1.0 / (K + rank as f64 + 1.0);
            first_seen.entry(*id).or_insert_with(|| {
                n += 1;
                n
            });
        }
    }
    let best = lists.iter().filter(|l| !l.is_empty()).count().max(1) as f64 / (K + 1.0);
    let mut out: Vec<(i64, f64)> = score.into_iter().map(|(id, s)| (id, s / best)).collect();
    out.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(first_seen[&a.0].cmp(&first_seen[&b.0]))
    });
    out
}

/// Words for `to_tsquery`, any of them: "can guests check out?" becomes
/// `can | guests | check | out` (Postgres drops the stop words).
fn any_words(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(32)
        .map(str::to_lowercase)
        .collect();
    (!words.is_empty()).then(|| words.join(" | "))
}

const FILTERS: &str = "c.space_id = ANY($1)
    AND ($2::text IS NULL OR c.doc_type = $2)
    AND ($3::text IS NULL OR c.source = $3)
    AND ($4::text IS NULL OR c.applies_to && ARRAY['all', $4])";

#[derive(sqlx::FromRow)]
struct Hit {
    id: i64,
    space_id: String,
    slug: String,
    space_name: String,
    source: String,
    doc_id: Option<String>,
    path: Option<String>,
    title: Option<String>,
    rule_id: Option<String>,
    decision_id: Option<String>,
    revision: Option<i32>,
    heading_path: String,
    anchors: Vec<String>,
    text: String,
    doc_type: String,
    applies_to: Vec<String>,
    status: Option<String>,
    written_by: String,
}

/// What a result's standing is, and the approved decisions overriding it.
fn standing(h: &Hit, overriding: &[&Decision]) -> Standing {
    match h.source.as_str() {
        "document" if !overriding.is_empty() => Standing::Outdated,
        "document" if by_you(&h.written_by) => Standing::Authoritative,
        "document" => Standing::Tentative,
        _ => match h.status.as_deref() {
            Some("approved") => Standing::Authoritative,
            Some("proposed") => Standing::Tentative,
            _ => Standing::Outdated,
        },
    }
}

/// Run a search. `meaning` says whether the vector side ran (it doesn't
/// without an embedder, or when the provider's rate limit is used up).
pub async fn run(
    conn: &mut PgConnection,
    embedder: Option<&Embedder>,
    q: &Query,
) -> AppResult<Value> {
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let words: Vec<i64> = match any_words(&q.text) {
        None => vec![],
        Some(tsq) => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT c.id FROM chunks c, to_tsquery('english', $5) query
             WHERE c.tsv @@ query AND {FILTERS}
             ORDER BY ts_rank_cd(c.tsv, query) DESC, c.id LIMIT {CANDIDATES}"
            )))
            .bind(&q.spaces)
            .bind(&q.doc_type)
            .bind(&q.source)
            .bind(&q.target)
            .bind(tsq)
            .fetch_all(&mut *conn)
            .await?
        }
    };

    let mut meaning = false;
    let mut note = None;
    let mut vector: Vec<i64> = vec![];
    if let Some(e) = embedder
        && !q.text.trim().is_empty()
    {
        match e.embed_query(&q.text).await {
            None => {
                note = Some(
                    "searched by words only: the embedding provider's rate limit is used up for this minute",
                )
            }
            Some(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "query embedding failed; words only");
                note = Some("searched by words only: the embedding provider didn't answer");
            }
            Some(Ok(v)) => {
                meaning = true;
                vector = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT c.id FROM chunks c
                     WHERE c.embedding IS NOT NULL AND c.embed_model = $6 AND {FILTERS}
                     ORDER BY c.embedding <=> $5::vector LIMIT {CANDIDATES}"
                )))
                .bind(&q.spaces)
                .bind(&q.doc_type)
                .bind(&q.source)
                .bind(&q.target)
                .bind(embed::literal(&v))
                .bind(e.model())
                .fetch_all(&mut *conn)
                .await?;
            }
        }
    }

    let fused = fuse(&[words.clone(), vector.clone()]);
    let ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
    let hits: Vec<Hit> = sqlx::query_as(
        "SELECT c.id, c.space_id, s.slug, s.name AS space_name, c.source, c.doc_id, d.path, d.title,
                c.rule_id, c.decision_id, c.revision, c.heading_path, c.anchors, c.text, c.doc_type,
                c.applies_to, c.status, c.written_by
         FROM chunks c JOIN spaces s ON s.id = c.space_id LEFT JOIN documents d ON d.id = c.doc_id
         WHERE c.id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let by_id: HashMap<i64, &Hit> = hits.iter().map(|h| (h.id, h)).collect();

    // Approved decisions in these spaces, to mark the sections they override.
    let mut in_force: Vec<Decision> = Vec::new();
    for s in &q.spaces {
        in_force.extend(
            decisions::list(conn, s)
                .await?
                .into_iter()
                .filter(|d| d.status == "approved"),
        );
    }
    let overriding = |h: &Hit| -> Vec<&Decision> {
        let Some(doc) = &h.doc_id else { return vec![] };
        in_force
            .iter()
            .filter(|d| {
                d.contradicts.0.iter().any(|s| {
                    &s.doc == doc && s.anchor.as_ref().is_none_or(|a| h.anchors.contains(a))
                })
            })
            .collect()
    };

    let mut results: Vec<(bool, f64, Value)> = Vec::new();
    for (id, score) in &fused {
        let Some(h) = by_id.get(id) else { continue };
        let over = overriding(h);
        let st = standing(h, &over);
        if q.standing.as_ref().is_some_and(|f| !f.contains(&st)) {
            continue;
        }
        let mut v = json!({
            "id": h.id,
            "source": h.source,
            "space": { "id": h.space_id, "slug": h.slug, "name": h.space_name },
            "heading_path": h.heading_path,
            "text": h.text,
            "type": h.doc_type,
            "applies_to": h.applies_to,
            "score": (score * 1000.0).round() / 1000.0,
            "standing": st,
            "matched": { "words": words.contains(id), "meaning": vector.contains(id) },
        });
        match h.source.as_str() {
            "document" => {
                v["document"] = json!({ "id": h.doc_id, "path": h.path, "title": h.title, "revision": h.revision });
                v["anchor"] = json!(h.anchors.last());
            }
            "rule" => v["rule_id"] = json!(h.rule_id),
            _ => v["decision_id"] = json!(h.decision_id),
        }
        if !over.is_empty() {
            v["overridden_by"] = json!(
                over.iter()
                    .map(|d| json!({ "id": d.id, "statement": d.statement, "reason": d.reason }))
                    .collect::<Vec<_>>()
            );
        }
        results.push((st == Standing::Outdated, *score, v));
    }
    // The most relevant, then outdated ones drop below everything current
    // (spec 6.3): an overridden section still shows, under what overrides it.
    results.truncate(limit);
    results.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)));
    Ok(json!({
        "results": results.into_iter().map(|(_, _, v)| v).collect::<Vec<_>>(),
        "meaning": meaning,
        "embeddings": embedder.map(Embedder::model),
        "note": note,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_rewards_agreement() {
        // 3 is second for words and first for meaning: it wins.
        let f = fuse(&[vec![1, 3, 2], vec![3, 4]]);
        let order: Vec<i64> = f.iter().map(|(id, _)| *id).collect();
        assert_eq!(order, [3, 1, 4, 2]);
        assert!(f[0].1 < 1.0 && f[0].1 > 0.95, "{}", f[0].1);
        // One list on its own: its order, the first scoring 1.
        let f = fuse(&[vec![5, 6], vec![]]);
        assert_eq!(f.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [5, 6]);
        assert!(
            (f[0].1 - 1.0).abs() < 1e-9 && (f[1].1 - 61.0 / 62.0).abs() < 1e-9,
            "{f:?}"
        );
        assert!(fuse(&[vec![], vec![]]).is_empty());
    }

    #[test]
    fn any_of_the_words() {
        assert_eq!(
            any_words("Can guests check-out?").as_deref(),
            Some("can | guests | check | out")
        );
        assert_eq!(any_words("  ?! "), None);
        assert_eq!(
            any_words("x' | !y & (z)").as_deref(),
            Some("x | y | z"),
            "no operators get through"
        );
    }
}
