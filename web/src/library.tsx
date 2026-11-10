// The library: reusable files the apps work from (Design's style
// references first), by kind; an item to read, edit and pin to clients.

import { useSignal } from "@preact/signals";
import { useEffect } from "preact/hooks";

import { api, type LibraryItem, type LibraryRevision, type Listed, type SearchResult } from "./api.ts";
import { renderMarkdown } from "./markdown.ts";
import { libraryUrl, navigate } from "./router.ts";
import { ago, full } from "./time.ts";
import { Link, message } from "./ui.tsx";

/** Which app each kind of file is for, what it's for, and who reads it. */
interface Kind {
  kind: string;
  /** "Styles", "Guides". */
  name: string;
  /** One of them: "a style", "a guide". */
  one: string;
  what: string;
  /** How it reaches the app. */
  read: string;
  placeholder: string;
}

interface App {
  app: string;
  name: string;
  kinds: Kind[];
}

export const APPS: App[] = [
  {
    app: "design",
    name: "Design",
    kinds: [
      {
        kind: "design-style",
        name: "Styles",
        one: "a style",
        what: "Style references (DESIGN.md files): the type scale, spacing, components and do's and don'ts Design borrows, on top of a client's own colours, fonts and logos.",
        read: "Design reads these itself (its Styles panel and Claude in Design).",
        placeholder: "# ElevenLabs — Style Reference\n> Calm, editorial, generous spacing\n\n## Typography\n…",
      },
    ],
  },
  {
    app: "motion",
    name: "Motion",
    kinds: [
      {
        kind: "motion-guide",
        name: "Guides",
        one: "a guide",
        what: "How motion graphics should be made: pacing and easing, how much text a card holds, what makes a reel card or carousel land.",
        read: "Claude reads these from Hub before a Motion job.",
        placeholder: "# Reel cards\n> Short, punchy, one idea per card\n\n## Timing\n…",
      },
    ],
  },
  {
    app: "typeset",
    name: "Typeset",
    kinds: [
      {
        kind: "typeset-guide",
        name: "Guides",
        one: "a guide",
        what: "How reports and proposals are put together: their structure, what each section must say, tone and length.",
        read: "Claude reads these from Hub before writing a document for Typeset.",
        placeholder: "# Monthly report\n> What we did, what it changed, what's next\n\n## Sections\n…",
      },
    ],
  },
  {
    app: "board",
    name: "Board",
    kinds: [
      {
        kind: "board-guide",
        name: "Guides",
        one: "a guide",
        what: "How a client's plan is laid out: which flows, tables and sitemap a job needs, and how cards are written.",
        read: "Claude reads these from Hub before planning in Board.",
        placeholder: "# Website plan\n> Every page, every flow, the data behind them\n\n## Always include\n…",
      },
    ],
  },
];

const KNOWN = new Map(APPS.flatMap((a) => a.kinds.map((k) => [k.kind, { app: a, kind: k }] as const)));

/** "Design styles", "Motion guides", or the kind itself for one we don't know. */
export const kindName = (k: string) => {
  const known = KNOWN.get(k);
  return known ? `${known.app.name} ${known.kind.name.toLowerCase()}` : k;
};

export function LibraryPage() {
  const items = useSignal<LibraryItem[] | null>(null);
  const tab = useSignal("");
  const q = useSignal("");
  const results = useSignal<SearchResult[] | null>(null);
  const error = useSignal("");
  const adding = useSignal<string | null>(null);

  const load = () =>
    api.library().then(
      (r) => (items.value = r.items),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), []);

  const search = async (e: Event) => {
    e.preventDefault();
    if (!q.value.trim()) return (results.value = null);
    try {
      results.value = (await api.searchLibrary(q.value)).results;
    } catch (err) {
      error.value = message(err);
    }
  };

  const all = items.value ?? [];
  const other = all.filter((i) => !KNOWN.has(i.kind));
  const apps = APPS.filter((a) => !tab.value || a.app === tab.value);
  const count = (a: App) => all.filter((i) => a.kinds.some((k) => k.kind === i.kind)).length;

  return (
    <main class="approvals">
      <div class="page-head">
        <h1>Library</h1>
      </div>
      <p class="lede">
        Files each app works from, kept once so every session can use them. Most are for every client; a client can
        have its own, and you pin the ones a client uses so Claude sees them in that client's context.
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      <form class="search-box" role="search" onSubmit={search}>
        <input
          type="search"
          aria-label="Search the library"
          placeholder="Search the library: calm, editorial, big type…"
          value={q.value}
          onInput={(e) => (q.value = e.currentTarget.value)}
        />
      </form>
      {results.value ? (
        <>
          <p class="meta">
            {results.value.length} {results.value.length === 1 ? "match" : "matches"}.{" "}
            <button type="button" class="quiet" onClick={() => ((results.value = null), (q.value = ""))}>
              Show everything
            </button>
          </p>
          <ol class="results">
            {results.value.map((r) =>
              r.library ? (
                <li key={r.id} class="result">
                  <span class={`app-chip app-${KNOWN.get(r.library.kind)?.app.app ?? "other"}`}>
                    {kindName(r.library.kind)}
                  </span>
                  <Link href={libraryUrl(r.library.kind, r.library.scope, r.library.name)} class="result-title">
                    {r.heading_path}
                  </Link>
                  <p class="result-text">{r.text.slice(0, 300)}</p>
                </li>
              ) : null,
            )}
          </ol>
        </>
      ) : !items.value ? (
        <p class="meta">Loading…</p>
      ) : (
        <>
          <div class="modes app-tabs" role="group" aria-label="App">
            <button type="button" aria-pressed={!tab.value} onClick={() => (tab.value = "")}>
              Every app
            </button>
            {APPS.map((a) => (
              <button type="button" key={a.app} aria-pressed={tab.value === a.app} onClick={() => (tab.value = a.app)}>
                {a.name}
                <span class="tab-count">{count(a)}</span>
              </button>
            ))}
          </div>
          {apps.map((a) =>
            a.kinds.map((k) => {
              const list = all.filter((i) => i.kind === k.kind);
              return (
                <section key={k.kind} class={`lib-app app-${a.app}`}>
                  <div class="lib-app-head">
                    <h2>
                      {a.name} <span>{k.name.toLowerCase()}</span>
                    </h2>
                    <button type="button" class="quiet" onClick={() => (adding.value = adding.value === k.kind ? null : k.kind)}>
                      {adding.value === k.kind ? "Close" : `Add ${k.one}`}
                    </button>
                  </div>
                  <p class="meta">
                    {k.what} {k.read}
                  </p>
                  {adding.value === k.kind && <NewItem k={k} onDone={() => ((adding.value = null), void load())} />}
                  {list.length ? (
                    <ul class="space-list">
                      {list.map((i) => (
                        <ItemRow key={i.id} i={i} />
                      ))}
                    </ul>
                  ) : (
                    <p class="empty">No {a.name} {k.name.toLowerCase()} yet.</p>
                  )}
                </section>
              );
            }),
          )}
          {!tab.value && other.length > 0 && (
            <section class="lib-app app-other">
              <div class="lib-app-head">
                <h2>Other</h2>
              </div>
              <ul class="space-list">
                {other.map((i) => (
                  <ItemRow key={i.id} i={i} showKind />
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </main>
  );
}

function ItemRow({ i, showKind }: { i: LibraryItem; showKind?: boolean }) {
  return (
    <li>
      <Link href={libraryUrl(i.kind, i.scope, i.name)} class="space-row">
        <span>
          <span class="space-name">{i.title}</span>
          {i.summary && <span class="item-summary">{i.summary}</span>}
        </span>
        <span class="space-meta">
          {showKind && `${i.kind}, `}
          {i.scope === "_studio" ? "every client" : `${i.scope} only`}
          {i.tags.length > 0 && `, ${i.tags.join(", ")}`}
        </span>
      </Link>
    </li>
  );
}

function NewItem({ k, onDone }: { k: Kind; onDone: () => void }) {
  const name = useSignal("");
  const scope = useSignal("_studio");
  const body = useSignal("");
  const tags = useSignal("");
  const source = useSignal("");
  const error = useSignal("");
  const spaces = useSignal<Listed[]>([]);
  useEffect(() => {
    api.spaces().then(
      (r) => (spaces.value = r.spaces),
      () => {},
    );
  }, []);

  const submit = async (e: Event) => {
    e.preventDefault();
    error.value = "";
    try {
      const r = await api.putLibrary(k.kind, scope.value, name.value.trim(), {
        body: body.value,
        tags: tags.value.split(",").map((t) => t.trim()).filter(Boolean),
        ...(source.value.trim() ? { source: source.value.trim() } : {}),
      });
      onDone();
      navigate(libraryUrl(r.item.kind, r.item.scope, r.item.name));
    } catch (err) {
      error.value = message(err);
    }
  };

  return (
    <form class="card-form" onSubmit={submit}>
      <div class="row">
        <label>
          Name (lowercase, with -)
          <input placeholder="reel-cards" value={name.value} onInput={(e) => (name.value = e.currentTarget.value)} />
        </label>
        <label>
          For
          <select value={scope.value} onChange={(e) => (scope.value = e.currentTarget.value)}>
            {spaces.value.map((s) => (
              <option key={s.id} value={s.slug}>
                {s.kind === "studio" ? "Every client" : `${s.name} only`}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label>
        The file: markdown. Its first # line is the title, its first &gt; line the summary.
        <textarea
          class="tall"
          placeholder={k.placeholder}
          value={body.value}
          onInput={(e) => (body.value = e.currentTarget.value)}
        />
      </label>
      <div class="row">
        <label>
          Tags, comma-separated
          <input value={tags.value} onInput={(e) => (tags.value = e.currentTarget.value)} />
        </label>
        <label>
          Where it came from
          <input placeholder="Refero Styles" value={source.value} onInput={(e) => (source.value = e.currentTarget.value)} />
        </label>
      </div>
      {error.value && <p class="field-error">{error.value}</p>}
      <div class="row">
        <button type="submit" class="primary">
          Save {k.one}
        </button>
        <button type="button" class="quiet" onClick={onDone}>
          Cancel
        </button>
      </div>
    </form>
  );
}

export function LibraryItemPage({ kind, scope, name }: { kind: string; scope: string; name: string }) {
  const item = useSignal<LibraryItem | null>(null);
  const error = useSignal("");
  const editing = useSignal(false);
  const history = useSignal<LibraryRevision[] | null>(null);
  const spaces = useSignal<Listed[]>([]);

  const load = () =>
    api.libraryItem(kind, scope, name).then(
      (i) => (item.value = i),
      (e) => (error.value = message(e)),
    );
  useEffect(() => {
    void load();
    api.spaces().then(
      (r) => (spaces.value = r.spaces.filter((s) => s.kind === "client")),
      () => {},
    );
  }, [kind, scope, name]);

  const i = item.value;
  if (error.value && !i)
    return (
      <main class="approvals">
        <p class="is-bad">{error.value}</p>
        <Link href="/library">Library</Link>
      </main>
    );
  if (!i) return <p class="page-note">Loading…</p>;

  const pinned = new Set((i.pinned_to ?? []).map((p) => p.id));
  const togglePin = async (s: Listed) => {
    try {
      if (pinned.has(s.id)) await api.unpin(s.slug, i.id);
      else await api.pin(s.slug, i.id);
      await load();
    } catch (e) {
      error.value = message(e);
    }
  };

  const showHistory = async () => {
    if (history.value) return (history.value = null);
    history.value = (await api.libraryRevisions(kind, scope, name)).revisions;
  };

  const restore = async (r: LibraryRevision) => {
    if (!confirm(`Put version ${r.version} back? (It becomes a new version.)`)) return;
    try {
      await api.putLibrary(kind, scope, name, { body: r.body, version: i.version });
      history.value = null;
      await load();
    } catch (e) {
      error.value = message(e);
    }
  };

  return (
    <main class="approvals">
      <Link href="/library" class="back">
        Library › {kindName(i.kind)}
      </Link>
      <div class="page-head">
        <h1>{i.title}</h1>
        <div class="row">
          <button type="button" class="quiet" aria-pressed={history.value !== null} onClick={showHistory}>
            History
          </button>
          <button type="button" class={editing.value ? "quiet" : "primary"} onClick={() => (editing.value = !editing.value)}>
            {editing.value ? "Stop editing" : "Edit"}
          </button>
        </div>
      </div>
      <p class="meta" title={full(i.updated_at)}>
        {kindName(i.kind)}, <code>{i.name}</code>
        {i.scope === "_studio" ? ", for every client" : `, ${i.scope}'s own`}, version {i.version}, edited{" "}
        {ago(i.updated_at)} by {i.updated_by}
        {i.source && `. From ${i.source}`}
        {i.tags.length > 0 && `. ${i.tags.join(", ")}`}
      </p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {i.scope === "_studio" && spaces.value.length > 0 && (
        <fieldset class="pins">
          <legend>Clients that use it</legend>
          {spaces.value.map((s) => (
            <label key={s.id}>
              <input type="checkbox" checked={pinned.has(s.id)} onChange={() => togglePin(s)} />
              {s.name}
            </label>
          ))}
        </fieldset>
      )}
      {history.value && (
        <ol class="lib-history">
          {history.value.map((r, n) => (
            <li key={r.id}>
              <span>
                Version {r.version}, {r.changed_by}, <span title={full(r.created_at)}>{ago(r.created_at)}</span>
              </span>
              {n > 0 && (
                <button type="button" class="quiet" onClick={() => restore(r)}>
                  Put this back
                </button>
              )}
            </li>
          ))}
        </ol>
      )}
      {editing.value ? (
        <EditItem
          item={i}
          onSaved={(n) => {
            item.value = { ...i, ...n };
            editing.value = false;
            void load();
          }}
          onDelete={async () => {
            if (!confirm(`Delete ${i.title}? Clients that pin it lose it.`)) return;
            try {
              await api.deleteLibrary(kind, scope, name);
              navigate("/library");
            } catch (e) {
              error.value = message(e);
            }
          }}
        />
      ) : (
        <article class="prose lib-body" dangerouslySetInnerHTML={{ __html: renderMarkdown(i.body ?? "") }} />
      )}
    </main>
  );
}

function EditItem({
  item,
  onSaved,
  onDelete,
}: {
  item: LibraryItem;
  onSaved: (i: LibraryItem) => void;
  onDelete: () => void;
}) {
  const body = useSignal(item.body ?? "");
  const tags = useSignal(item.tags.join(", "));
  const source = useSignal(item.source ?? "");
  const error = useSignal("");
  const save = async (e: Event) => {
    e.preventDefault();
    error.value = "";
    try {
      const r = await api.putLibrary(item.kind, item.scope, item.name, {
        body: body.value,
        tags: tags.value.split(",").map((t) => t.trim()).filter(Boolean),
        source: source.value,
        version: item.version,
      });
      onSaved(r.item);
    } catch (err) {
      error.value = message(err);
    }
  };
  return (
    <form class="card-form" onSubmit={save}>
      <textarea class="tall source-edit" aria-label="The file" value={body.value} onInput={(e) => (body.value = e.currentTarget.value)} />
      <div class="row">
        <label>
          Tags
          <input value={tags.value} onInput={(e) => (tags.value = e.currentTarget.value)} />
        </label>
        <label>
          Where it came from
          <input value={source.value} onInput={(e) => (source.value = e.currentTarget.value)} />
        </label>
      </div>
      {error.value && <p class="field-error">{error.value}</p>}
      <div class="row">
        <button type="submit" class="primary">
          Save
        </button>
        <button type="button" class="quiet" onClick={onDelete}>
          Delete
        </button>
      </div>
    </form>
  );
}

/** On a client's Linked work page: the library items it uses. */
export function ClientLibrary({ slug }: { slug: string }) {
  const items = useSignal<LibraryItem[] | null>(null);
  const load = () =>
    api.clientLibrary(slug).then(
      (r) => (items.value = r.items),
      () => (items.value = []),
    );
  useEffect(() => void load(), [slug]);
  if (!items.value) return null;
  return (
    <section class="group">
      <h3>Library it uses</h3>
      {items.value.length === 0 ? (
        <p class="empty">
          No styles pinned. Open one in the <Link href="/library">library</Link> and tick this client.
        </p>
      ) : (
        <ul class="trash-list">
          {items.value.map((i) => (
            <li key={i.id}>
              <span>
                <Link href={libraryUrl(i.kind, i.scope, i.name)}>{i.title}</Link>{" "}
                <span class="meta">{i.scope === "_studio" ? kindName(i.kind) : "its own"}</span>
              </span>
              <span class="meta">{i.summary}</span>
              {i.scope === "_studio" ? (
                <button type="button" class="quiet" onClick={() => api.unpin(slug, i.id).then(load)}>
                  Unpin
                </button>
              ) : (
                <span />
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

