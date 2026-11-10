// A tiny router: a path signal and the pages it maps to.

import { signal } from "@preact/signals";

/** The path and query string (not the #section). */
const here = () => location.pathname + location.search;

export const path = signal(here());

window.addEventListener("popstate", () => {
  path.value = here();
});

export function navigate(to: string, replace = false): void {
  if (to === here() + location.hash) return;
  if (replace) history.replaceState(null, "", to);
  else history.pushState(null, "", to);
  path.value = here();
}

/** A query-string value of the current page. */
export const param = (name: string): string => new URLSearchParams(path.value.split("?")[1] ?? "").get(name) ?? "";

export type Route =
  | { page: "spaces" }
  | { page: "login" }
  | { page: "passkeys" }
  | { page: "search" }
  | { page: "approvals" }
  | { page: "runs" }
  | { page: "run"; id: string }
  | { page: "library" }
  | { page: "libraryItem"; kind: string; scope: string; name: string }
  | { page: "settings" }
  | { page: "space"; slug: string; doc?: string; view?: "trash" | "rules" | "decisions" | "work" | "runs" }
  | { page: "missing" };

export function route(full: string): Route {
  const p = full.split("?")[0] ?? "";
  if (p === "/" || p === "") return { page: "spaces" };
  if (p === "/search" || p === "/search/") return { page: "search" };
  if (p === "/approvals" || p === "/approvals/") return { page: "approvals" };
  if (p === "/runs" || p === "/runs/") return { page: "runs" };
  const run = /^\/runs\/(run_[0-9A-Za-z]+)\/?$/.exec(p);
  if (run?.[1]) return { page: "run", id: run[1] };
  if (p === "/library" || p === "/library/") return { page: "library" };
  const lib = /^\/library\/([a-z0-9-]+)\/([a-z0-9_-]+)\/([a-z0-9-]+)\/?$/.exec(p);
  if (lib?.[1] && lib[2] && lib[3]) return { page: "libraryItem", kind: lib[1], scope: lib[2], name: lib[3] };
  if (p === "/settings" || p === "/settings/") return { page: "settings" };
  if (p === "/login" || p === "/login/") return { page: "login" };
  if (p === "/passkeys" || p === "/passkeys/") return { page: "passkeys" };
  const m = /^\/s\/([a-z0-9_-]+)(?:\/(trash|rules|decisions|work|runs|d\/(doc_[0-9A-Za-z]+)))?\/?$/.exec(p);
  if (m?.[1]) {
    const r: Route = { page: "space", slug: m[1] };
    if (m[2] === "trash" || m[2] === "rules" || m[2] === "decisions" || m[2] === "work" || m[2] === "runs") r.view = m[2];
    if (m[3]) r.doc = m[3];
    return r;
  }
  return { page: "missing" };
}

export const spaceUrl = (slug: string) => `/s/${slug}`;
export const docUrl = (slug: string, id: string) => `/s/${slug}/d/${id}`;

export const libraryUrl = (kind: string, scope: string, name: string) => `/library/${kind}/${scope}/${name}`;
export const runUrl = (id: string) => `/runs/${id}`;
