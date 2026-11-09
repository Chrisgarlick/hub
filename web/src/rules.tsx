// A space's rules (with a box to check a draft) and its decisions.

import { useSignal } from "@preact/signals";
import { useEffect } from "preact/hooks";

import {
  TARGETS,
  api,
  type CheckResult,
  type Decision,
  type Finding,
  type Heading,
  type Rule,
  type RuleKind,
  type Summary,
  type Target,
} from "./api.ts";
import { renderMarkdown } from "./markdown.ts";
import { docUrl } from "./router.ts";
import { ago, full } from "./time.ts";
import { Link, message } from "./ui.tsx";

const KIND: Record<RuleKind, string> = {
  must: "Must",
  must_not: "Must not",
  prefer: "Prefer",
  avoid: "Avoid",
};

const TARGET_NAME: Record<Target, string> = {
  all: "Everywhere",
  board: "Board",
  design: "Design",
  motion: "Motion",
  typeset: "Typeset",
};

function Targets({ list }: { list: Target[] }) {
  if (list.includes("all")) return null;
  return <span class="targets">{list.map((t) => TARGET_NAME[t]).join(", ")}</span>;
}

/** Tick boxes for where something applies; "Everywhere" clears the rest. */
function TargetPicker({ value, onChange }: { value: Target[]; onChange: (v: Target[]) => void }) {
  return (
    <fieldset class="target-picker">
      <legend>Applies to</legend>
      {TARGETS.map((t) => (
        <label key={t}>
          <input
            type="checkbox"
            checked={value.includes(t)}
            onChange={(e) => {
              const on = e.currentTarget.checked;
              if (t === "all") return onChange(on ? ["all"] : []);
              const rest = value.filter((v) => v !== "all" && v !== t);
              onChange(on ? [...rest, t] : rest.length ? rest : ["all"]);
            }}
          />
          {TARGET_NAME[t]}
        </label>
      ))}
    </fieldset>
  );
}

// ---------- Rules ----------

export function RulesPage({ slug, studio }: { slug: string; studio: boolean }) {
  const rules = useSignal<Rule[] | null>(null);
  const error = useSignal("");
  const load = () =>
    api.rules(slug).then(
      (r) => (rules.value = r.rules),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), [slug]);

  const act = async (f: () => Promise<unknown>) => {
    error.value = "";
    try {
      await f();
      await load();
    } catch (e) {
      error.value = message(e);
    }
  };

  const list = rules.value;
  const waiting = list?.filter((r) => r.status === "proposed") ?? [];
  const live = list?.filter((r) => r.status === "approved") ?? [];
  const checked = live.filter((r) => r.check);
  const guidance = live.filter((r) => !r.check);

  return (
    <div class="page">
      <h2>Rules</h2>
      <p class="meta">
        From this space's Writing rules document, rules added here{studio ? "" : ", and the studio's rules"}. Rules
        with words to check are checked in drafts; the rest is guidance for Claude.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      <CheckDraft slug={slug} />
      {!list ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          {waiting.length > 0 && (
            <section class="group">
              <h3>Waiting for you</h3>
              <ul class="rule-list">
                {waiting.map((r) => (
                  <RuleRow key={r.id} r={r} slug={slug}>
                    <button type="button" class="primary" onClick={() => act(() => api.approveRule(r.id))}>
                      Approve
                    </button>
                    {!r.source_doc && (
                      <button type="button" class="quiet" onClick={() => act(() => api.retireRule(r.id))}>
                        Reject
                      </button>
                    )}
                  </RuleRow>
                ))}
              </ul>
            </section>
          )}
          <section class="group">
            <h3>Checked in drafts</h3>
            {checked.length ? (
              <ul class="rule-list">
                {checked.map((r) => (
                  <RuleRow key={r.id} r={r} slug={slug} act={act} studio={studio} />
                ))}
              </ul>
            ) : (
              <p class="empty">
                None yet. Put exact words in "double quotes" in a rule in Writing rules and Hub checks for them.
              </p>
            )}
          </section>
          <section class="group">
            <h3>Guidance</h3>
            {guidance.length ? (
              <ul class="rule-list">
                {guidance.map((r) => (
                  <RuleRow key={r.id} r={r} slug={slug} act={act} studio={studio} />
                ))}
              </ul>
            ) : (
              <p class="empty">None yet.</p>
            )}
          </section>
          <AddRule slug={slug} onAdded={load} />
        </>
      )}
    </div>
  );
}

function RuleRow({
  r,
  slug,
  act,
  studio,
  children,
}: {
  r: Rule;
  slug: string;
  act?: (f: () => Promise<unknown>) => Promise<void>;
  studio?: boolean;
  children?: preact.ComponentChildren;
}) {
  const asking = useSignal(false);
  const reason = useSignal("");
  const c = r.check;
  const words = c ? [...(c.terms ?? []), ...(c.required_terms ?? [])] : [];
  const fromStudio = r.scope === "studio" && !studio;
  return (
    <li class={r.excepted ? "rule is-off" : "rule"}>
      <div class="rule-main">
        <span class={`kind kind-${r.kind}`}>{KIND[r.kind]}</span>
        <span class="rule-text">{r.text}</span>
      </div>
      <div class="rule-meta">
        <Targets list={r.applies_to} />
        {words.length > 0 && (
          <span>
            {c?.required_terms?.length ? "needs " : "looks for "}
            {words.map((w) => `“${w}”`).join(", ")}
            {c?.case_sensitive ? " (exact case)" : ""}
          </span>
        )}
        {c?.pattern && <code>/{c.pattern}/</code>}
        {fromStudio && <span class="badge">Studio</span>}
        {r.source_doc && r.source_path && !fromStudio && (
          <Link href={docUrl(slug, r.source_doc)}>{r.source_path}</Link>
        )}
        {!r.source_doc && <span>added by {r.created_by}</span>}
        {r.status === "proposed" && <span class="badge is-tentative">proposed by {r.created_by}</span>}
      </div>
      {r.warning && <p class="field-error">{r.warning}</p>}
      {r.excepted && <p class="meta">Off for this client: {r.excepted.reason}</p>}
      {asking.value && act && (
        <form
          class="row except"
          onSubmit={(e) => {
            e.preventDefault();
            void act(() => api.except(slug, r.id, reason.value)).then(() => (asking.value = false));
          }}
        >
          <input
            aria-label="Why it doesn't apply"
            placeholder="Why it doesn't apply to this client"
            value={reason.value}
            onInput={(e) => (reason.value = e.currentTarget.value)}
            ref={(el) => el?.focus()}
          />
          <button type="submit">Switch off</button>
          <button type="button" class="quiet" onClick={() => (asking.value = false)}>
            Cancel
          </button>
        </form>
      )}
      <div class="row rule-actions">
        {children}
        {act && fromStudio && !asking.value &&
          (r.excepted ? (
            <button type="button" class="quiet" onClick={() => act(() => api.unexcept(slug, r.id))}>
              Switch back on
            </button>
          ) : (
            <button type="button" class="quiet" onClick={() => (asking.value = true)}>
              Doesn't apply here
            </button>
          ))}
        {act && !r.source_doc && !fromStudio && r.status === "approved" && (
          <button type="button" class="quiet" onClick={() => confirm("Retire this rule?") && act(() => api.retireRule(r.id))}>
            Retire
          </button>
        )}
      </div>
    </li>
  );
}

function AddRule({ slug, onAdded }: { slug: string; onAdded: () => void }) {
  const open = useSignal(false);
  const kind = useSignal<RuleKind>("must_not");
  const text = useSignal("");
  const words = useSignal("");
  const targets = useSignal<Target[]>(["all"]);
  const error = useSignal("");

  if (!open.value)
    return (
      <p>
        <button type="button" class="quiet" onClick={() => (open.value = true)}>
          Add a rule here
        </button>
        <span class="meta"> Most rules belong in Writing rules; add one here for a one-off.</span>
      </p>
    );

  const submit = async (e: Event) => {
    e.preventDefault();
    error.value = "";
    const list = words.value
      .split(",")
      .map((w) => w.trim())
      .filter(Boolean);
    const check = list.length ? (kind.value === "must" ? { required_terms: list } : { terms: list }) : undefined;
    try {
      await api.addRule(slug, {
        kind: kind.value,
        text: text.value,
        applies_to: targets.value,
        ...(check ? { check } : {}),
      });
      open.value = false;
      text.value = "";
      words.value = "";
      onAdded();
    } catch (err) {
      error.value = message(err);
    }
  };

  return (
    <form class="card-form" onSubmit={submit}>
      <h3>Add a rule</h3>
      <div class="row">
        <select aria-label="Kind" value={kind.value} onChange={(e) => (kind.value = e.currentTarget.value as RuleKind)}>
          {(Object.keys(KIND) as RuleKind[]).map((k) => (
            <option key={k} value={k}>
              {KIND[k]}
            </option>
          ))}
        </select>
        <input
          aria-label="Rule"
          placeholder="Don't mention the old office"
          value={text.value}
          onInput={(e) => (text.value = e.currentTarget.value)}
        />
      </div>
      <label>
        {kind.value === "must" ? "Words that must appear" : "Words to look for"}, comma-separated (optional)
        <input value={words.value} onInput={(e) => (words.value = e.currentTarget.value)} />
      </label>
      <TargetPicker value={targets.value} onChange={(v) => (targets.value = v)} />
      {error.value && <p class="field-error">{error.value}</p>}
      <div class="row">
        <button type="submit" class="primary">
          Add rule
        </button>
        <button type="button" class="quiet" onClick={() => (open.value = false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}

// ---------- Checking a draft ----------

/** The draft with each finding marked. */
function Marked({ text, findings }: { text: string; findings: Finding[] }) {
  const spans = findings
    .flatMap((f) => f.spans.map((s) => ({ ...s, hard: f.rule.kind === "must" || f.rule.kind === "must_not" })))
    .sort((a, b) => a.start - b.start);
  // Offsets are UTF-8 bytes; map them to string positions.
  const bytes = new TextEncoder().encode(text);
  const dec = new TextDecoder();
  const at = (b: number) => dec.decode(bytes.slice(0, b)).length;
  const parts: preact.ComponentChildren[] = [];
  let pos = 0;
  for (const s of spans) {
    const a = at(s.start);
    const b = at(s.end);
    if (a < pos) continue;
    parts.push(text.slice(pos, a));
    parts.push(
      <mark key={s.start} class={s.hard ? "hit" : "hit is-soft"}>
        {text.slice(a, b)}
      </mark>,
    );
    pos = b;
  }
  parts.push(text.slice(pos));
  return <pre class="marked">{parts}</pre>;
}

function CheckDraft({ slug }: { slug: string }) {
  const text = useSignal("");
  const target = useSignal<Target>("all");
  const result = useSignal<CheckResult | null>(null);
  const checked = useSignal("");
  const error = useSignal("");
  const busy = useSignal(false);

  const run = async (e: Event) => {
    e.preventDefault();
    busy.value = true;
    error.value = "";
    try {
      result.value = await api.check(slug, text.value, target.value);
      checked.value = text.value;
    } catch (err) {
      error.value = message(err);
    } finally {
      busy.value = false;
    }
  };

  const r = result.value;
  return (
    <form class="check" onSubmit={run}>
      <h3>Check a draft</h3>
      <textarea
        aria-label="Draft"
        placeholder="Paste copy, a report section or a caption"
        value={text.value}
        onInput={(e) => (text.value = e.currentTarget.value)}
      />
      <div class="row">
        <select aria-label="For" value={target.value} onChange={(e) => (target.value = e.currentTarget.value as Target)}>
          {TARGETS.map((t) => (
            <option key={t} value={t}>
              {t === "all" ? "Any app" : `For ${TARGET_NAME[t]}`}
            </option>
          ))}
        </select>
        <button type="submit" class="primary" disabled={busy.value || !text.value.trim()}>
          {busy.value ? "Checking…" : "Check"}
        </button>
      </div>
      {error.value && <p class="field-error">{error.value}</p>}
      {r && (
        <div class="check-result" role="status">
          <p class={r.ok ? "verdict is-ok" : "verdict is-bad"}>
            {r.ok
              ? "No rules broken."
              : `${r.violations.length} ${r.violations.length === 1 ? "rule" : "rules"} broken.`}
            {r.advice.length > 0 && ` ${r.advice.length} to consider.`}
          </p>
          {[...r.violations, ...r.advice].length > 0 && <Marked text={checked.value} findings={[...r.violations, ...r.advice]} />}
          <ul class="findings">
            {r.violations.map((f) => (
              <FindingRow key={f.rule.id} f={f} />
            ))}
            {r.advice.map((f) => (
              <FindingRow key={f.rule.id} f={f} soft />
            ))}
          </ul>
          {r.guidance.length > 0 && (
            <details>
              <summary>
                {r.guidance.length} {r.guidance.length === 1 ? "rule" : "rules"} for Claude to judge
              </summary>
              <ul>
                {r.guidance.map((g) => (
                  <li key={g.id}>{g.text}</li>
                ))}
              </ul>
            </details>
          )}
          {r.decisions.length > 0 && (
            <details>
              <summary>
                {r.decisions.length} {r.decisions.length === 1 ? "decision" : "decisions"} in force
              </summary>
              <ul>
                {r.decisions.map((d) => (
                  <li key={d.id}>{d.statement}</li>
                ))}
              </ul>
            </details>
          )}
        </div>
      )}
    </form>
  );
}

function FindingRow({ f, soft }: { f: Finding; soft?: boolean }) {
  return (
    <li class={soft ? "finding is-soft" : "finding"}>
      <span class={`kind kind-${f.rule.kind}`}>{KIND[f.rule.kind]}</span> {f.rule.text}
      <span class="meta">
        {f.spans.length > 0 &&
          ` ${f.spans.map((s) => `line ${s.line}:${s.column} “${s.text}”`).join(", ")}`}
        {f.missing.length > 0 && ` missing ${f.missing.map((m) => `“${m}”`).join(", ")}`}
        {f.rule.scope === "studio" && ", from the studio"}
      </span>
    </li>
  );
}

// ---------- Decisions ----------

export function DecisionsPage({ slug, docs }: { slug: string; docs: Summary[] }) {
  const list = useSignal<Decision[] | null>(null);
  const error = useSignal("");
  const asDoc = useSignal<string | null>(null);
  const adding = useSignal<{ supersedes?: Decision } | null>(null);

  const load = () =>
    api.decisions(slug).then(
      (r) => (list.value = r.decisions),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), [slug]);

  const act = async (f: () => Promise<unknown>) => {
    error.value = "";
    try {
      await f();
      await load();
      if (asDoc.value !== null) asDoc.value = (await api.decisionsMarkdown(slug)).markdown;
    } catch (e) {
      error.value = message(e);
    }
  };

  const toggleDoc = async () => {
    if (asDoc.value !== null) return (asDoc.value = null);
    try {
      asDoc.value = (await api.decisionsMarkdown(slug)).markdown;
    } catch (e) {
      error.value = message(e);
    }
  };

  const all = list.value;
  const waiting = all?.filter((d) => d.status === "proposed") ?? [];
  const live = all?.filter((d) => d.status === "approved") ?? [];
  const past = all?.filter((d) => d.status === "superseded" || d.status === "rejected") ?? [];

  return (
    <div class="page">
      <div class="page-head">
        <h2>Decisions</h2>
        <button type="button" class="quiet" aria-pressed={asDoc.value !== null} onClick={toggleDoc}>
          Read as a document
        </button>
      </div>
      <p class="meta">
        Where the work departs from the docs, and why. A decision in force overrides the sections it names; a later
        one can supersede it.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {asDoc.value !== null ? (
        <div class="prose" dangerouslySetInnerHTML={{ __html: renderMarkdown(asDoc.value) }} />
      ) : !all ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          {adding.value ? (
            <NewDecision
              slug={slug}
              docs={docs}
              {...(adding.value.supersedes ? { supersedes: adding.value.supersedes } : {})}
              onDone={(made) => {
                adding.value = null;
                if (made) void load();
              }}
            />
          ) : (
            <p>
              <button type="button" class="primary" onClick={() => (adding.value = {})}>
                Record a decision
              </button>
            </p>
          )}
          {waiting.length > 0 && (
            <section class="group">
              <h3>Waiting for you</h3>
              {waiting.map((d) => (
                <DecisionCard key={d.id} d={d} slug={slug} all={all}>
                  <button type="button" class="primary" onClick={() => act(() => api.approveDecision(d.id))}>
                    Approve
                  </button>
                  <button type="button" class="quiet" onClick={() => act(() => api.rejectDecision(d.id))}>
                    Reject
                  </button>
                </DecisionCard>
              ))}
            </section>
          )}
          <section class="group">
            <h3>In force</h3>
            {live.length ? (
              live.map((d) => (
                <DecisionCard key={d.id} d={d} slug={slug} all={all}>
                  <button type="button" class="quiet" onClick={() => (adding.value = { supersedes: d })}>
                    Supersede
                  </button>
                </DecisionCard>
              ))
            ) : (
              <p class="empty">No decisions yet.</p>
            )}
          </section>
          {past.length > 0 && (
            <details class="group">
              <summary>No longer in force ({past.length})</summary>
              {past.map((d) => (
                <DecisionCard key={d.id} d={d} slug={slug} all={all} />
              ))}
            </details>
          )}
        </>
      )}
    </div>
  );
}

function DecisionCard({
  d,
  slug,
  all,
  children,
}: {
  d: Decision;
  slug: string;
  all: Decision[];
  children?: preact.ComponentChildren;
}) {
  const replaced = d.supersedes ? all.find((x) => x.id === d.supersedes) : undefined;
  const by = d.superseded_by ? all.find((x) => x.id === d.superseded_by) : undefined;
  return (
    <article class={`decision is-${d.status}`} id={d.id}>
      <h4>{d.statement}</h4>
      <p>{d.reason}</p>
      <ul class="decision-meta">
        {d.contradicts.map((s) => (
          <li key={s.doc + (s.anchor ?? "")}>
            Overrides <Link href={`${docUrl(slug, s.doc)}${s.anchor ? `#${s.anchor}` : ""}`}>{s.path}{s.anchor ? ` › ${s.anchor}` : ""}</Link>
          </li>
        ))}
        {replaced && <li>Replaces “{replaced.statement}”</li>}
        {by && <li>Superseded by “{by.statement}”</li>}
        {d.evidence.map((e) => (
          <li key={e.url}>
            <a href={e.url} target="_blank" rel="noopener noreferrer">
              {e.note ?? e.app ?? e.url}
            </a>
          </li>
        ))}
        <li class="meta" title={full(d.decided_at ?? d.created_at)}>
          {d.status === "proposed"
            ? `Proposed by ${d.proposed_by} ${ago(d.created_at)}`
            : `${d.status[0]?.toUpperCase()}${d.status.slice(1)} ${ago(d.decided_at ?? d.created_at)}`}
          {!d.applies_to.includes("all") && `, for ${d.applies_to.map((t) => TARGET_NAME[t]).join(", ")}`}
        </li>
      </ul>
      {children && <div class="row">{children}</div>}
    </article>
  );
}

function NewDecision({
  slug,
  docs,
  supersedes,
  onDone,
}: {
  slug: string;
  docs: Summary[];
  supersedes?: Decision;
  onDone: (made: boolean) => void;
}) {
  const statement = useSignal(supersedes?.statement ?? "");
  const reason = useSignal("");
  const targets = useSignal<Target[]>(supersedes?.applies_to ?? ["all"]);
  const sections = useSignal(supersedes?.contradicts.map((s) => ({ doc: s.doc, path: s.path, anchor: s.anchor ?? "" })) ?? []);
  const pickDoc = useSignal("");
  const headings = useSignal<Heading[]>([]);
  const pickAnchor = useSignal("");
  const error = useSignal("");

  useEffect(() => {
    headings.value = [];
    pickAnchor.value = "";
    if (pickDoc.value) void api.get(pickDoc.value).then((d) => (headings.value = d.headings));
  }, [pickDoc.value]);

  const addSection = () => {
    const d = docs.find((x) => x.id === pickDoc.value);
    if (!d) return;
    sections.value = [...sections.value, { doc: d.id, path: d.path, anchor: pickAnchor.value }];
    pickDoc.value = "";
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    error.value = "";
    try {
      await api.addDecision(slug, {
        statement: statement.value,
        reason: reason.value,
        applies_to: targets.value,
        contradicts: sections.value.map((s) => (s.anchor ? { doc: s.doc, anchor: s.anchor } : { doc: s.doc })),
        ...(supersedes ? { supersedes: supersedes.id } : {}),
      });
      onDone(true);
    } catch (err) {
      error.value = message(err);
    }
  };

  return (
    <form class="card-form" onSubmit={submit}>
      <h3>{supersedes ? "Supersede a decision" : "Record a decision"}</h3>
      {supersedes && <p class="meta">Replaces “{supersedes.statement}” once saved.</p>}
      <label>
        What's decided
        <input
          value={statement.value}
          placeholder="Guests can check out without an account."
          onInput={(e) => (statement.value = e.currentTarget.value)}
        />
      </label>
      <label>
        Why
        <textarea
          value={reason.value}
          placeholder="Two thirds of orders are gifts from one-off buyers (see Audience)."
          onInput={(e) => (reason.value = e.currentTarget.value)}
        />
      </label>
      <fieldset>
        <legend>What it overrides in the docs</legend>
        {sections.value.length > 0 && (
          <ul class="picked">
            {sections.value.map((s, i) => (
              <li key={i}>
                {s.path}
                {s.anchor && ` › ${s.anchor}`}{" "}
                <button
                  type="button"
                  class="quiet"
                  onClick={() => (sections.value = sections.value.filter((_, j) => j !== i))}
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}
        <div class="row">
          <select aria-label="Document" value={pickDoc.value} onChange={(e) => (pickDoc.value = e.currentTarget.value)}>
            <option value="">Choose a document…</option>
            {docs.map((d) => (
              <option key={d.id} value={d.id}>
                {d.title}
              </option>
            ))}
          </select>
          {headings.value.length > 0 && (
            <select aria-label="Section" value={pickAnchor.value} onChange={(e) => (pickAnchor.value = e.currentTarget.value)}>
              <option value="">The whole document</option>
              {headings.value.map((h) => (
                <option key={h.anchor} value={h.anchor}>
                  {"  ".repeat(Math.max(0, h.level - 1))}
                  {h.text}
                </option>
              ))}
            </select>
          )}
          {pickDoc.value && (
            <button type="button" onClick={addSection}>
              Add
            </button>
          )}
        </div>
      </fieldset>
      <TargetPicker value={targets.value} onChange={(v) => (targets.value = v)} />
      {error.value && <p class="field-error">{error.value}</p>}
      <div class="row">
        <button type="submit" class="primary">
          {supersedes ? "Save and supersede" : "Save decision"}
        </button>
        <button type="button" class="quiet" onClick={() => onDone(false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}
