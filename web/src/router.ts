// A tiny router: a path signal and the pages it maps to.

import { signal } from "@preact/signals";

export const path = signal(location.pathname);

window.addEventListener("popstate", () => {
  path.value = location.pathname;
});

export function navigate(to: string): void {
  if (to === location.pathname) return;
  history.pushState(null, "", to);
  path.value = to;
}

export type Route =
  | { page: "spaces" }
  | { page: "login" }
  | { page: "passkeys" }
  | { page: "space"; slug: string; doc?: string; trash?: boolean }
  | { page: "missing" };

export function route(p: string): Route {
  if (p === "/" || p === "") return { page: "spaces" };
  if (p === "/login" || p === "/login/") return { page: "login" };
  if (p === "/passkeys" || p === "/passkeys/") return { page: "passkeys" };
  const m = /^\/s\/([a-z0-9_-]+)(?:\/(trash|d\/(doc_[0-9A-Za-z]+)))?\/?$/.exec(p);
  if (m?.[1]) {
    const r: Route = { page: "space", slug: m[1] };
    if (m[2] === "trash") r.trash = true;
    if (m[3]) r.doc = m[3];
    return r;
  }
  return { page: "missing" };
}

export const spaceUrl = (slug: string) => `/s/${slug}`;
export const docUrl = (slug: string, id: string) => `/s/${slug}/d/${id}`;
