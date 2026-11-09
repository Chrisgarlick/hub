//! The search index (spec section 6.1): chunks of documents, rules and
//! decisions, rebuilt by a background worker from a queue. Chunks whose
//! text hasn't changed keep their embedding, so small edits are cheap.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tokio::sync::Notify;

use crate::documents;
use crate::embed::{self, Embedder};
use crate::error::AppResult;

/// A section longer than this (about 800 tokens) is split on paragraphs,
/// into pieces of about this many characters.
const MAX_CHARS: usize = 3200;
const PIECE_CHARS: usize = 2400;
/// Jobs per pass, so one pass doesn't run forever.
const JOBS_PER_PASS: usize = 50;
const EMBED_BATCHES_PER_PASS: usize = 10;

// ---------- Chunking ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub heading_path: String,
    pub anchors: Vec<String>,
    pub text: String,
}

/// Split a document at its headings (and long sections at paragraphs).
pub fn chunk(title: &str, content: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, String, String)> = Vec::new(); // (level, text, anchor)
    let mut seen: Vec<String> = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut fenced = false;

    let flush = |stack: &[(usize, String, String)], lines: &mut Vec<&str>, out: &mut Vec<Piece>| {
        let text = lines.join("\n").trim().to_string();
        lines.clear();
        if text.is_empty() {
            return;
        }
        let mut path: Vec<&str> = vec![title];
        for (level, h, _) in stack {
            // The H1 is usually the title again.
            if !(*level == 1 && h.eq_ignore_ascii_case(title)) {
                path.push(h);
            }
        }
        let heading_path = path.join(" › ");
        let anchors: Vec<String> = stack.iter().map(|(_, _, a)| a.clone()).collect();
        for t in split(&text) {
            out.push(Piece {
                heading_path: heading_path.clone(),
                anchors: anchors.clone(),
                text: t,
            });
        }
    };

    for line in documents::body(content).lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            lines.push(line);
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if !fenced && (1..=6).contains(&level) && t[level..].starts_with(' ') {
            let text = t[level..].trim().trim_end_matches('#').trim().to_string();
            let base = documents::slug(&text);
            if !base.is_empty() {
                flush(&stack, &mut lines, &mut out);
                let mut anchor = base.clone();
                let mut n = 2;
                while seen.contains(&anchor) {
                    anchor = format!("{base}-{n}");
                    n += 1;
                }
                seen.push(anchor.clone());
                stack.retain(|(l, _, _)| *l < level);
                stack.push((level, text, anchor));
                continue;
            }
        }
        lines.push(line);
    }
    flush(&stack, &mut lines, &mut out);
    out
}

/// A long section as pieces, breaking between paragraphs (and inside a
/// very long paragraph, between sentences or words).
fn split(text: &str) -> Vec<String> {
    if text.len() <= MAX_CHARS {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !cur.is_empty() && cur.len() + para.len() + 2 > PIECE_CHARS {
            out.push(std::mem::take(&mut cur));
        }
        if para.len() > MAX_CHARS {
            for part in hard_split(para) {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur = part;
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn hard_split(para: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in para.split_inclusive(char::is_whitespace) {
        if cur.len() + word.len() > PIECE_CHARS && !cur.is_empty() {
            out.push(std::mem::take(&mut cur).trim().to_string());
        }
        if word.len() > PIECE_CHARS {
            // One enormous "word" (a URL, base64): cut it by characters.
            let chars: Vec<char> = word.chars().collect();
            for piece in chars.chunks(PIECE_CHARS) {
                out.push(piece.iter().collect::<String>().trim().to_string());
            }
            continue;
        }
        cur.push_str(word);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// What gets embedded: the heading path gives the text its context.
pub fn embed_text(heading_path: &str, text: &str) -> String {
    format!("{heading_path}\n\n{text}")
}

fn hash(heading_path: &str, text: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(embed_text(heading_path, text).as_bytes()))
}

// ---------- The queue ----------

/// Ask for a document's chunks to be rebuilt.
pub async fn queue_document(conn: &mut PgConnection, doc_id: &str) -> AppResult<()> {
    queue(conn, "document", doc_id).await
}

/// Ask for a library item's chunks to be rebuilt.
pub async fn queue_library(conn: &mut PgConnection, item_id: &str) -> AppResult<()> {
    queue(conn, "library", item_id).await
}

/// Ask for a space's rule and decision chunks to be rebuilt.
pub async fn queue_space(conn: &mut PgConnection, space_id: &str) -> AppResult<()> {
    queue(conn, "space", space_id).await
}

async fn queue(conn: &mut PgConnection, kind: &str, target: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO index_queue (kind, target) VALUES ($1, $2)
         ON CONFLICT (kind, target) DO UPDATE SET queued_at = now(), next_at = now(), attempts = 0, last_error = NULL",
    )
    .bind(kind)
    .bind(target)
    .execute(conn)
    .await?;
    Ok(())
}

// ---------- Rebuilding ----------

/// What an unchanged chunk keeps: its embedding and who wrote it.
struct Kept {
    embedding: Option<String>,
    model: Option<String>,
    written_by: String,
}

async fn kept(conn: &mut PgConnection, filter: &str, id: &str) -> AppResult<HashMap<String, Kept>> {
    let rows: Vec<(String, Option<String>, Option<String>, String)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT text_hash, embedding::text, embed_model, written_by FROM chunks WHERE {filter}"
        )))
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(h, embedding, model, written_by)| {
            (
                h,
                Kept {
                    embedding,
                    model,
                    written_by,
                },
            )
        })
        .collect())
}

/// One chunk to write.
struct Row<'a> {
    space_id: &'a str,
    source: &'static str,
    doc_id: Option<&'a str>,
    library_id: Option<&'a str>,
    rule_id: Option<&'a str>,
    decision_id: Option<&'a str>,
    ord: i32,
    revision: Option<i32>,
    heading_path: String,
    anchors: Vec<String>,
    text: String,
    doc_type: String,
    applies_to: Vec<String>,
    status: Option<String>,
    written_by: String,
    secret: bool,
}

async fn insert(conn: &mut PgConnection, r: Row<'_>, old: &HashMap<String, Kept>) -> AppResult<()> {
    let h = hash(&r.heading_path, &r.text);
    let k = old.get(&h);
    let (embedding, model) = match k {
        Some(k) if !r.secret => (k.embedding.clone(), k.model.clone()),
        _ => (None, None),
    };
    // Unchanged text keeps its author: your review isn't undone by
    // someone else's edit elsewhere in the document.
    let written_by = k.map_or(r.written_by, |k| k.written_by.clone());
    sqlx::query(
        "INSERT INTO chunks (space_id, source, doc_id, rule_id, decision_id, ord, revision, heading_path, anchors,
                             text, text_hash, doc_type, applies_to, status, written_by, secret, embedding, embed_model,
                             library_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17::vector, $18, $19)",
    )
    .bind(r.space_id)
    .bind(r.source)
    .bind(r.doc_id)
    .bind(r.rule_id)
    .bind(r.decision_id)
    .bind(r.ord)
    .bind(r.revision)
    .bind(&r.heading_path)
    .bind(&r.anchors)
    .bind(&r.text)
    .bind(&h)
    .bind(&r.doc_type)
    .bind(&r.applies_to)
    .bind(&r.status)
    .bind(&written_by)
    .bind(r.secret)
    .bind(embedding)
    .bind(model)
    .bind(r.library_id)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn rebuild_document(conn: &mut PgConnection, doc_id: &str) -> AppResult<()> {
    type DocRow = (
        String,
        String,
        String,
        i32,
        String,
        String,
        Option<DateTime<Utc>>,
        String,
    );
    let doc: Option<DocRow> = sqlx::query_as(
        "SELECT space_id, title, content, version, visibility, doc_type, deleted_at, updated_by FROM documents WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(&mut *conn)
    .await?;
    let old = kept(conn, "doc_id = $1", doc_id).await?;
    sqlx::query("DELETE FROM chunks WHERE doc_id = $1")
        .bind(doc_id)
        .execute(&mut *conn)
        .await?;
    let Some((space_id, title, content, version, visibility, doc_type, None, updated_by)) = doc
    else {
        return Ok(()); // gone, or in the trash: nothing to find
    };
    for (i, p) in chunk(&title, &content).into_iter().enumerate() {
        insert(
            conn,
            Row {
                space_id: &space_id,
                source: "document",
                doc_id: Some(doc_id),
                library_id: None,
                rule_id: None,
                decision_id: None,
                ord: i as i32,
                revision: Some(version),
                heading_path: p.heading_path,
                anchors: p.anchors,
                text: p.text,
                doc_type: doc_type.clone(),
                applies_to: vec!["all".into()],
                status: None,
                written_by: updated_by.clone(),
                secret: visibility == "secret",
            },
            &old,
        )
        .await?;
    }
    Ok(())
}

/// A library item, split like a document: found by its words and meaning.
pub async fn rebuild_library(conn: &mut PgConnection, item_id: &str) -> AppResult<()> {
    type ItemRow = (
        String,
        String,
        String,
        String,
        String,
        i32,
        Option<DateTime<Utc>>,
    );
    let item: Option<ItemRow> = sqlx::query_as(
        "SELECT space_id, kind, title, body, updated_by, version, deleted_at FROM library_items WHERE id = $1",
    )
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?;
    let old = kept(conn, "library_id = $1", item_id).await?;
    sqlx::query("DELETE FROM chunks WHERE library_id = $1")
        .bind(item_id)
        .execute(&mut *conn)
        .await?;
    let Some((space_id, kind, title, body, updated_by, version, None)) = item else {
        return Ok(());
    };
    for (i, p) in chunk(&title, &body).into_iter().enumerate() {
        insert(
            conn,
            Row {
                space_id: &space_id,
                source: "library",
                doc_id: None,
                library_id: Some(item_id),
                rule_id: None,
                decision_id: None,
                ord: i as i32,
                revision: Some(version),
                heading_path: p.heading_path,
                anchors: p.anchors,
                text: p.text,
                doc_type: kind.clone(),
                applies_to: vec!["all".into()],
                status: None,
                written_by: updated_by.clone(),
                secret: false,
            },
            &old,
        )
        .await?;
    }
    Ok(())
}

/// id, kind, applies_to, text, status, created_by, approved_by
type RuleRow = (
    String,
    String,
    Vec<String>,
    String,
    String,
    String,
    Option<String>,
);
/// id, statement, reason, applies_to, status, proposed_by, decided_by
type DecisionRow = (
    String,
    String,
    String,
    Vec<String>,
    String,
    String,
    Option<String>,
);

/// A space's rules and decisions, a chunk each.
pub async fn rebuild_space(conn: &mut PgConnection, space_id: &str) -> AppResult<()> {
    let old = kept(conn, "space_id = $1 AND source <> 'document'", space_id).await?;
    sqlx::query("DELETE FROM chunks WHERE space_id = $1 AND source <> 'document'")
        .bind(space_id)
        .execute(&mut *conn)
        .await?;
    let rules: Vec<RuleRow> = sqlx::query_as(
        "SELECT id, kind, applies_to, text, status, created_by, approved_by FROM rules
         WHERE space_id = $1 AND status <> 'retired' ORDER BY created_at",
    )
    .bind(space_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, kind, applies_to, text, status, created_by, approved_by) in rules {
        let label = match kind.as_str() {
            "must" => "Must",
            "must_not" => "Must not",
            "avoid" => "Avoid",
            _ => "Prefer",
        };
        insert(
            conn,
            Row {
                space_id,
                source: "rule",
                doc_id: None,
                library_id: None,
                rule_id: Some(&id),
                decision_id: None,
                ord: 0,
                revision: None,
                heading_path: format!("Rules › {label}"),
                anchors: vec![],
                text,
                doc_type: "rule".into(),
                applies_to,
                status: Some(status),
                // "auto": in force, but you haven't reviewed it.
                written_by: approved_by.filter(|a| a != "auto").unwrap_or(created_by),
                secret: false,
            },
            &old,
        )
        .await?;
    }
    let decisions: Vec<DecisionRow> = sqlx::query_as(
        "SELECT id, statement, reason, applies_to, status, proposed_by, decided_by FROM decisions
         WHERE space_id = $1 AND status <> 'rejected' ORDER BY created_at",
    )
    .bind(space_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, statement, reason, applies_to, status, proposed_by, decided_by) in decisions {
        insert(
            conn,
            Row {
                space_id,
                source: "decision",
                doc_id: None,
                library_id: None,
                rule_id: None,
                decision_id: Some(&id),
                ord: 0,
                revision: None,
                heading_path: "Decisions".into(),
                anchors: vec![],
                text: format!("{statement}\n\n{reason}"),
                doc_type: "decision".into(),
                applies_to,
                status: Some(status),
                written_by: decided_by.filter(|a| a != "auto").unwrap_or(proposed_by),
                secret: false,
            },
            &old,
        )
        .await?;
    }
    Ok(())
}

// ---------- The worker ----------

/// What one pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub jobs: usize,
    pub embedded: usize,
}

/// Run queued jobs, then embed chunks that have no embedding yet.
pub async fn run_once(pool: &PgPool, embedder: Option<&Embedder>) -> anyhow::Result<Pass> {
    let mut pass = Pass::default();
    for _ in 0..JOBS_PER_PASS {
        let mut tx = pool.begin().await?;
        let job: Option<(String, String)> = sqlx::query_as(
            "SELECT kind, target FROM index_queue WHERE next_at <= now()
             ORDER BY queued_at LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((kind, target)) = job else { break };
        let done = match kind.as_str() {
            "document" => rebuild_document(&mut tx, &target).await,
            "library" => rebuild_library(&mut tx, &target).await,
            _ => rebuild_space(&mut tx, &target).await,
        };
        match done {
            Ok(()) => {
                sqlx::query("DELETE FROM index_queue WHERE kind = $1 AND target = $2")
                    .bind(&kind)
                    .bind(&target)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                pass.jobs += 1;
            }
            Err(e) => {
                tx.rollback().await?;
                tracing::warn!(kind, target, error = %e, "index job failed; will retry");
                sqlx::query(
                    "UPDATE index_queue SET attempts = attempts + 1, last_error = $3,
                       next_at = now() + make_interval(secs => least(3600, 30 * power(2, attempts)))
                     WHERE kind = $1 AND target = $2",
                )
                .bind(&kind)
                .bind(&target)
                .bind(e.to_string())
                .execute(pool)
                .await?;
            }
        }
    }
    if let Some(embedder) = embedder {
        let model = embedder.model();
        let max_chars = embedder.limits().chars_per_request;
        for _ in 0..EMBED_BATCHES_PER_PASS {
            let rows: Vec<(i64, String, String)> = sqlx::query_as(
                "SELECT id, heading_path, text FROM chunks WHERE embedding IS NULL AND NOT secret ORDER BY id LIMIT $1",
            )
            .bind(embed::BATCH as i64)
            .fetch_all(pool)
            .await?;
            if rows.is_empty() {
                break;
            }
            // As many as fit in one request's share of the token limit
            // (always at least one; a chunk is at most about 800 tokens).
            let mut texts: Vec<String> = Vec::new();
            let mut chars = 0;
            for (_, h, t) in &rows {
                let text = embed_text(h, t);
                if !texts.is_empty() && chars + text.len() > max_chars {
                    break;
                }
                chars += text.len();
                texts.push(text);
            }
            let vectors = embedder.embed_documents(&texts).await?;
            for ((id, _, _), v) in rows.iter().zip(vectors) {
                // The chunk may have been rebuilt meanwhile; then it's a new row.
                sqlx::query(
                    "UPDATE chunks SET embedding = $1::vector, embed_model = $2 WHERE id = $3",
                )
                .bind(embed::literal(&v))
                .bind(&model)
                .bind(id)
                .execute(pool)
                .await?;
            }
            pass.embedded += texts.len();
        }
    }
    Ok(pass)
}

/// Keep the index up to date, forever: a pass whenever something is
/// queued (or every few seconds), backing off when embedding fails.
pub fn spawn_worker(pool: PgPool, embedder: Option<Embedder>, wake: Arc<Notify>) {
    tokio::spawn(async move {
        loop {
            let wait = match run_once(&pool, embedder.as_ref()).await {
                Ok(p) => {
                    if p.jobs > 0 || p.embedded > 0 {
                        tracing::debug!(jobs = p.jobs, embedded = p.embedded, "indexed");
                    }
                    // More waiting: go again at once.
                    if p.jobs == JOBS_PER_PASS || p.embedded > 0 {
                        continue;
                    }
                    Duration::from_secs(5)
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "indexing paused for a minute");
                    Duration::from_secs(60)
                }
            };
            let _ = tokio::time::timeout(wait, wake.notified()).await;
        }
    });
}

/// `hub reindex`: drop every embedding and rebuild every chunk, e.g. after
/// changing the embedding model.
pub async fn reindex(pool: &PgPool) -> AppResult<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE chunks SET embedding = NULL, embed_model = NULL")
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query(
        "INSERT INTO index_queue (kind, target)
           SELECT 'document', id FROM documents WHERE deleted_at IS NULL
           UNION ALL SELECT 'space', id FROM spaces WHERE archived_at IS NULL
           UNION ALL SELECT 'library', id FROM library_items WHERE deleted_at IS NULL
         ON CONFLICT (kind, target) DO UPDATE SET queued_at = now(), next_at = now(), attempts = 0",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// For the UI and `/api/index`: how far along the index is.
pub async fn status(pool: &PgPool, embedder: Option<&Embedder>) -> AppResult<serde_json::Value> {
    let (chunks, embedded, secret): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), count(embedding), count(*) FILTER (WHERE secret) FROM chunks",
    )
    .fetch_one(pool)
    .await?;
    let (queued, failing): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(last_error) FROM index_queue")
            .fetch_one(pool)
            .await?;
    Ok(serde_json::json!({
        "embeddings": embedder.map(Embedder::model),
        "chunks": chunks,
        "embedded": embedded,
        "words_only": secret,
        "queued": queued,
        "failing": failing,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_at_headings_with_their_path() {
        let doc = "---\ntitle: Constraints\n---\n# Constraints\n\nIntro.\n\n## Accounts\n\nGuests need one.\n\n\
                   ### Guests\n\nNo, they don't.\n\n## Empty\n\n## Code\n\n```\n# not a heading\n```\n";
        let c = chunk("Constraints", doc);
        let got: Vec<(&str, Vec<&str>, &str)> = c
            .iter()
            .map(|p| {
                (
                    p.heading_path.as_str(),
                    p.anchors.iter().map(String::as_str).collect(),
                    p.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("Constraints", vec!["constraints"], "Intro."),
                (
                    "Constraints › Accounts",
                    vec!["constraints", "accounts"],
                    "Guests need one."
                ),
                (
                    "Constraints › Accounts › Guests",
                    vec!["constraints", "accounts", "guests"],
                    "No, they don't."
                ),
                (
                    "Constraints › Code",
                    vec!["constraints", "code"],
                    "```\n# not a heading\n```"
                ),
            ]
        );
    }

    #[test]
    fn long_sections_split_on_paragraphs() {
        let para = "word ".repeat(300); // 1500 chars
        let doc = format!(
            "## Long\n\n{para}\n\n{para}\n\n{para}\n\n{}",
            "x".repeat(5000)
        );
        let c = chunk("Doc", &doc);
        assert!(c.len() >= 3, "{}", c.len());
        assert!(
            c.iter().all(|p| p.text.len() <= MAX_CHARS),
            "{:?}",
            c.iter().map(|p| p.text.len()).collect::<Vec<_>>()
        );
        assert!(c.iter().all(|p| p.heading_path == "Doc › Long"));
    }
}
