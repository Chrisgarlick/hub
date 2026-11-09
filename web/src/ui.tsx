// Shared bits of the UI: links that respect unsaved changes, and errors.

import { signal } from "@preact/signals";
import type { ComponentChildren } from "preact";

import { navigate } from "./router.ts";

/** Unsaved changes in the open document: asked about before leaving. */
export const dirty = signal(false);

window.addEventListener("beforeunload", (e) => {
  if (dirty.value) e.preventDefault();
});

export function go(to: string): void {
  if (dirty.value && !confirm("You have unsaved changes. Leave without saving?")) return;
  dirty.value = false;
  navigate(to);
}

export function Link({ href, class: cls, children }: { href: string; class?: string; children: ComponentChildren }) {
  return (
    <a
      href={href}
      class={cls}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        go(href);
      }}
    >
      {children}
    </a>
  );
}

export function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

