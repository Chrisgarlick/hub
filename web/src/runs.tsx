// Runs: a job across the apps for one client, step by step. Claude records
// each step; at a checkpoint it stops, and you approve here or send it back.

import { useSignal } from "@preact/signals";
import { useEffect } from "preact/hooks";

import { api, type Run, type RunStep, type RunSummary } from "./api.ts";
import { runUrl, spaceUrl } from "./router.ts";
import { ago, full } from "./time.ts";
import { Link, message } from "./ui.tsx";

const APP: Record<string, string> = {
  hub: "Hub",
  board: "Board",
  design: "Design",
  motion: "Motion",
  typeset: "Typeset",
  other: "Other",
};

const RUN_STATUS: Record<string, string> = {
  running: "Running",
  waiting: "Waiting for you",
  done: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
};

const STEP_STATUS: Record<string, string> = {
  pending: "To do",
  running: "In progress",
  done: "Done",
  waiting: "Waiting for you",
  failed: "Failed",
  skipped: "Skipped",
};

/** Runs for every client, or one (`space`). */
export function RunsList({ space }: { space?: string }) {
  const runs = useSignal<RunSummary[] | null>(null);
  const error = useSignal("");
  useEffect(() => {
    api.runs(space).then(
      (r) => (runs.value = r.runs),
      (e) => (error.value = message(e)),
    );
  }, [space]);

  if (error.value) return <p class="banner is-warn">{error.value}</p>;
  if (!runs.value) return <p class="meta">Loading…</p>;
  if (runs.value.length === 0)
    return (
      <p class="empty">
        No runs yet. Ask Claude to take a job from brief to proposal and it records the run here, stopping for you at
        each checkpoint.
      </p>
    );
  return (
    <ul class="space-list runs-list">
      {runs.value.map((r) => (
        <li key={r.id}>
          <Link href={runUrl(r.id)} class={`space-row run-row is-${r.status}`}>
            <span>
              <span class="space-name">{r.title}</span>
              <span class="item-summary">
                {!space && r.space ? `${r.space.name}, ` : ""}
                {r.done} of {r.steps} steps, updated {ago(r.updated_at)}
              </span>
            </span>
            <span class={`run-status is-${r.status}`}>{RUN_STATUS[r.status] ?? r.status}</span>
          </Link>
        </li>
      ))}
    </ul>
  );
}

export function RunsPage() {
  return (
    <main class="approvals">
      <h1>Runs</h1>
      <p class="lede">
        Jobs across the apps, one client each: what Claude has done, step by step, with links to what it made. A run
        stops at each checkpoint until you approve it here.
      </p>
      <RunsList />
    </main>
  );
}

export function RunPage({ id }: { id: string }) {
  const run = useSignal<Run | null>(null);
  const error = useSignal("");
  const load = () =>
    api.run(id).then(
      (r) => (run.value = r),
      (e) => (error.value = message(e)),
    );
  useEffect(() => {
    void load();
    // Claude works while you watch: keep the timeline fresh.
    const t = setInterval(load, 15_000);
    return () => clearInterval(t);
  }, [id]);

  const r = run.value;
  if (error.value && !r) return <p class="page-note is-bad">{error.value}</p>;
  if (!r) return <p class="page-note">Loading…</p>;

  const cancel = async () => {
    if (!confirm("Cancel this run? Claude won't be able to carry on with it.")) return;
    try {
      run.value = await api.cancelRun(id);
    } catch (e) {
      error.value = message(e);
    }
  };

  const open = r.status === "running" || r.status === "waiting";
  return (
    <main class="approvals">
      <Link href="/runs" class="back">
        Runs
      </Link>
      <div class="page-head">
        <h1>{r.title}</h1>
        <span class={`run-status is-${r.status}`}>{RUN_STATUS[r.status] ?? r.status}</span>
      </div>
      <p class="meta" title={full(r.created_at)}>
        {r.space && (
          <>
            <Link href={`${spaceUrl(r.space.slug)}/runs`}>{r.space.name}</Link>,{" "}
          </>
        )}
        started by {r.created_by} {ago(r.created_at)}, {r.done} of {r.steps.length} steps done
      </p>
      {r.goal && <p class="lede">{r.goal}</p>}
      {error.value && <p class="banner is-warn">{error.value}</p>}
      <ol class="timeline">
        {r.steps.map((s) => (
          <StepItem key={s.ord} s={s} runId={id} onChange={(n) => (run.value = n)} />
        ))}
      </ol>
      {r.notes && <p class="run-notes">{r.notes}</p>}
      {open && (
        <p>
          <button type="button" class="quiet" onClick={cancel}>
            Cancel the run
          </button>
        </p>
      )}
    </main>
  );
}

function StepItem({ s, runId, onChange }: { s: RunStep; runId: string; onChange: (r: Run) => void }) {
  const sending = useSignal(false);
  const note = useSignal("");
  const error = useSignal("");
  const busy = useSignal(false);

  const act = async (f: () => Promise<Run>) => {
    busy.value = true;
    error.value = "";
    try {
      onChange(await f());
      sending.value = false;
      note.value = "";
    } catch (e) {
      error.value = message(e);
    } finally {
      busy.value = false;
    }
  };

  return (
    <li class={`step is-${s.status} app-${s.app}`}>
      <span class="step-dot" aria-hidden="true" />
      <div class="step-body">
        <div class="step-head">
          <span class="step-num">{s.ord}</span>
          <span class="app-chip">{APP[s.app] ?? s.app}</span>
          <span class="step-action">{s.action}</span>
          {s.checkpoint && s.status !== "waiting" && <span class="badge">checkpoint</span>}
          <span class={`step-status is-${s.status}`}>{STEP_STATUS[s.status] ?? s.status}</span>
        </div>
        {s.notes && <p class="step-notes">{s.notes}</p>}
        {s.links.length > 0 && (
          <ul class="step-links">
            {s.links.map((l) => (
              <li key={l.url}>
                <a href={l.url} target="_blank" rel="noopener noreferrer">
                  {l.title}
                </a>
              </li>
            ))}
          </ul>
        )}
        {s.status === "running" && s.feedback && <p class="step-feedback">You asked: {s.feedback}</p>}
        {s.status === "waiting" && (
          <div class="checkpoint-box">
            <p>
              <strong>Your turn.</strong> {s.review ?? "Look at what this step made, then approve it or send it back."}
            </p>
            {error.value && <p class="field-error">{error.value}</p>}
            {sending.value ? (
              <form
                class="row"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (note.value.trim()) void act(() => api.sendBack(runId, s.ord, note.value));
                }}
              >
                <input
                  class="grow"
                  placeholder="What should change? Claude sees this"
                  value={note.value}
                  onInput={(e) => (note.value = e.currentTarget.value)}
                  ref={(el) => el?.focus()}
                />
                <button type="submit" disabled={busy.value || !note.value.trim()}>
                  Send back
                </button>
                <button type="button" class="quiet" onClick={() => (sending.value = false)}>
                  Cancel
                </button>
              </form>
            ) : (
              <div class="row">
                <button type="button" class="primary" disabled={busy.value} onClick={() => act(() => api.approveStep(runId, s.ord))}>
                  Approve and go on
                </button>
                <button type="button" class="quiet" disabled={busy.value} onClick={() => (sending.value = true)}>
                  Send back
                </button>
              </div>
            )}
            <p class="meta">Then tell Claude to carry on.</p>
          </div>
        )}
        {s.approved_at && (
          <p class="meta" title={full(s.approved_at)}>
            Approved by you {ago(s.approved_at)}
          </p>
        )}
      </div>
    </li>
  );
}

/** For Approvals: checkpoints waiting for you, across runs. */
export function WaitingCheckpoints() {
  const runs = useSignal<Run[]>([]);
  useEffect(() => {
    api.runs(undefined, "waiting").then(
      async (r) => (runs.value = await Promise.all(r.runs.map((x) => api.run(x.id)))),
      () => {},
    );
  }, []);
  if (runs.value.length === 0) return null;
  return (
    <section class="group">
      <h2>Runs waiting for you</h2>
      <ul class="space-list">
        {runs.value.flatMap((r) =>
          r.steps
            .filter((s) => s.status === "waiting")
            .map((s) => (
              <li key={`${r.id}-${s.ord}`}>
                <Link href={runUrl(r.id)} class="space-row run-row is-waiting">
                  <span>
                    <span class="space-name">{r.title}</span>
                    <span class="item-summary">
                      {r.space?.name}, step {s.ord}: {s.action}
                    </span>
                  </span>
                  <span class="run-status is-waiting">Checkpoint</span>
                </Link>
              </li>
            )),
        )}
      </ul>
    </section>
  );
}
