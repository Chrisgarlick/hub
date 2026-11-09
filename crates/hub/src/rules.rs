//! Rules (spec section 4): read from a space's Writing rules document on
//! every save, or added directly; checked against drafts by code when they
//! carry a check.
//!
//! In the document, each list item is a rule:
//!
//! - Its first words give the kind: "Don't", "Never", "Do not" (must not);
//!   "Always", "Must", "Include" (must); "Avoid", "Try not to" (avoid);
//!   anything else (prefer).
//! - The heading it sits under says where it applies: "Reports and
//!   proposals" is Typeset, "Website" or "Copy" is Design, and so on; no
//!   match is everywhere. A trailing `[typeset, design]` says it outright.
//! - Words in double quotes become the check: terms that mustn't appear (for
//!   a must not or avoid), terms that must (for a must), or, for a prefer,
//!   the ones after "over", "to", "instead of" or "rather than". A
//!   `` `/regex/` `` is a pattern. "(case-sensitive)" makes either exact.
//!
//! A rule keeps its id while its text is unchanged, or while it's edited in
//! place (same heading, same position), so exceptions and links to it last.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use hub_model::{Check, RuleKind, Span, Standing, TARGETS};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::documents;
use crate::error::{AppError, AppResult};
use crate::spaces::STUDIO;

/// The document type whose list items are rules.
pub const RULES_DOC_TYPE: &str = "writing-rules";
const MAX_RULE_CHARS: usize = 1000;
const MAX_PATTERN_BYTES: usize = 500;

/// Written or approved by you (a browser session, or Hub's own templates),
/// as opposed to a Clients key.
pub fn by_you(by: &str) -> bool {
    by == "user" || by == "hub"
}

// ---------- Reading rules from a document ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub kind: RuleKind,
    pub applies_to: Vec<String>,
    pub text: String,
    pub check: Option<Check>,
    pub warning: Option<String>,
    pub anchor: Option<String>,
    pub index: i32,
}

/// The rules in a Writing rules document, in order.
pub fn parse(content: &str) -> Vec<Parsed> {
    let mut out = Vec::new();
    let mut anchors: Vec<String> = Vec::new();
    let mut heading: Option<(String, String)> = None; // (text, anchor)
    let mut fenced = false;
    let mut item: Option<String> = None;

    let flush =
        |item: &mut Option<String>, heading: &Option<(String, String)>, out: &mut Vec<Parsed>| {
            if let Some(text) = item.take()
                && let Some(p) = parse_item(&text, heading.as_ref(), out.len() as i32)
            {
                out.push(p);
            }
        };

    for line in body(content).lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut item, &heading, &mut out);
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && t[level..].starts_with(' ') {
            flush(&mut item, &heading, &mut out);
            let text = t[level..].trim().trim_end_matches('#').trim().to_string();
            let base = documents::slug(&text);
            if !base.is_empty() {
                let mut anchor = base.clone();
                let mut n = 2;
                while anchors.contains(&anchor) {
                    anchor = format!("{base}-{n}");
                    n += 1;
                }
                anchors.push(anchor.clone());
                heading = Some((text, anchor));
            }
            continue;
        }
        if let Some(rest) = list_item(t) {
            flush(&mut item, &heading, &mut out);
            item = Some(rest.to_string());
        } else if t.is_empty() {
            flush(&mut item, &heading, &mut out);
        } else if let Some(text) = item.as_mut()
            && line.starts_with([' ', '\t'])
        {
            text.push(' ');
            text.push_str(t);
        } else {
            flush(&mut item, &heading, &mut out);
        }
    }
    flush(&mut item, &heading, &mut out);
    out
}

/// The content without its frontmatter.
fn body(content: &str) -> &str {
    if let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        && let Some(end) = rest.find("\n---")
    {
        let after = &rest[end + 4..];
        return after.split_once('\n').map_or("", |(_, b)| b);
    }
    content
}

fn list_item(t: &str) -> Option<&str> {
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return Some(r);
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let r = &t[digits..];
        return r.strip_prefix(". ").or_else(|| r.strip_prefix(") "));
    }
    // A bare "-" (the templates' empty items).
    (t == "-" || t == "*").then_some("")
}

fn parse_item(raw: &str, heading: Option<&(String, String)>, index: i32) -> Option<Parsed> {
    let mut text = raw.trim().to_string();
    // A task-list box is just decoration here.
    for b in ["[ ] ", "[x] ", "[X] "] {
        if let Some(r) = text.strip_prefix(b) {
            text = r.to_string();
        }
    }
    // An explicit target list at the end: "… [typeset, design]".
    let mut applies_to = None;
    if text.ends_with(']')
        && let Some(open) = text.rfind('[')
    {
        let inside = &text[open + 1..text.len() - 1];
        let names: Vec<String> = inside
            .split([',', ' '])
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if !names.is_empty() && names.iter().all(|n| TARGETS.contains(&n.as_str())) {
            applies_to = Some(names);
            text = text[..open].trim_end().to_string();
        }
    }
    let case_sensitive = ["(case-sensitive)", "(case sensitive)"]
        .iter()
        .any(|m| text.to_lowercase().contains(m));
    for m in [
        "(case-sensitive)",
        "(case sensitive)",
        "(Case-sensitive)",
        "(Case sensitive)",
    ] {
        text = text.replace(m, "");
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().all(|c| !c.is_alphanumeric()) {
        return None;
    }
    let kind = kind_of(&text);
    let applies_to = applies_to.unwrap_or_else(|| targets_for(heading.map(|h| h.0.as_str())));
    let (check, warning) = check_from(&text, kind, case_sensitive);
    Some(Parsed {
        kind,
        applies_to,
        text: text.chars().take(MAX_RULE_CHARS).collect(),
        check,
        warning,
        anchor: heading.map(|h| h.1.clone()),
        index,
    })
}

fn kind_of(text: &str) -> RuleKind {
    // Emphasis doesn't change the kind: "**Never** …" is "Never …".
    let t = text
        .replace(['*', '_'], "")
        .to_lowercase()
        .replace('’', "'");
    let starts = |words: &[&str]| {
        words
            .iter()
            .any(|w| t == *w || t.starts_with(&format!("{w} ")))
    };
    if starts(&[
        "don't", "dont", "do not", "never", "no", "must not", "mustn't", "not",
    ]) {
        RuleKind::MustNot
    } else if starts(&["always", "must", "include", "mention", "do"]) {
        RuleKind::Must
    } else if starts(&[
        "avoid",
        "try not to",
        "steer clear of",
        "minimise",
        "minimize",
    ]) {
        RuleKind::Avoid
    } else {
        RuleKind::Prefer
    }
}

/// Where a rule under this heading applies.
fn targets_for(heading: Option<&str>) -> Vec<String> {
    let h = heading.unwrap_or("").to_lowercase();
    let words: Vec<&str> = h.split(|c: char| !c.is_alphanumeric()).collect();
    let has = |list: &[&str]| words.iter().any(|w| list.iter().any(|l| w.starts_with(l)));
    let mut out = Vec::new();
    if has(&[
        "report", "proposal", "pdf", "document", "print", "invoice", "quote",
    ]) {
        out.push("typeset");
    }
    if has(&["design", "website", "site", "web", "copy", "page", "ui"]) {
        out.push("design");
    }
    if has(&["board", "plan", "sitemap"]) {
        out.push("board");
    }
    if has(&[
        "motion",
        "video",
        "animation",
        "social",
        "reel",
        "story",
        "stories",
    ]) {
        out.push("motion");
    }
    if out.is_empty() {
        out.push("all");
    }
    out.into_iter().map(String::from).collect()
}

/// The double-quoted phrases in a rule, and backticked `/patterns/`.
fn quoted(text: &str) -> (Vec<String>, Vec<String>) {
    let mut terms = Vec::new();
    let mut patterns = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let close = match chars[i] {
            '"' => '"',
            '“' => '”',
            '`' => '`',
            _ => {
                i += 1;
                continue;
            }
        };
        let open = chars[i];
        if let Some(len) = chars[i + 1..]
            .iter()
            .position(|c| *c == close || (close == '”' && *c == '"'))
        {
            let inner: String = chars[i + 1..i + 1 + len].iter().collect();
            let inner = inner.trim().to_string();
            if open == '`' {
                if inner.len() > 2 && inner.starts_with('/') && inner.ends_with('/') {
                    patterns.push(inner[1..inner.len() - 1].to_string());
                } else if !inner.is_empty() {
                    terms.push(inner);
                }
            } else if inner.chars().any(char::is_alphanumeric) {
                terms.push(inner);
            }
            i += len + 2;
        } else {
            i += 1;
        }
    }
    (terms, patterns)
}

fn check_from(text: &str, kind: RuleKind, case_sensitive: bool) -> (Option<Check>, Option<String>) {
    let (terms, patterns) = match kind {
        RuleKind::Prefer => {
            // "Prefer "car park" to "parking lot"": the second is to avoid.
            let lower = text.to_lowercase();
            let cut = [" over ", " to ", " instead of ", " rather than ", " not "]
                .iter()
                .filter_map(|s| lower.find(s).map(|i| i + s.len()))
                .min();
            match cut {
                Some(i) => quoted(&text[i..]),
                None => (vec![], vec![]),
            }
        }
        _ => quoted(text),
    };
    let mut warning = None;
    let pattern = patterns
        .into_iter()
        .next()
        .and_then(|p| match compile(&p, case_sensitive) {
            Ok(_) => Some(p),
            Err(e) => {
                warning = Some(format!(
                    "the pattern `/{p}/` isn't valid ({e}), so it isn't checked"
                ));
                None
            }
        });
    let mut check = Check {
        case_sensitive,
        ..Check::default()
    };
    if kind == RuleKind::Must {
        check.required_terms = terms;
        if pattern.is_some() {
            warning =
                Some("a pattern can only say what mustn't appear, so it isn't checked".into());
        }
    } else {
        check.terms = terms;
        check.pattern = pattern;
    }
    (if check.is_empty() { None } else { Some(check) }, warning)
}

// ---------- Checking text ----------

fn compile(pattern: &str, case_sensitive: bool) -> Result<Regex, String> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("longer than {MAX_PATTERN_BYTES} bytes"));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| {
            e.to_string()
                .lines()
                .last()
                .unwrap_or("invalid")
                .trim()
                .to_string()
        })
}

/// Whole-word matches of a term (its words may be split by any whitespace).
fn term_matches(text: &str, term: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    let words: Vec<String> = term.split_whitespace().map(regex::escape).collect();
    if words.is_empty() {
        return vec![];
    }
    let Ok(re) = compile(&words.join(r"\s+"), case_sensitive) else {
        return vec![];
    };
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let first_word = word(term.chars().next());
    let last_word = word(term.chars().last());
    re.find_iter(text)
        .filter(|m| {
            (!first_word || !word(text[..m.start()].chars().next_back()))
                && (!last_word || !word(text[m.end()..].chars().next()))
        })
        .map(|m| (m.start(), m.end()))
        .collect()
}

fn span(text: &str, start: usize, end: usize) -> Span {
    let before = &text[..start];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Span {
        start,
        end,
        line,
        column: text[line_start..start].chars().count() + 1,
        text: text[start..end].to_string(),
    }
}

/// What a check found for one rule: where it matched (for terms and
/// patterns), or nothing (a required term that's missing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub spans: Vec<Span>,
    pub missing: Vec<String>,
}

/// Run one rule's check over a text. `None` means it holds.
pub fn run(check: &Check, text: &str) -> Option<Found> {
    let mut spans = Vec::new();
    for term in &check.terms {
        for (s, e) in term_matches(text, term, check.case_sensitive) {
            spans.push(span(text, s, e));
        }
    }
    if let Some(p) = &check.pattern
        && let Ok(re) = compile(p, check.case_sensitive)
    {
        for m in re.find_iter(text).filter(|m| !m.is_empty()).take(200) {
            spans.push(span(text, m.start(), m.end()));
        }
    }
    let missing: Vec<String> = check
        .required_terms
        .iter()
        .filter(|t| term_matches(text, t, check.case_sensitive).is_empty())
        .cloned()
        .collect();
    spans.sort_by_key(|s| s.start);
    spans.dedup_by_key(|s| s.start);
    (!spans.is_empty() || !missing.is_empty()).then_some(Found { spans, missing })
}

// ---------- Storage ----------

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Rule {
    pub id: String,
    pub space_id: String,
    pub kind: String,
    pub applies_to: Vec<String>,
    pub text: String,
    pub check: Option<Json<Check>>,
    pub warning: Option<String>,
    pub source_doc: Option<String>,
    pub source_anchor: Option<String>,
    pub source_index: Option<i32>,
    pub status: String,
    pub created_by: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Rule {
    pub fn kind(&self) -> RuleKind {
        RuleKind::parse(&self.kind).unwrap_or(RuleKind::Prefer)
    }

    pub fn check(&self) -> Option<&Check> {
        self.check.as_ref().map(|c| &c.0)
    }

    pub fn is_hard(&self) -> bool {
        self.check().is_some() && self.kind().is_strict()
    }

    pub fn standing(&self) -> Standing {
        match self.status.as_str() {
            "approved" => Standing::Authoritative,
            "proposed" => Standing::Tentative,
            _ => Standing::Outdated,
        }
    }

    pub fn applies(&self, target: Option<&str>) -> bool {
        target.is_none_or(|t| self.applies_to.iter().any(|a| a == "all" || a == t))
    }

    /// As the API returns it.
    pub fn json(&self, source_path: Option<&str>, excepted: Option<&str>) -> Value {
        let mut v = json!(self);
        v["check"] = json!(self.check());
        v["hard"] = json!(self.is_hard());
        v["standing"] = json!(self.standing());
        v["scope"] = json!(if self.space_id == STUDIO {
            "studio"
        } else {
            "client"
        });
        if let Some(p) = source_path {
            v["source_path"] = json!(p);
        }
        if let Some(r) = excepted {
            v["excepted"] = json!({ "reason": r });
        }
        v
    }
}

const COLS: &str = r#"id, space_id, kind, applies_to, text, "check", warning, source_doc, source_anchor,
    source_index, status, created_by, approved_by, approved_at, retired_at, created_at, updated_at"#;

fn new_id() -> String {
    format!("rul_{}", ulid::Ulid::generate())
}

pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Rule> {
    sqlx::query_as::<_, Rule>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM rules WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("rule"))
}

/// Re-read a document's rules after it changed: kept, edited, added,
/// retired. A no-op for documents that aren't Writing rules (beyond
/// retiring rules from a document that no longer is one).
pub async fn sync_document(conn: &mut PgConnection, doc_id: &str, by: &str) -> AppResult<()> {
    let doc: Option<(String, String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT space_id, doc_type, content, deleted_at FROM documents WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((space_id, doc_type, content, deleted)) = doc else {
        return Ok(());
    };
    let parsed = if doc_type == RULES_DOC_TYPE && deleted.is_none() {
        parse(&content)
    } else {
        vec![]
    };
    let old: Vec<Rule> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM rules WHERE source_doc = $1 AND status <> 'retired' ORDER BY source_index"
    )))
    .bind(doc_id)
    .fetch_all(&mut *conn)
    .await?;

    // Match: the same text first, then the same place (an edit in place).
    let mut taken: HashSet<usize> = HashSet::new();
    let mut matched: Vec<Option<usize>> = parsed
        .iter()
        .map(|p| {
            let i = old
                .iter()
                .enumerate()
                .position(|(i, r)| !taken.contains(&i) && r.text == p.text)?;
            taken.insert(i);
            Some(i)
        })
        .collect();
    for (k, p) in parsed.iter().enumerate() {
        if matched[k].is_some() {
            continue;
        }
        let same_place = |r: &Rule| {
            r.source_anchor == p.anchor
                && parsed
                    .iter()
                    .filter(|q| q.anchor == p.anchor && q.index < p.index)
                    .count()
                    == old
                        .iter()
                        .filter(|o| {
                            o.source_anchor == r.source_anchor && o.source_index < r.source_index
                        })
                        .count()
        };
        if let Some(i) = old
            .iter()
            .enumerate()
            .position(|(i, r)| !taken.contains(&i) && same_place(r))
        {
            taken.insert(i);
            matched[k] = Some(i);
        }
    }

    let yours = by_you(by);
    for (p, m) in parsed.iter().zip(&matched) {
        let check = p.check.clone().map(Json);
        match m {
            Some(i) => {
                let r = &old[*i];
                let changed = r.text != p.text
                    || r.kind != p.kind.as_str()
                    || r.applies_to != p.applies_to
                    || r.check() != p.check.as_ref();
                // A change you didn't make waits for you.
                let (status, approved_by): (&str, Option<&str>) = if !changed {
                    (&r.status, r.approved_by.as_deref())
                } else if yours {
                    ("approved", Some(by))
                } else {
                    ("proposed", None)
                };
                sqlx::query(
                    r#"UPDATE rules SET kind = $1, applies_to = $2, text = $3, "check" = $4, warning = $5,
                         source_anchor = $6, source_index = $7, status = $8, approved_by = $9,
                         approved_at = CASE WHEN NOT $10 THEN approved_at WHEN $8 = 'approved' THEN now() END,
                         updated_at = now()
                       WHERE id = $11"#,
                )
                .bind(p.kind.as_str())
                .bind(&p.applies_to)
                .bind(&p.text)
                .bind(check)
                .bind(&p.warning)
                .bind(&p.anchor)
                .bind(p.index)
                .bind(status)
                .bind(approved_by)
                .bind(changed)
                .bind(&r.id)
                .execute(&mut *conn)
                .await?;
            }
            None => {
                sqlx::query(
                    r#"INSERT INTO rules (id, space_id, kind, applies_to, text, "check", warning, source_doc,
                         source_anchor, source_index, status, created_by, approved_by, approved_at)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                               CASE WHEN $11 = 'approved' THEN now() END)"#,
                )
                .bind(new_id())
                .bind(&space_id)
                .bind(p.kind.as_str())
                .bind(&p.applies_to)
                .bind(&p.text)
                .bind(check)
                .bind(&p.warning)
                .bind(doc_id)
                .bind(&p.anchor)
                .bind(p.index)
                .bind(if yours { "approved" } else { "proposed" })
                .bind(by)
                .bind(yours.then_some(by))
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    let gone: Vec<&str> = old
        .iter()
        .enumerate()
        .filter(|(i, _)| !taken.contains(i))
        .map(|(_, r)| r.id.as_str())
        .collect();
    if !gone.is_empty() {
        sqlx::query("UPDATE rules SET status = 'retired', retired_at = now(), updated_at = now() WHERE id = ANY($1)")
            .bind(&gone)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Re-read every Writing rules document (at start-up: picks up documents
/// written before rules existed, and any parser change). Idempotent.
pub async fn sync_all(pool: &sqlx::PgPool) -> AppResult<usize> {
    let docs: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, updated_by FROM documents WHERE doc_type = $1 AND deleted_at IS NULL",
    )
    .bind(RULES_DOC_TYPE)
    .fetch_all(pool)
    .await?;
    for (id, by) in &docs {
        let mut tx = pool.begin().await?;
        sync_document(&mut tx, id, by).await?;
        tx.commit().await?;
    }
    Ok(docs.len())
}

/// A rule as it bears on one space.
#[derive(Debug, Clone)]
pub struct InForce {
    pub rule: Rule,
    pub source_path: Option<String>,
    /// Why a studio rule doesn't apply to this client.
    pub excepted: Option<String>,
}

impl InForce {
    pub fn json(&self) -> Value {
        self.rule
            .json(self.source_path.as_deref(), self.excepted.as_deref())
    }
}

/// A space's live rules and the studio's (for a client), proposed ones
/// included; hard rules first, then by where they came from.
pub async fn for_space(conn: &mut PgConnection, space_id: &str) -> AppResult<Vec<InForce>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        rule: Rule,
        source_path: Option<String>,
        excepted: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {cols}, d.path AS source_path, x.reason AS excepted
         FROM rules r
         LEFT JOIN documents d ON d.id = r.source_doc
         LEFT JOIN rule_exceptions x ON x.rule_id = r.id AND x.space_id = $1
         WHERE r.status <> 'retired' AND (r.space_id = $1 OR r.space_id = $2)
         ORDER BY r.space_id = $2, d.path, r.source_index, r.created_at",
        cols = COLS
            .split(',')
            .map(|c| format!("r.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    )))
    .bind(space_id)
    .bind(STUDIO)
    .fetch_all(conn)
    .await?;
    let mut out: Vec<InForce> = rows
        .into_iter()
        .map(|r| InForce {
            rule: r.rule,
            source_path: r.source_path,
            excepted: r.excepted,
        })
        .collect();
    out.sort_by_key(|r| !r.rule.is_hard());
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRule {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    #[serde(default)]
    pub check: Option<Check>,
}

pub fn check_targets(list: Option<Vec<String>>) -> AppResult<Vec<String>> {
    let list = list.unwrap_or_else(|| vec!["all".into()]);
    if list.is_empty() {
        return Err(AppError::invalid(
            "applies_to",
            "say where it applies, or `all`",
        ));
    }
    for t in &list {
        if !TARGETS.contains(&t.as_str()) {
            return Err(AppError::invalid(
                "applies_to",
                format!("`{t}` isn't one of {}", TARGETS.join(", ")),
            ));
        }
    }
    Ok(list)
}

fn valid_rule(b: &NewRule) -> AppResult<(RuleKind, Vec<String>, String, Option<Check>)> {
    let kind = RuleKind::parse(&b.kind)
        .ok_or_else(|| AppError::invalid("kind", "use must, must_not, prefer or avoid"))?;
    let text = b.text.trim().to_string();
    if text.is_empty() || text.chars().count() > MAX_RULE_CHARS {
        return Err(AppError::invalid(
            "text",
            format!("write the rule in up to {MAX_RULE_CHARS} characters"),
        ));
    }
    let applies_to = check_targets(b.applies_to.clone())?;
    let check = b.check.clone().filter(|c| !c.is_empty());
    if let Some(c) = &check {
        if let Some(p) = &c.pattern {
            compile(p, c.case_sensitive).map_err(|e| AppError::invalid("check.pattern", e))?;
        }
        if kind == RuleKind::Must && (!c.terms.is_empty() || c.pattern.is_some()) {
            return Err(AppError::invalid(
                "check",
                "a must rule checks required_terms",
            ));
        }
        if kind != RuleKind::Must && !c.required_terms.is_empty() {
            return Err(AppError::invalid(
                "check",
                "only a must rule has required_terms",
            ));
        }
    }
    Ok((kind, applies_to, text, check))
}

/// Add a rule directly: approved if you add it, proposed if a key does.
pub async fn create(
    conn: &mut PgConnection,
    space_id: &str,
    b: NewRule,
    by: &str,
) -> AppResult<Rule> {
    let (kind, applies_to, text, check) = valid_rule(&b)?;
    let id = new_id();
    let yours = by_you(by);
    sqlx::query(
        r#"INSERT INTO rules (id, space_id, kind, applies_to, text, "check", status, created_by, approved_by, approved_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CASE WHEN $7 = 'approved' THEN now() END)"#,
    )
    .bind(&id)
    .bind(space_id)
    .bind(kind.as_str())
    .bind(&applies_to)
    .bind(&text)
    .bind(check.map(Json))
    .bind(if yours { "approved" } else { "proposed" })
    .bind(by)
    .bind(yours.then_some(by))
    .execute(&mut *conn)
    .await?;
    get(conn, &id).await
}

fn from_document(r: &Rule) -> AppResult<()> {
    if r.source_doc.is_some() {
        return Err(AppError::State {
            code: "from_document",
            message: "this rule comes from a Writing rules document; change it there".into(),
        });
    }
    Ok(())
}

/// Change a rule added directly. You can change any; a key only the
/// proposals it made, which stay proposed.
pub async fn update(conn: &mut PgConnection, id: &str, b: NewRule, by: &str) -> AppResult<Rule> {
    let r = get(conn, id).await?;
    from_document(&r)?;
    if r.status == "retired" {
        return Err(AppError::NotFound("rule"));
    }
    let yours = by_you(by);
    if !yours && !(r.status == "proposed" && r.created_by == by) {
        return Err(AppError::Forbidden(
            "only your own proposed rules can be changed with a key",
        ));
    }
    let (kind, applies_to, text, check) = valid_rule(&b)?;
    sqlx::query(
        r#"UPDATE rules SET kind = $1, applies_to = $2, text = $3, "check" = $4, updated_at = now() WHERE id = $5"#,
    )
    .bind(kind.as_str())
    .bind(&applies_to)
    .bind(&text)
    .bind(check.map(Json))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    get(conn, id).await
}

/// Retire a rule added directly (you), or withdraw a proposal (its key).
pub async fn retire(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Rule> {
    let r = get(conn, id).await?;
    from_document(&r)?;
    if !by_you(by) && !(r.status == "proposed" && r.created_by == by) {
        return Err(AppError::Forbidden(
            "only your own proposed rules can be withdrawn with a key",
        ));
    }
    sqlx::query(
        "UPDATE rules SET status = 'retired', retired_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    get(conn, id).await
}

/// Approve a proposed rule. Only you, in a browser.
pub async fn approve(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Rule> {
    let r = get(conn, id).await?;
    if r.status != "proposed" {
        return Err(AppError::State {
            code: "not_proposed",
            message: format!("this rule is {}, not waiting for approval", r.status),
        });
    }
    sqlx::query("UPDATE rules SET status = 'approved', approved_by = $1, approved_at = now(), updated_at = now() WHERE id = $2")
        .bind(by)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    get(conn, id).await
}

/// Say a studio rule doesn't apply to this client (or that it does again).
pub async fn set_exception(
    conn: &mut PgConnection,
    space_id: &str,
    rule_id: &str,
    reason: Option<&str>,
    by: &str,
) -> AppResult<()> {
    let r = get(conn, rule_id).await?;
    if r.space_id != STUDIO || space_id == STUDIO {
        return Err(AppError::State {
            code: "not_studio_rule",
            message: "only a studio rule can be switched off for one client".into(),
        });
    }
    match reason.map(str::trim) {
        Some("") => return Err(AppError::invalid("reason", "say why it doesn't apply here")),
        Some(reason) => {
            sqlx::query(
                "INSERT INTO rule_exceptions (space_id, rule_id, reason, created_by) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (space_id, rule_id) DO UPDATE SET reason = EXCLUDED.reason",
            )
            .bind(space_id)
            .bind(rule_id)
            .bind(reason)
            .bind(by)
            .execute(&mut *conn)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM rule_exceptions WHERE space_id = $1 AND rule_id = $2")
                .bind(space_id)
                .bind(rule_id)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
