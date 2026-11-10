//! Runs (spec section 10): a job across the apps for one client, as steps.
//! Claude starts a run, records each step as it goes, and stops at
//! checkpoints: a checkpoint step that's done waits for you, and nothing
//! after it can start until you approve it in Hub (or send it back with a
//! note). A new session picks a run up from its record.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::error::{AppError, AppResult};

pub const APPS: [&str; 6] = ["hub", "board", "design", "motion", "typeset", "other"];
const MAX_STEPS: usize = 40;
const MAX_TEXT: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewStep {
    /// hub, board, design, motion, typeset or other.
    pub app: String,
    /// What the step does, e.g. "Plan the site in Board".
    pub action: String,
    /// Wait for the user to approve the result before going on.
    #[serde(default)]
    pub checkpoint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Step {
    pub ord: i32,
    pub app: String,
    pub action: String,
    pub checkpoint: bool,
    pub status: String,
    pub notes: Option<String>,
    pub links: Json<Vec<Link>>,
    pub review: Option<String>,
    pub feedback: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Run {
    pub id: String,
    pub space_id: String,
    pub title: String,
    pub goal: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

fn text(field: &str, v: &str, max: usize) -> AppResult<String> {
    let t = v.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(AppError::invalid(
            field,
            format!("write it in up to {max} characters"),
        ));
    }
    Ok(t.to_string())
}

fn check_steps(steps: &[NewStep], from: usize) -> AppResult<()> {
    for (i, s) in steps.iter().enumerate() {
        if !APPS.contains(&s.app.as_str()) {
            return Err(AppError::invalid(
                format!("steps[{}].app", i + from),
                format!("use one of {}", APPS.join(", ")),
            ));
        }
        text(&format!("steps[{}].action", i + from), &s.action, 300)?;
    }
    Ok(())
}

fn check_links(links: &[Link]) -> AppResult<()> {
    if links.len() > 20 {
        return Err(AppError::invalid("links", "at most 20"));
    }
    for (i, l) in links.iter().enumerate() {
        if !(l.url.starts_with("https://") || l.url.starts_with("http://")) || l.url.len() > 2000 {
            return Err(AppError::invalid(
                format!("links[{i}].url"),
                "give a link starting https://",
            ));
        }
        text(&format!("links[{i}].title"), &l.title, 200)?;
    }
    Ok(())
}

pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Run> {
    sqlx::query_as(
        "SELECT id, space_id, title, goal, status, notes, created_by, created_at, updated_at, finished_at
         FROM runs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("run"))
}

pub async fn steps(conn: &mut PgConnection, run_id: &str) -> AppResult<Vec<Step>> {
    Ok(sqlx::query_as(
        "SELECT ord, app, action, checkpoint, status, notes, links, review, feedback, approved_by, approved_at,
                started_at, finished_at
         FROM run_steps WHERE run_id = $1 ORDER BY ord",
    )
    .bind(run_id)
    .fetch_all(conn)
    .await?)
}

/// The step to do next, and whether a checkpoint is in the way.
fn next(steps: &[Step]) -> Value {
    if let Some(w) = steps.iter().find(|s| s.status == "waiting") {
        return json!({
            "blocked": true,
            "step": w.ord,
            "say": format!(
                "Step {} ({}) is waiting for the user to approve it in Hub. Stop here and ask them to; when they say \
                 it's approved, call get_run and carry on.",
                w.ord, w.action
            ),
        });
    }
    match steps
        .iter()
        .find(|s| !matches!(s.status.as_str(), "done" | "skipped"))
    {
        Some(s) => {
            let mut v =
                json!({ "blocked": false, "step": s.ord, "app": s.app, "action": s.action });
            if let Some(f) = &s.feedback {
                v["feedback"] = json!(f);
                v["say"] = json!(
                    "The user sent this step back: do what their feedback asks, then mark it done again."
                );
            }
            v
        }
        None => {
            json!({ "blocked": false, "step": null, "say": "Every step is done: finish the run." })
        }
    }
}

/// A run as the API and the MCP return it: with its steps and what's next.
pub async fn full(conn: &mut PgConnection, id: &str) -> AppResult<Value> {
    let run = get(conn, id).await?;
    let steps = steps(conn, id).await?;
    let mut v = json!(run);
    v["next"] = if matches!(run.status.as_str(), "running" | "waiting") {
        next(&steps)
    } else {
        Value::Null
    };
    v["done"] = json!(
        steps
            .iter()
            .filter(|s| matches!(s.status.as_str(), "done" | "skipped"))
            .count()
    );
    v["steps"] = json!(steps);
    Ok(v)
}

/// id, space_id, title, status, updated_at, steps done, steps
type RunRow = (String, String, String, String, DateTime<Utc>, i64, i64);

/// Runs in these spaces, waiting ones first, then newest.
pub async fn list(
    conn: &mut PgConnection,
    spaces: &[String],
    status: Option<&str>,
) -> AppResult<Vec<Value>> {
    let rows: Vec<RunRow> = sqlx::query_as(
        "SELECT r.id, r.space_id, r.title, r.status, r.updated_at,
                (SELECT count(*) FROM run_steps s WHERE s.run_id = r.id AND s.status IN ('done', 'skipped')),
                (SELECT count(*) FROM run_steps s WHERE s.run_id = r.id)
         FROM runs r WHERE r.space_id = ANY($1) AND ($2::text IS NULL OR r.status = $2)
         ORDER BY r.status = 'waiting' DESC, r.status = 'running' DESC, r.updated_at DESC LIMIT 200",
    )
    .bind(spaces)
    .bind(status)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, space_id, title, status, updated_at, done, total)| {
            json!({ "id": id, "space_id": space_id, "title": title, "status": status, "updated_at": updated_at,
                    "done": done, "steps": total })
        })
        .collect())
}

async fn insert_steps(
    conn: &mut PgConnection,
    run_id: &str,
    from: i32,
    steps: &[NewStep],
) -> AppResult<()> {
    for (i, s) in steps.iter().enumerate() {
        sqlx::query(
            "INSERT INTO run_steps (run_id, ord, app, action, checkpoint, status) VALUES ($1, $2, $3, $4, $5, 'pending')",
        )
        .bind(run_id)
        .bind(from + i as i32)
        .bind(&s.app)
        .bind(s.action.trim())
        .bind(s.checkpoint)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub async fn start(
    conn: &mut PgConnection,
    space_id: &str,
    title: &str,
    goal: Option<&str>,
    steps: &[NewStep],
    by: &str,
) -> AppResult<Value> {
    let title = text("title", title, 200)?;
    let goal = goal.map(|g| text("goal", g, MAX_TEXT)).transpose()?;
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(AppError::invalid(
            "steps",
            format!("plan the job as 1 to {MAX_STEPS} steps"),
        ));
    }
    check_steps(steps, 0)?;
    let id = format!("run_{}", ulid::Ulid::generate());
    sqlx::query("INSERT INTO runs (id, space_id, title, goal, status, created_by) VALUES ($1, $2, $3, $4, 'running', $5)")
        .bind(&id)
        .bind(space_id)
        .bind(&title)
        .bind(&goal)
        .bind(by)
        .execute(&mut *conn)
        .await?;
    insert_steps(conn, &id, 1, steps).await?;
    full(conn, &id).await
}

/// What a session reports about a step.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepUpdate {
    /// running, done, failed or skipped. A checkpoint step marked done waits for the user.
    #[serde(default)]
    pub status: Option<String>,
    /// What happened, what was decided, anything the next session needs.
    #[serde(default)]
    pub notes: Option<String>,
    /// What the step made: [{"title": "Booking site plan", "url": "https://board.kritano.com/boards/…"}]. Added to its links.
    #[serde(default)]
    pub links: Vec<Link>,
    /// For a checkpoint: what the user should look at before approving.
    #[serde(default)]
    pub review: Option<String>,
}

fn open(run: &Run) -> AppResult<()> {
    if !matches!(run.status.as_str(), "running" | "waiting") {
        return Err(AppError::State {
            code: "run_closed",
            message: format!("this run is {}; start a new one", run.status),
        });
    }
    Ok(())
}

async fn touch(conn: &mut PgConnection, run_id: &str) -> AppResult<()> {
    let waiting: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run_steps WHERE run_id = $1 AND status = 'waiting'",
    )
    .bind(run_id)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE runs SET status = CASE WHEN $2 > 0 THEN 'waiting' ELSE 'running' END, updated_at = now()
         WHERE id = $1 AND status IN ('running', 'waiting')",
    )
    .bind(run_id)
    .bind(waiting)
    .execute(conn)
    .await?;
    Ok(())
}

/// Record progress on a step. A step can't start (or be done) while an
/// earlier checkpoint is waiting for the user: that's the gate.
pub async fn update_step(
    conn: &mut PgConnection,
    run_id: &str,
    ord: i32,
    u: StepUpdate,
) -> AppResult<Value> {
    let run = get(conn, run_id).await?;
    open(&run)?;
    let all = steps(conn, run_id).await?;
    let step = all
        .iter()
        .find(|s| s.ord == ord)
        .ok_or(AppError::NotFound("step"))?;
    if let Some(status) = &u.status {
        if !matches!(status.as_str(), "running" | "done" | "failed" | "skipped") {
            return Err(AppError::invalid(
                "status",
                "use running, done, failed or skipped",
            ));
        }
        if let Some(w) = all.iter().find(|s| s.ord < ord && s.status == "waiting") {
            return Err(AppError::State {
                code: "waiting_for_you",
                message: format!(
                    "step {} ({}) is waiting for the user's approval in Hub: stop and ask them to approve it there",
                    w.ord, w.action
                ),
            });
        }
        if step.status == "waiting" {
            return Err(AppError::State {
                code: "waiting_for_you",
                message: "this step is waiting for the user's approval in Hub".into(),
            });
        }
    }
    let notes = u
        .notes
        .as_deref()
        .map(|n| text("notes", n, MAX_TEXT))
        .transpose()?;
    let review = u
        .review
        .as_deref()
        .map(|n| text("review", n, MAX_TEXT))
        .transpose()?;
    check_links(&u.links)?;
    let mut links = step.links.0.clone();
    for l in u.links {
        if !links.iter().any(|x| x.url == l.url) {
            links.push(l);
        }
    }
    // A checkpoint step that's done waits for you instead.
    let status = match u.status.as_deref() {
        Some("done") if step.checkpoint => "waiting".to_string(),
        Some(s) => s.to_string(),
        None => step.status.clone(),
    };
    sqlx::query(
        "UPDATE run_steps SET status = $3, notes = COALESCE($4, notes), links = $5, review = COALESCE($6, review),
                started_at = CASE WHEN $3 IN ('running', 'done', 'waiting') AND started_at IS NULL THEN now() ELSE started_at END,
                finished_at = CASE WHEN $3 IN ('done', 'failed', 'skipped') THEN now()
                                   WHEN $3 = 'waiting' THEN finished_at ELSE NULL END,
                feedback = CASE WHEN $3 IN ('done', 'waiting') THEN NULL ELSE feedback END
         WHERE run_id = $1 AND ord = $2",
    )
    .bind(run_id)
    .bind(ord)
    .bind(&status)
    .bind(&notes)
    .bind(Json(&links))
    .bind(&review)
    .execute(&mut *conn)
    .await?;
    touch(conn, run_id).await?;
    full(conn, run_id).await
}

/// Pause at a step for the user, though it wasn't planned as a checkpoint.
pub async fn checkpoint(
    conn: &mut PgConnection,
    run_id: &str,
    ord: i32,
    review: &str,
) -> AppResult<Value> {
    let review = text("review", review, MAX_TEXT)?;
    let run = get(conn, run_id).await?;
    open(&run)?;
    let r = sqlx::query(
        "UPDATE run_steps SET checkpoint = true, status = 'waiting', review = $3,
                started_at = COALESCE(started_at, now())
         WHERE run_id = $1 AND ord = $2 AND status NOT IN ('skipped')",
    )
    .bind(run_id)
    .bind(ord)
    .bind(&review)
    .execute(&mut *conn)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("step"));
    }
    touch(conn, run_id).await?;
    full(conn, run_id).await
}

/// Add steps at the end (the plan grew).
pub async fn add_steps(
    conn: &mut PgConnection,
    run_id: &str,
    steps: &[NewStep],
) -> AppResult<Value> {
    let run = get(conn, run_id).await?;
    open(&run)?;
    let have: i64 = sqlx::query_scalar("SELECT count(*) FROM run_steps WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(&mut *conn)
        .await?;
    if steps.is_empty() || have as usize + steps.len() > MAX_STEPS {
        return Err(AppError::invalid(
            "steps",
            format!("a run has at most {MAX_STEPS} steps"),
        ));
    }
    check_steps(steps, have as usize)?;
    insert_steps(conn, run_id, have as i32 + 1, steps).await?;
    touch(conn, run_id).await?;
    full(conn, run_id).await
}

/// You approve a checkpoint (in Hub): it's done and the run goes on.
pub async fn approve(
    conn: &mut PgConnection,
    run_id: &str,
    ord: i32,
    by: &str,
    note: Option<&str>,
) -> AppResult<Value> {
    let r = sqlx::query(
        "UPDATE run_steps SET status = 'done', approved_by = $3, approved_at = now(), finished_at = now(),
                feedback = $4
         WHERE run_id = $1 AND ord = $2 AND status = 'waiting'",
    )
    .bind(run_id)
    .bind(ord)
    .bind(by)
    .bind(note.map(str::trim).filter(|n| !n.is_empty()))
    .execute(&mut *conn)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::State {
            code: "not_waiting",
            message: "that step isn't waiting for approval".into(),
        });
    }
    touch(conn, run_id).await?;
    full(conn, run_id).await
}

/// You send a checkpoint back with what to change: the step is open again.
pub async fn send_back(
    conn: &mut PgConnection,
    run_id: &str,
    ord: i32,
    feedback: &str,
) -> AppResult<Value> {
    let feedback = text("feedback", feedback, MAX_TEXT)?;
    let r = sqlx::query(
        "UPDATE run_steps SET status = 'running', feedback = $3, finished_at = NULL
         WHERE run_id = $1 AND ord = $2 AND status = 'waiting'",
    )
    .bind(run_id)
    .bind(ord)
    .bind(&feedback)
    .execute(&mut *conn)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::State {
            code: "not_waiting",
            message: "that step isn't waiting for approval".into(),
        });
    }
    touch(conn, run_id).await?;
    full(conn, run_id).await
}

/// End a run: done (every step done or skipped), failed or cancelled.
pub async fn finish(
    conn: &mut PgConnection,
    run_id: &str,
    status: &str,
    notes: Option<&str>,
) -> AppResult<Value> {
    let run = get(conn, run_id).await?;
    open(&run)?;
    if !matches!(status, "done" | "failed" | "cancelled") {
        return Err(AppError::invalid("status", "use done, failed or cancelled"));
    }
    let all = steps(conn, run_id).await?;
    if status == "done"
        && let Some(s) = all
            .iter()
            .find(|s| !matches!(s.status.as_str(), "done" | "skipped"))
    {
        return Err(AppError::State {
            code: "steps_left",
            message: format!(
                "step {} ({}) is {}: finish or skip it first",
                s.ord, s.action, s.status
            ),
        });
    }
    let notes = notes.map(|n| text("notes", n, MAX_TEXT)).transpose()?;
    sqlx::query("UPDATE runs SET status = $2, notes = $3, finished_at = now(), updated_at = now() WHERE id = $1")
        .bind(run_id)
        .bind(status)
        .bind(&notes)
        .execute(&mut *conn)
        .await?;
    full(conn, run_id).await
}
