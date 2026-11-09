// Markdown for documents, rendered without raw HTML (as in Board):
// HTML in the source is shown as text, links are limited to safe schemes, and
// images are shown as links (Hub doesn't load third-party images).

import { Marked } from "marked";

const SAFE_URL = /^(https?:|mailto:|\/|#)/i;

function escape(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** A heading's anchor, as the server makes it (crates/hub/src/documents.rs). */
export function slug(text: string): string {
  let out = "";
  let dash = false;
  for (const ch of text.toLowerCase()) {
    if (/[\p{L}\p{N}]/u.test(ch)) {
      out += ch;
      dash = false;
    } else if ((/\s/.test(ch) || ch === "-" || ch === "_") && out && !dash) {
      out += "-";
      dash = true;
    }
  }
  return out.replace(/-+$/, "");
}

/** Notes to show under headings, by anchor ("" for the whole document). */
let notes: Map<string, string> = new Map();
let seen: string[] = [];

const marked = new Marked({
  gfm: true,
  breaks: true,
  renderer: {
    heading({ tokens, depth, text }) {
      const inner = this.parser.parseInline(tokens);
      const base = slug(text.replace(/#+\s*$/, "").trim());
      let anchor = base;
      for (let n = 2; seen.includes(anchor); n++) anchor = `${base}-${n}`;
      if (base) seen.push(anchor);
      const id = base ? ` id="${escape(anchor)}"` : "";
      return `<h${depth}${id}>${inner}</h${depth}>\n${notes.get(anchor) ?? ""}`;
    },
    html({ text }) {
      return escape(text);
    },
    link({ href, tokens }) {
      const label = this.parser.parseInline(tokens);
      if (!SAFE_URL.test(href.trim())) return label;
      return `<a href="${escape(href)}" target="_blank" rel="noopener noreferrer">${label}</a>`;
    },
    image({ href, text }) {
      if (!SAFE_URL.test(href.trim())) return escape(text);
      return `<a href="${escape(href)}" target="_blank" rel="noopener noreferrer">${escape(text || href)}</a>`;
    },
  },
});

/**
 * Render markdown; `under` puts trusted HTML after the heading with that
 * anchor (the empty anchor goes first).
 */
export function renderMarkdown(source: string, under?: Map<string, string>): string {
  notes = under ?? new Map();
  seen = [];
  const html = marked.parse(source, { async: false });
  return (notes.get("") ?? "") + html;
}

export { escape as escapeHtml };
