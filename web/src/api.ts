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

export interface Doc extends Summary {
  space_id: string;
  content: string;
  created_by: string;
  created_at: string;
  headings: Heading[];
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
  passkeys: () => call<{ passkeys: Passkey[] }>("GET", "/api/passkeys"),
  removePasskey: (id: string) => call<null>("DELETE", `/api/passkeys/${seg(id)}`),
  logout: () => fetch("/logout", { method: "POST" }),
};
