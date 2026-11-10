// Approvals: what Claude changed (applied, revertible) and what's waiting
// for you (removals, and kinds you've set to wait); those settings; and the
// work linked to a client.

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
import { WaitingCheckpoints } from "./runs.tsx";

/** Waiting proposals, for the top bar. Refreshed every 30 seconds. */
export const pendingCount = signal(0);

export function watchPending(): void {
  const load = () =>
    api.proposals("pending").then(
      (r) => (pendingCount.value = r.pending + r.checkpoints),
      () => {},
    );
  void load();
  setInterval(load, 30_000);
}

const KIND: Record<ProposalKind, string> = {
  doc_edit: "Document edit",
  doc_create: "New document",
  doc_delete: "Delete document",
  rule: "Rule",
  rule_edit: "Rule change",
  rule_retire: "Retire rule",
  decision: "Decision",
  decision_edit: "Decision change",
  decision_retire: "Retire decision",
  artefact_link: "Linked work",
  artefact_unlink: "Unlink work",
  library_put: "Library item",
  library_pin: "Library pin",
  library_delete: "Delete library item",
  client_create: "New client",
  client_update: "Rename client",
  client_delete: "Delete client",
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
    case "doc_delete":
      return <p class="change">Move {str(c["path"])} to the trash</p>;
    case "rule_edit": {
      const n = (c["new"] ?? {}) as Record<string, unknown>;
      const b = (c["base"] ?? {}) as Record<string, unknown>;
      return (
        <p class="change">
          {str(b["text"]) !== str(n["text"]) ? (
            <>
              <del>{str(b["text"])}</del> → {str(n["text"])}
            </>
          ) : (
            str(n["text"])
          )}
        </p>
      );
    }
    case "rule_retire":
      return <p class="change">Retire “{str(c["text"])}”</p>;
    case "decision_edit":
      return (
        <p class="change">
          “{str(c["statement"])}”: changes {Object.keys((c["patch"] ?? {}) as object).join(", ")}
        </p>
      );
    case "decision_retire":
      return <p class="change">Retire “{str(c["statement"])}”</p>;
    case "artefact_unlink":
      return <p class="change">Unlink {str(c["title"])}</p>;
    case "library_delete":
      return <p class="change">Delete {str(c["title"])} ({str(c["label"])})</p>;
    case "client_create":
      return (
        <p class="change">
          Add {str(c["name"])} (<code>{str(c["slug"])}</code>) in Clients, with a space here
        </p>
      );
    case "client_update":
      return (
        <p class="change">
          Rename {str(c["old_name"])} to {str(c["name"])}
        </p>
      );
    case "client_delete":
      return (
        <p class="change">
          Delete {str(c["name"])} from Clients (brand, library) and archive its space here (documents are kept)
        </p>
      );
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

/** What a change does, for its colour: adds, changes or removes. */
function action(kind: ProposalKind): "add" | "change" | "remove" {
  if (kind.endsWith("_delete") || kind.endsWith("_retire") || kind === "artefact_unlink") return "remove";
  if (kind.endsWith("_edit") || kind.endsWith("_update") || kind === "library_put" || kind === "library_pin")
    return "change";
  return "add";
}

/** Adding or deleting a client can't be undone from Hub. */
const revertible = (k: ProposalKind) => k !== "client_create" && k !== "client_delete";

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
    <article class={`proposal is-${p.status} act-${action(p.kind)}`}>
      <header class="proposal-head">
        <span class="badge kind-badge">{KIND[p.kind]}</span>
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
            {p.status === "applied" && p.decided_by === "auto" ? "Applied at once" : (STATUS[p.status] ?? p.status)}
            {p.decided_by && p.decided_by !== "user" && p.decided_by !== "auto" && `, by ${p.decided_by}`}
            {p.decided_at && ` ${ago(p.decided_at)}`}
            {p.note && `: “${p.note}”`}
          </span>
          {link && p.status === "applied" && <Link href={link}>See it</Link>}
          {p.status === "applied" && revertible(p.kind) && (
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
        pendingCount.value = r.pending + r.checkpoints;
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
          What waits for me
        </Link>
      </div>
      <p class="lede">
        What Claude has changed, and what's waiting for you. Its creates and edits apply at once (revert any below);
        anything it removes waits for you here, and so does anything you've set to wait.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!all ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          <WaitingCheckpoints />
          {waiting.length ? (
            waiting.map((p) => <ProposalCard key={p.id} p={p} onDone={load} />)
          ) : (
            <p class="empty">Nothing waiting for you.</p>
          )}
          {done.length > 0 && (
            <section class="group">
              <h2>Recent changes</h2>
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

// ---------- What waits ----------

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

  const save = async (policies: ApprovalSettings["policies"]) => {
    error.value = "";
    try {
      settings.value = await api.setApprovalSettings({ policies });
    } catch (e) {
      error.value = message(e);
    }
  };

  const s = settings.value;
  const mode = (space: string, kind: ProposalKind) => s?.policies.find((p) => p.space === space && p.kind === kind)?.mode;
  const cols: { id: string; name: string }[] = [
    { id: "*", name: "Every client" },
    ...spaces.value.map((x) => ({ id: x.id, name: x.name })),
  ];

  return (
    <main class="approvals">
      <Link href="/approvals" class="back">
        Approvals
      </Link>
      <h1>What waits for me</h1>
      <p class="lede">
        Claude's creates and edits apply at once, and you can revert them from Approvals. Choose any you'd rather
        approve first. Removals always wait for you, and so does any change to a document clients can see.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!s ? (
        <p class="meta">Loading…</p>
      ) : (
        <div class="table-wrap">
          <table class="policies">
            <thead>
              <tr>
                <th scope="col">Change</th>
                {cols.map((c) => (
                  <th scope="col" key={c.id}>
                    {c.name}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {s.kinds.map((k) => (
                <tr key={k}>
                  <th scope="row">{KIND[k]}</th>
                  {cols.map((c) => {
                    const own = mode(c.id, k);
                    const inherited = mode("*", k) ?? "auto";
                    return (
                      <td key={c.id}>
                        <select
                          aria-label={`${KIND[k]}: ${c.name}`}
                          value={own ?? (c.id === "*" ? "auto" : "")}
                          onChange={(e) => {
                            const v = e.currentTarget.value as "review" | "auto" | "";
                            void save([{ space: c.id, kind: k, mode: v || inherited }]);
                          }}
                        >
                          {c.id !== "*" && <option value="">Default: {inherited === "auto" ? "apply" : "wait"}</option>}
                          <option value="auto">Apply</option>
                          <option value="review">Wait</option>
                        </select>
                      </td>
                    );
                  })}
                </tr>
              ))}
              {s.deletes.map((k) => (
                <tr key={k} class="fixed">
                  <th scope="row">{KIND[k]}</th>
                  <td colSpan={cols.length}>Always waits for you</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
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
