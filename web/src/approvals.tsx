// Approvals: what Claude proposed, waiting for you; the hands-off switch and
// its policies; and the work linked to a client.

import { signal, useSignal } from "@preact/signals";
import { useEffect } from "preact/hooks";

import {
  api,
  type ApprovalSettings,
  type Artefact,
  type Listed,
  type Proposal,
  type ProposalKind,
} from "./api.ts";
import { docUrl, spaceUrl } from "./router.ts";
import { ago, full } from "./time.ts";
import { Link, message } from "./ui.tsx";

/** Waiting proposals, for the top bar. Refreshed every 30 seconds. */
export const pendingCount = signal(0);

export function watchPending(): void {
  const load = () =>
    api.proposals("pending").then(
      (r) => (pendingCount.value = r.pending),
      () => {},
    );
  void load();
  setInterval(load, 30_000);
}

const KIND: Record<ProposalKind, string> = {
  doc_edit: "Document edit",
  doc_create: "New document",
  rule: "Rule",
  decision: "Decision",
  artefact_link: "Linked work",
  library_put: "Library item",
  library_pin: "Library pin",
};

const STATUS: Record<string, string> = {
  applied: "Applied",
  rejected: "Rejected",
  withdrawn: "Withdrawn",
  stale: "Out of date",
  reverted: "Reverted",
};

/** A unified diff, coloured by line. */
function Diff({ text }: { text: string }) {
  const lines = text.split("\n").filter((l, i, all) => !(i === all.length - 1 && l === ""));
  return (
    <pre class="diff">
      {lines.map((l, i) => {
        const cls = l.startsWith("+++") || l.startsWith("---") ? "d-head" : l.startsWith("@@") ? "d-hunk" : l.startsWith("+") ? "d-add" : l.startsWith("-") ? "d-del" : "";
        return (
          <span key={i} class={cls}>
            {l}
            {"\n"}
          </span>
        );
      })}
    </pre>
  );
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** What the proposal would do, by kind. */
function Change({ p }: { p: Proposal }) {
  const c = p.change;
  switch (p.kind) {
    case "doc_edit":
    case "doc_create":
    case "library_put":
      return p.diff ? <Diff text={p.diff} /> : null;
    case "library_pin":
      return (
        <p class="change">
          {c["pin"] === false ? "Unpin" : "Pin"} {str(c["title"])} ({str(c["kind"])}/{str(c["name"])})
        </p>
      );
    case "rule": {
      const words = [
        ...((c["check"] as { terms?: string[] } | null)?.terms ?? []),
        ...((c["check"] as { required_terms?: string[] } | null)?.required_terms ?? []),
      ];
      return (
        <p class="change">
          <span class={`kind kind-${str(c["kind"])}`}>{str(c["kind"]).replace("_", " ")}</span> {str(c["text"])}
          {words.length > 0 && <span class="meta"> looks for {words.map((w) => `“${w}”`).join(", ")}</span>}
        </p>
      );
    }
    case "decision": {
      const over = (c["contradicts"] as { doc: string; anchor?: string }[] | undefined) ?? [];
      return (
        <div class="change">
          <p>
            <strong>{str(c["statement"])}</strong>
          </p>
          <p>{str(c["reason"])}</p>
          {over.length > 0 && (
            <p class="meta">Overrides {over.map((s) => `${s.doc}${s.anchor ? ` › ${s.anchor}` : ""}`).join(", ")}</p>
          )}
          {c["supersedes"] ? <p class="meta">Replaces decision {str(c["supersedes"])}</p> : null}
        </div>
      );
    }
    case "artefact_link":
      return (
        <p class="change">
          {str(c["app"])} {str(c["kind"])}:{" "}
          <a href={str(c["url"])} target="_blank" rel="noopener noreferrer">
            {str(c["title"])}
          </a>
        </p>
      );
  }
}

/** Where to go to see what an applied proposal made. */
function resultLink(p: Proposal): string | null {
  const r = p.result;
  const slug = p.space?.slug;
  if (!r || !slug) return null;
  if (typeof r["document"] === "string") return docUrl(slug, r["document"]);
  if (typeof r["decision"] === "string") return `${spaceUrl(slug)}/decisions#${r["decision"]}`;
  if (typeof r["rule"] === "string") return `${spaceUrl(slug)}/rules`;
  if (typeof r["artefact"] === "string") return `${spaceUrl(slug)}/work`;
  if (typeof r["library"] === "string" && p.kind === "library_put")
    return `/library/${str(p.change["kind"])}/${slug}/${str(p.change["name"])}`;
  if (typeof r["library"] === "string") return `${spaceUrl(slug)}/work`;
  return null;
}

function ProposalCard({ p, onDone }: { p: Proposal; onDone: () => void }) {
  const busy = useSignal(false);
  const error = useSignal("");
  const rejecting = useSignal(false);
  const note = useSignal("");

  const act = async (f: () => Promise<Proposal>) => {
    busy.value = true;
    error.value = "";
    try {
      const r = await f();
      if (r.status === "stale") error.value = "The document changed since Claude proposed this; ask Claude to propose it again.";
      onDone();
    } catch (e) {
      error.value = message(e);
    } finally {
      busy.value = false;
    }
  };

  const link = resultLink(p);
  return (
    <article class={`proposal is-${p.status}`}>
      <header class="proposal-head">
        <span class="badge">{KIND[p.kind]}</span>
        {p.space && (
          <Link href={spaceUrl(p.space.slug)} class="proposal-space">
            {p.space.name}
          </Link>
        )}
        {typeof p.change["path"] === "string" && <code>{p.change["path"]}</code>}
        {typeof p.change["label"] === "string" && <code>{p.change["label"]}</code>}
        <span class="meta" title={full(p.created_at)}>
          {p.proposed_by}, {ago(p.created_at)}
        </span>
      </header>
      <p class="rationale">{p.rationale}</p>
      <Change p={p} />
      {p.sources.length > 0 && (
        <p class="meta">
          Based on{" "}
          {p.sources
            .map((s) => str(s["doc"]) + (s["anchor"] ? ` › ${str(s["anchor"])}` : "") || str(s["decision"]) || str(s["url"]))
            .join(", ")}
        </p>
      )}
      {p.stays_manual && p.status === "pending" && (
        <p class="meta">Always waits for you here: {p.stays_manual}.</p>
      )}
      {error.value && <p class="field-error">{error.value}</p>}
      {p.status === "pending" ? (
        rejecting.value ? (
          <form
            class="row"
            onSubmit={(e) => {
              e.preventDefault();
              void act(() => api.rejectProposal(p.id, note.value));
            }}
          >
            <input
              class="grow"
              placeholder="Why not (optional, Claude sees it)"
              value={note.value}
              onInput={(e) => (note.value = e.currentTarget.value)}
              ref={(el) => el?.focus()}
            />
            <button type="submit" disabled={busy.value}>
              Reject
            </button>
            <button type="button" class="quiet" onClick={() => (rejecting.value = false)}>
              Cancel
            </button>
          </form>
        ) : (
          <div class="row">
            <button type="button" class="primary" disabled={busy.value} onClick={() => act(() => api.approveProposal(p.id))}>
              Approve
            </button>
            <button type="button" class="quiet" disabled={busy.value} onClick={() => (rejecting.value = true)}>
              Reject
            </button>
          </div>
        )
      ) : (
        <p class="decided">
          <span>
            {STATUS[p.status] ?? p.status}
            {p.decided_by && p.decided_by !== "user" && `, by ${p.decided_by}`}
            {p.decided_at && ` ${ago(p.decided_at)}`}
            {p.note && `: “${p.note}”`}
          </span>
          {link && p.status === "applied" && <Link href={link}>See it</Link>}
          {p.status === "applied" && (
            <button
              type="button"
              class="quiet"
              disabled={busy.value}
              onClick={() => confirm("Undo this change?") && act(() => api.revertProposal(p.id))}
            >
              Revert
            </button>
          )}
        </p>
      )}
    </article>
  );
}

export function ApprovalsPage() {
  const list = useSignal<Proposal[] | null>(null);
  const error = useSignal("");
  const load = () =>
    api.proposals("all").then(
      (r) => {
        list.value = r.proposals;
        pendingCount.value = r.pending;
      },
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), []);

  const all = list.value;
  const waiting = all?.filter((p) => p.status === "pending") ?? [];
  const done = all?.filter((p) => p.status !== "pending").slice(0, 30) ?? [];
  return (
    <main class="approvals">
      <div class="page-head">
        <h1>Approvals</h1>
        <Link href="/settings" class="top-link">
          Hands-off settings
        </Link>
      </div>
      <p class="lede">
        What Claude has proposed. Nothing changes until you approve it, unless you've let a kind of change through in
        the hands-off settings.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!all ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          {waiting.length ? (
            waiting.map((p) => <ProposalCard key={p.id} p={p} onDone={load} />)
          ) : (
            <p class="empty">Nothing waiting for you.</p>
          )}
          {done.length > 0 && (
            <section class="group">
              <h2>Recently decided</h2>
              {done.map((p) => (
                <ProposalCard key={p.id} p={p} onDone={load} />
              ))}
            </section>
          )}
        </>
      )}
    </main>
  );
}

// ---------- Hands-off settings ----------

const KIND_ORDER: ProposalKind[] = ["doc_edit", "doc_create", "rule", "decision", "artefact_link", "library_put", "library_pin"];

export function SettingsPage() {
  const settings = useSignal<ApprovalSettings | null>(null);
  const spaces = useSignal<Listed[]>([]);
  const error = useSignal("");
  useEffect(() => {
    api.approvalSettings().then(
      (s) => (settings.value = s),
      (e) => (error.value = message(e)),
    );
    api.spaces().then(
      (r) => (spaces.value = r.spaces),
      () => {},
    );
  }, []);

  const save = async (patch: Parameters<typeof api.setApprovalSettings>[0]) => {
    error.value = "";
    try {
      settings.value = await api.setApprovalSettings(patch);
    } catch (e) {
      error.value = message(e);
    }
  };

  const s = settings.value;
  const mode = (space: string, kind: ProposalKind) => s?.policies.find((p) => p.space === space && p.kind === kind)?.mode;
  const rows: { id: string; name: string }[] = [
    { id: "*", name: "Every client" },
    ...spaces.value.map((x) => ({ id: x.id, name: x.name })),
  ];

  return (
    <main class="approvals">
      <Link href="/approvals" class="back">
        Approvals
      </Link>
      <h1>Hands-off</h1>
      <p class="lede">
        With the switch off, everything Claude proposes waits for you. With it on, the kinds you allow below apply at
        once. They're still recorded, and you can revert them from Approvals.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!s ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          <label class={s.hands_off ? "switch is-on" : "switch"}>
            <input type="checkbox" checked={s.hands_off} onChange={(e) => save({ hands_off: e.currentTarget.checked })} />
            <span>{s.hands_off ? "Hands-off is on" : "Hands-off is off: everything waits for you"}</span>
          </label>
          <p class="meta">
            Wait: it waits for you in Approvals. Apply: it applies at once while the switch is on. A client's
            "Default" follows the Every client row. Whatever you choose, an edit to a document clients can see always
            waits for you, and Claude can't approve one for you.
          </p>
          <div class="table-wrap">
            <table class="policies">
              <thead>
                <tr>
                  <th scope="col">Client</th>
                  {KIND_ORDER.map((k) => (
                    <th scope="col" key={k}>
                      {KIND[k]}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.id}>
                    <th scope="row">{r.name}</th>
                    {KIND_ORDER.map((k) => {
                      const own = mode(r.id, k);
                      const inherited = r.id === "*" ? "review" : (mode("*", k) ?? "review");
                      return (
                        <td key={k}>
                          <select
                            aria-label={`${r.name}: ${KIND[k]}`}
                            value={own ?? (r.id === "*" ? "review" : "")}
                            onChange={(e) => {
                              const v = e.currentTarget.value as "review" | "auto" | "";
                              void save({ policies: [{ space: r.id, kind: k, mode: v || (r.id === "*" ? "review" : inherited) }] });
                            }}
                          >
                            {r.id !== "*" && <option value="">Default: {inherited === "auto" ? "apply" : "wait"}</option>}
                            <option value="review">Wait</option>
                            <option value="auto">Apply</option>
                          </select>
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </main>
  );
}

// ---------- A client's linked work ----------

export function WorkPage({ slug }: { slug: string }) {
  const list = useSignal<Artefact[] | null>(null);
  const error = useSignal("");
  const load = () =>
    api.artefacts(slug).then(
      (r) => (list.value = r.artefacts),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), [slug]);

  return (
    <div class="page">
      <h2>Linked work</h2>
      <p class="meta">Boards, designs, scenes and PDFs made for this client, linked by Claude as it works.</p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!list.value ? (
        <p class="meta">Loading…</p>
      ) : list.value.length === 0 ? (
        <p class="empty">Nothing linked yet.</p>
      ) : (
        <ul class="trash-list">
          {list.value.map((a) => (
            <li key={a.id}>
              <span>
                <a href={a.url} target="_blank" rel="noopener noreferrer">
                  {a.title}
                </a>{" "}
                <span class="meta">
                  {a.app} {a.kind}
                </span>
              </span>
              <span class="meta" title={full(a.created_at)}>
                {ago(a.created_at)}
              </span>
              <button
                type="button"
                class="quiet"
                onClick={() =>
                  confirm(`Unlink “${a.title}”? It stays in ${a.app}.`) &&
                  api.removeArtefact(a.id).then(load, (e) => (error.value = message(e)))
                }
              >
                Unlink
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
