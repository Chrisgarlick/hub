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

const marked = new Marked({
  gfm: true,
  breaks: true,
  renderer: {
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

export function renderMarkdown(source: string): string {
  return marked.parse(source, { async: false });
}
