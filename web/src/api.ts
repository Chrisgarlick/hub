// Hub's REST API. A 401 sends you to sign in; other errors carry the
// server's `{ error, message, path?, current? }`.

export interface Space {
  id: string;
  slug: string;
  name: string;
  kind: "client" | "studio";
}

export interface Listed extends Space {
  documents: number;
}

export interface Summary {
  id: string;
  path: string;
  title: string;
  type: string;
  visibility: "internal" | "client" | "secret";
  version: number;
  updated_by: string;
  updated_at: string;
  deleted_at?: string;
}

export interface Heading {
  level: number;
  text: string;
  anchor: string;
}

export interface Override {
  anchor: string | null;
  decision: { id: string; statement: string; reason: string; decided_at: string };
}

export interface Doc extends Summary {
  space_id: string;
  content: string;
  created_by: string;
  created_at: string;
  headings: Heading[];
  overridden_by?: Override[];
}

export type Target = "all" | "board" | "design" | "motion" | "typeset";
export const TARGETS: Target[] = ["all", "board", "design", "motion", "typeset"];
export type RuleKind = "must" | "must_not" | "prefer" | "avoid";
export type Standing = "authoritative" | "tentative" | "outdated";

export interface Check {
  terms?: string[];
  pattern?: string;
  required_terms?: string[];
  case_sensitive?: boolean;
}

export interface Rule {
  id: string;
  space_id: string;
  kind: RuleKind;
  applies_to: Target[];
  text: string;
  check: Check | null;
  warning: string | null;
  source_doc: string | null;
  source_anchor: string | null;
  source_path?: string;
  status: "proposed" | "approved" | "retired";
  created_by: string;
  hard: boolean;
  standing: Standing;
  scope: "client" | "studio";
  excepted?: { reason: string };
}

export interface NewRule {
  kind: RuleKind;
  text: string;
  applies_to: Target[];
  check?: Check;
}

export interface Span {
  start: number;
  end: number;
  line: number;
  column: number;
  text: string;
}

export interface RuleBrief {
  id: string;
  kind: RuleKind;
  text: string;
  scope: "client" | "studio";
  source_path: string | null;
  source_anchor: string | null;
}

export interface Finding {
  rule: RuleBrief;
  spans: Span[];
  missing: string[];
}

export interface CheckResult {
  ok: boolean;
  for: Target;
  violations: Finding[];
  advice: Finding[];
  guidance: RuleBrief[];
  decisions: { id: string; statement: string; reason: string }[];
}

export interface Section {
  doc: string;
  path: string;
  anchor?: string;
}

export interface Evidence {
  app?: string;
  url: string;
  item?: string;
  note?: string;
}

export interface Decision {
  id: string;
  space_id: string;
  statement: string;
  reason: string;
  contradicts: Section[];
  applies_to: Target[];
  evidence: Evidence[];
  status: "proposed" | "approved" | "rejected" | "superseded";
  supersedes: string | null;
  superseded_by: string | null;
  proposed_by: string;
  decided_by: string | null;
  decided_at: string | null;
  created_at: string;
  standing: Standing;
}

export interface SearchResult {
  id: number;
  source: "document" | "rule" | "decision";
  space: { id: string; slug: string; name: string };
  heading_path: string;
  text: string;
  type: string;
  applies_to: Target[];
  score: number;
  standing: Standing;
  matched: { words: boolean; meaning: boolean };
  document?: { id: string; path: string; title: string; revision: number };
  anchor?: string | null;
  rule_id?: string;
  decision_id?: string;
  overridden_by?: { id: string; statement: string; reason: string }[];
}

export interface SearchResponse {
  results: SearchResult[];
  meaning: boolean;
  embeddings: string | null;
  note: string | null;
}

export interface IndexStatus {
  embeddings: string | null;
  chunks: number;
  embedded: number;
  words_only: number;
  queued: number;
  failing: number;
}

export interface NewDecision {
  statement: string;
  reason: string;
  contradicts: { doc: string; anchor?: string }[];
  applies_to: Target[];
  supersedes?: string;
}

export interface Passkey {
  id: string;
  name: string;
  created_at: string;
  last_used_at: string | null;
}

export interface Revision {
  id: number;
  version: number;
  title: string;
  content: string;
  changed_by: string;
  created_at: string;
  updated_at: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly current?: Doc,
  ) {
    super(message);
  }
}

async function call<T>(method: string, url: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, headers: {} };
  if (body !== undefined) {
    init.headers = { "content-type": "application/json" };
    init.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(url, init);
  } catch {
    throw new ApiError(0, "offline", "Can't reach Hub. Check your connection and try again.");
  }
  if (res.status === 401) {
    location.href = `/login?return=${encodeURIComponent(location.pathname + location.search)}`;
    throw new ApiError(401, "unauthorized", "Signing you in…");
  }
  const data = (await res.json().catch(() => null)) as Record<string, unknown> | null;
  if (!res.ok) {
    throw new ApiError(
      res.status,
      String(data?.["error"] ?? "error"),
      String(data?.["message"] ?? `Hub said ${res.status}`),
      data?.["current"] as Doc | undefined,
    );
  }
  return data as T;
}

const seg = encodeURIComponent;

export const api = {
  spaces: () => call<{ spaces: Listed[] }>("GET", "/api/spaces"),
  space: (slug: string) => call<{ space: Space; documents: Summary[] }>("GET", `/api/spaces/${seg(slug)}`),
  trash: (slug: string) => call<{ documents: Summary[] }>("GET", `/api/spaces/${seg(slug)}/trash`),
  create: (slug: string, path: string) => call<Doc>("POST", `/api/spaces/${seg(slug)}/documents`, { path }),
  get: (id: string) => call<Doc>("GET", `/api/documents/${seg(id)}`),
  save: (id: string, version: number, change: { content?: string; path?: string }) =>
    call<Doc>("PATCH", `/api/documents/${seg(id)}`, { version, ...change }),
  remove: (id: string) => call<Doc>("DELETE", `/api/documents/${seg(id)}`),
  undelete: (id: string) => call<Doc>("POST", `/api/documents/${seg(id)}/undelete`),
  revisions: (id: string) => call<{ revisions: Revision[] }>("GET", `/api/documents/${seg(id)}/revisions`),
  restore: (id: string, rev: number, version: number) =>
    call<Doc>("POST", `/api/documents/${seg(id)}/restore/${rev}`, { version }),
  sync: () => call<{ clients: number; created: number; events: number }>("POST", "/api/sync"),
  rules: (slug: string) => call<{ rules: Rule[] }>("GET", `/api/spaces/${seg(slug)}/rules`),
  addRule: (slug: string, r: NewRule) => call<Rule>("POST", `/api/spaces/${seg(slug)}/rules`, r),
  retireRule: (id: string) => call<Rule>("DELETE", `/api/rules/${seg(id)}`),
  approveRule: (id: string) => call<Rule>("POST", `/api/rules/${seg(id)}/approve`),
  except: (slug: string, id: string, reason: string) =>
    call<null>("PUT", `/api/spaces/${seg(slug)}/rules/${seg(id)}/exception`, { reason }),
  unexcept: (slug: string, id: string) => call<null>("DELETE", `/api/spaces/${seg(slug)}/rules/${seg(id)}/exception`),
  check: (slug: string, text: string, target: Target) =>
    call<CheckResult>("POST", `/api/spaces/${seg(slug)}/check`, { text, for: target }),
  decisions: (slug: string) => call<{ decisions: Decision[] }>("GET", `/api/spaces/${seg(slug)}/decisions`),
  decisionsMarkdown: (slug: string) => call<{ markdown: string }>("GET", `/api/spaces/${seg(slug)}/decisions.md`),
  addDecision: (slug: string, d: NewDecision) => call<Decision>("POST", `/api/spaces/${seg(slug)}/decisions`, d),
  approveDecision: (id: string) => call<Decision>("POST", `/api/decisions/${seg(id)}/approve`),
  rejectDecision: (id: string) => call<Decision>("POST", `/api/decisions/${seg(id)}/reject`),
  search: (q: string, space?: string) =>
    call<SearchResponse>(
      "GET",
      `/api/search?q=${encodeURIComponent(q)}${space ? `&space=${encodeURIComponent(space)}` : ""}`,
    ),
  indexStatus: () => call<IndexStatus>("GET", "/api/index"),
  passkeys: () => call<{ passkeys: Passkey[] }>("GET", "/api/passkeys"),
  removePasskey: (id: string) => call<null>("DELETE", `/api/passkeys/${seg(id)}`),
  logout: () => fetch("/logout", { method: "POST" }),
};
