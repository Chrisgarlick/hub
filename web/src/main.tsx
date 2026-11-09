// Hub's UI: spaces, their documents, and an editor with history and trash.

import { render } from "preact";
import { useSignal, useSignalEffect } from "@preact/signals";
import { useEffect, useRef } from "preact/hooks";

import { ApiError, api, type Doc, type Listed, type Passkey, type Revision, type Space, type Summary } from "./api.ts";
import { escapeHtml, renderMarkdown } from "./markdown.ts";
import { addPasskey, deviceName, explain, passkeysSupported, signInWithPasskey } from "./passkeys.ts";
import { docUrl, navigate, path, route, spaceUrl } from "./router.ts";
import { ago, full } from "./time.ts";
import { Link, dirty, go, message } from "./ui.tsx";
import { DecisionsPage, RulesPage } from "./rules.tsx";
import { SearchBox, SearchPage } from "./search.tsx";
import { ClientLibrary, LibraryItemPage, LibraryPage } from "./library.tsx";
import { ApprovalsPage, SettingsPage, WorkPage, pendingCount, watchPending } from "./approvals.tsx";

// ---------- Spaces ----------

function SpacesPage() {
  const spaces = useSignal<Listed[] | null>(null);
  const error = useSignal("");
  const note = useSignal("");
  const syncing = useSignal(false);
  const noPasskeys = useSignal(false);
  useEffect(() => {
    api.passkeys().then(
      (r) => (noPasskeys.value = r.passkeys.length === 0 && passkeysSupported()),
      () => {},
    );
  }, []);

  const load = () =>
    api.spaces().then(
      (r) => (spaces.value = r.spaces),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), []);

  const sync = async () => {
    syncing.value = true;
    note.value = "";
    try {
      const r = await api.sync();
      note.value = r.created
        ? `Added ${r.created} ${r.created === 1 ? "space" : "spaces"} from Clients.`
        : "Spaces match Clients.";
      await load();
    } catch (e) {
      note.value = message(e);
    } finally {
      syncing.value = false;
    }
  };

  if (error.value) return <p class="page-note is-bad">{error.value}</p>;
  if (!spaces.value) return <p class="page-note">Loading…</p>;
  const studio = spaces.value.filter((s) => s.kind === "studio");
  const clients = spaces.value.filter((s) => s.kind === "client");
  return (
    <main class="spaces">
      {noPasskeys.value && (
        <p class="nudge">
          Sign in with your phone or this computer next time, without going through Clients.{" "}
          <Link href="/passkeys">Add a passkey</Link>
        </p>
      )}
      <h1>Spaces</h1>
      <p class="lede">What you know about each client. A space appears here when you add the client in Clients.</p>
      <SearchBox label="Search every client" />
      <ul class="space-list">
        {studio.map((s) => (
          <SpaceRow key={s.id} s={s} hint="Rules and house style for every client" />
        ))}
      </ul>
      <h2>Clients</h2>
      {clients.length ? (
        <ul class="space-list">
          {clients.map((s) => (
            <SpaceRow key={s.id} s={s} />
          ))}
        </ul>
      ) : (
        <p class="empty">No clients yet. Add one in Clients and it will appear here within a minute.</p>
      )}
      <p class="sync">
        <button type="button" class="quiet" disabled={syncing.value} onClick={sync}>
          {syncing.value ? "Checking…" : "Check Clients now"}
        </button>
        {note.value && <span role="status">{note.value}</span>}
      </p>
    </main>
  );
}

function SpaceRow({ s, hint }: { s: Listed; hint?: string }) {
  return (
    <li>
      <Link href={spaceUrl(s.slug)} class="space-row">
        <span class="space-name">{s.name}</span>
        <span class="space-meta">
          {hint ?? s.slug}, {s.documents} {s.documents === 1 ? "document" : "documents"}
        </span>
      </Link>
    </li>
  );
}

// ---------- One space ----------

/** Documents grouped by folder, root first. */
function folders(docs: Summary[]): [string, Summary[]][] {
  const by = new Map<string, Summary[]>();
  for (const d of [...docs].sort((a, b) => a.path.localeCompare(b.path))) {
    const i = d.path.lastIndexOf("/");
    const dir = i < 0 ? "" : d.path.slice(0, i);
    by.set(dir, [...(by.get(dir) ?? []), d]);
  }
  return [...by.entries()].sort(([a], [b]) => (a === "" ? -1 : b === "" ? 1 : a.localeCompare(b)));
}

function SpacePage({ slug, doc, view }: { slug: string; doc?: string; view?: "trash" | "rules" | "decisions" | "work" }) {
  const trash = view === "trash";
  const space = useSignal<Space | null>(null);
  const docs = useSignal<Summary[]>([]);
  const error = useSignal("");
  const adding = useSignal(false);
  const listOpen = useSignal(false);
  useEffect(() => {
    listOpen.value = false;
  }, [doc, view]);

  const load = () =>
    api.space(slug).then(
      (r) => {
        space.value = r.space;
        docs.value = r.documents;
        error.value = "";
      },
      (e) => (error.value = e instanceof ApiError && e.status === 404 ? "There's no space here." : message(e)),
    );
  useEffect(() => void load(), [slug]);

  if (error.value)
    return (
      <main class="page-note">
        <p class="is-bad">{error.value}</p>
        <Link href="/">All spaces</Link>
      </main>
    );
  if (!space.value) return <p class="page-note">Loading…</p>;
  const s = space.value;

  return (
    <div class={doc || view ? "space has-main" : "space"}>
      <nav class={listOpen.value ? "side is-open" : "side"} aria-label={`${s.name} documents`}>
        <Link href="/" class="back">
          All spaces
        </Link>
        <div class="side-head">
          <h1 class="side-title">{s.name}</h1>
          <button
            type="button"
            class="quiet list-toggle"
            aria-expanded={listOpen.value}
            onClick={() => (listOpen.value = !listOpen.value)}
          >
            {listOpen.value ? "Hide documents" : `Documents (${docs.value.length})`}
          </button>
        </div>
        <SearchBox space={slug} label={`Search ${s.name}`} />
        <div class="space-links">
          <Link href={`${spaceUrl(slug)}/rules`} class={view === "rules" ? "doc-link is-current" : "doc-link"}>
            Rules
          </Link>
          <Link href={`${spaceUrl(slug)}/decisions`} class={view === "decisions" ? "doc-link is-current" : "doc-link"}>
            Decisions
          </Link>
          <Link href={`${spaceUrl(slug)}/work`} class={view === "work" ? "doc-link is-current" : "doc-link"}>
            Linked work
          </Link>
        </div>
        {folders(docs.value).map(([dir, list]) => (
          <div class="folder" key={dir}>
            {dir && <h2 class="folder-name">{dir}</h2>}
            <ul>
              {list.map((d) => (
                <li key={d.id}>
                  <Link href={docUrl(slug, d.id)} class={d.id === doc ? "doc-link is-current" : "doc-link"}>
                    {d.title}
                    {d.visibility !== "internal" && <span class={`vis vis-${d.visibility}`}>{d.visibility}</span>}
                  </Link>
                </li>
              ))}
            </ul>
          </div>
        ))}
        {adding.value ? (
          <NewDocument
            slug={slug}
            onDone={(d) => {
              adding.value = false;
              if (d) {
                void load();
                go(docUrl(slug, d.id));
              }
            }}
          />
        ) : (
          <button type="button" class="quiet new-doc" onClick={() => (adding.value = true)}>
            New document
          </button>
        )}
        <Link href={`${spaceUrl(slug)}/trash`} class={trash ? "doc-link trash-link is-current" : "doc-link trash-link"}>
          Trash
        </Link>
      </nav>
      <section class="main">
        {trash ? (
          <Trash slug={slug} onChange={load} />
        ) : view === "rules" ? (
          <RulesPage slug={slug} studio={s.kind === "studio"} />
        ) : view === "decisions" ? (
          <DecisionsPage slug={slug} docs={docs.value} />
        ) : view === "work" ? (
          <div>
            <WorkPage slug={slug} />
            <div class="page">
              <ClientLibrary slug={slug} />
            </div>
          </div>
        ) : doc ? (
          <Editor key={doc} id={doc} slug={slug} onChange={load} />
        ) : (
          <p class="page-note">Choose a document, or start a new one.</p>
        )}
      </section>
    </div>
  );
}

function NewDocument({ slug, onDone }: { slug: string; onDone: (d: Doc | null) => void }) {
  const value = useSignal("");
  const error = useSignal("");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => ref.current?.focus(), []);
  const submit = async (e: Event) => {
    e.preventDefault();
    let p = value.value.trim().toLowerCase().replace(/\s+/g, "-");
    if (!p) return onDone(null);
    if (!p.endsWith(".md")) p += ".md";
    try {
      onDone(await api.create(slug, p));
    } catch (err) {
      error.value = message(err);
    }
  };
  return (
    <form class="new-form" onSubmit={submit}>
      <label for="new-path">Path</label>
      <input
        id="new-path"
        ref={ref}
        value={value.value}
        placeholder="research/interviews"
        onInput={(e) => (value.value = e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Escape" && onDone(null)}
      />
      {error.value && <p class="field-error">{error.value}</p>}
      <div class="row">
        <button type="submit">Create</button>
        <button type="button" class="quiet" onClick={() => onDone(null)}>
          Cancel
        </button>
      </div>
    </form>
  );
}

// ---------- The editor ----------

type Mode = "write" | "both" | "read";

function Editor({ id, slug, onChange }: { id: string; slug: string; onChange: () => void }) {
  const doc = useSignal<Doc | null>(null);
  const text = useSignal("");
  const error = useSignal("");
  const status = useSignal("");
  const saving = useSignal(false);
  const conflict = useSignal<Doc | null>(null);
  const mode = useSignal<Mode>(matchMedia("(min-width: 70rem)").matches ? "both" : matchMedia("(max-width: 48rem)").matches ? "read" : "write");
  const history = useSignal(false);
  const renaming = useSignal(false);

  useEffect(() => {
    api.get(id).then(
      (d) => {
        doc.value = d;
        text.value = d.content;
        // A link to a section (from a decision, say): show it.
        const hash = decodeURIComponent(location.hash.slice(1));
        if (hash) {
          mode.value = mode.value === "write" ? "read" : mode.value;
          requestAnimationFrame(() => document.getElementById(hash)?.scrollIntoView({ block: "start" }));
        }
      },
      (e) =>
        (error.value =
          e instanceof ApiError && e.status === 404 ? "This document isn't here. It may be in the trash." : message(e)),
    );
    return () => {
      dirty.value = false;
    };
  }, [id]);

  useSignalEffect(() => {
    dirty.value = doc.value !== null && text.value !== doc.value.content;
  });

  const loaded = (d: Doc, note: string) => {
    doc.value = d;
    text.value = d.content;
    conflict.value = null;
    status.value = note;
    onChange();
  };

  const save = async (version?: number) => {
    const d = doc.value;
    if (!d || saving.value) return;
    saving.value = true;
    status.value = "";
    const sent = text.value;
    try {
      const saved = await api.save(d.id, version ?? d.version, { content: sent });
      const typed = text.value;
      loaded(saved, "");
      // Anything typed while saving stays.
      text.value = typed;
    } catch (e) {
      if (e instanceof ApiError && e.code === "conflict" && e.current) conflict.value = e.current;
      else status.value = message(e);
    } finally {
      saving.value = false;
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      void save();
    }
  };

  const trash = async () => {
    const d = doc.value;
    if (!d) return;
    if (dirty.value && !confirm("Move to trash without saving your changes?")) return;
    try {
      await api.remove(d.id);
      dirty.value = false;
      onChange();
      navigate(spaceUrl(slug));
    } catch (e) {
      status.value = message(e);
    }
  };

  if (error.value) return <p class="page-note is-bad">{error.value}</p>;
  const d = doc.value;
  if (!d) return <p class="page-note">Loading…</p>;

  return (
    <article class={`editor mode-${mode.value}`} onKeyDown={onKey}>
      <header class="editor-bar">
        <div class="editor-title">
          {renaming.value ? (
            <Rename
              doc={d}
              onDone={(n) => {
                renaming.value = false;
                if (n) loaded(n, "");
              }}
            />
          ) : (
            <button type="button" class="path" title="Rename or move" onClick={() => (renaming.value = true)}>
              {d.path}
            </button>
          )}
          <span class="meta" title={full(d.updated_at)}>
            {d.type}, {d.visibility}, edited {ago(d.updated_at)} by {d.updated_by}
          </span>
        </div>
        <div class="tools">
          <div class="modes" role="group" aria-label="View">
            {(["write", "both", "read"] as Mode[]).map((m) => (
              <button type="button" key={m} aria-pressed={mode.value === m} onClick={() => (mode.value = m)}>
                {m === "write" ? "Write" : m === "both" ? "Side by side" : "Preview"}
              </button>
            ))}
          </div>
          <button
            type="button"
            class="quiet"
            aria-pressed={history.value}
            onClick={() => (history.value = !history.value)}
          >
            History
          </button>
          <button type="button" class="quiet" onClick={trash}>
            Move to trash
          </button>
          <button type="button" class="primary" disabled={!dirty.value || saving.value} onClick={() => save()}>
            {saving.value ? "Saving…" : dirty.value ? "Save" : "Saved"}
          </button>
        </div>
      </header>
      {status.value && (
        <p class="banner" role="status">
          {status.value}
        </p>
      )}
      {conflict.value && (
        <div class="banner is-warn" role="alert">
          <p>
            {conflict.value.updated_by} changed this document {ago(conflict.value.updated_at)}, after you opened it.
          </p>
          <div class="row">
            <button type="button" onClick={() => conflict.value && loaded(conflict.value, "")}>
              Discard mine and load theirs
            </button>
            <button type="button" class="quiet" onClick={() => conflict.value && save(conflict.value.version)}>
              Save mine over theirs
            </button>
          </div>
        </div>
      )}
      <div class={history.value ? "panes with-history" : "panes"}>
        {mode.value !== "read" && (
          <textarea
            class="source"
            aria-label="Markdown"
            spellcheck
            value={text.value}
            onInput={(e) => (text.value = e.currentTarget.value)}
          />
        )}
        {mode.value !== "write" && (
          <div
            class="prose"
            dangerouslySetInnerHTML={{ __html: renderMarkdown(stripFrontmatter(text.value), overrideNotes(d, slug)) }}
          />
        )}
        {history.value && (
          <History
            doc={d}
            onRestore={(n) => {
              history.value = false;
              loaded(n, "Restored. The version you replaced is still in History.");
            }}
          />
        )}
      </div>
    </article>
  );
}

/** A note under each section an approved decision overrides. */
function overrideNotes(d: Doc, slug: string): Map<string, string> {
  const notes = new Map<string, string>();
  for (const o of d.overridden_by ?? []) {
    const key = o.anchor ?? "";
    const note =
      `<aside class="override"><strong>Overridden by a decision:</strong> ${escapeHtml(o.decision.statement)} ` +
      `<a href="${spaceUrl(slug)}/decisions#${escapeHtml(o.decision.id)}">Why</a></aside>`;
    notes.set(key, (notes.get(key) ?? "") + note);
  }
  return notes;
}

function stripFrontmatter(s: string): string {
  return s.replace(/^---\n[\s\S]*?\n---\n?/, "");
}

function Rename({ doc, onDone }: { doc: Doc; onDone: (d: Doc | null) => void }) {
  const value = useSignal(doc.path);
  const error = useSignal("");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => ref.current?.focus(), []);
  const submit = async (e: Event) => {
    e.preventDefault();
    if (value.value === doc.path) return onDone(null);
    try {
      onDone(await api.save(doc.id, doc.version, { path: value.value }));
    } catch (err) {
      error.value = message(err);
    }
  };
  return (
    <form class="rename" onSubmit={submit}>
      <input
        aria-label="Path"
        ref={ref}
        value={value.value}
        onInput={(e) => (value.value = e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Escape" && onDone(null)}
      />
      <button type="submit">Rename</button>
      {error.value && <span class="field-error">{error.value}</span>}
    </form>
  );
}

function History({ doc, onRestore }: { doc: Doc; onRestore: (d: Doc) => void }) {
  const revs = useSignal<Revision[] | null>(null);
  const open = useSignal<number | null>(null);
  const error = useSignal("");
  useEffect(() => {
    api.revisions(doc.id).then(
      (r) => (revs.value = r.revisions),
      (e) => (error.value = message(e)),
    );
  }, [doc.id, doc.version]);

  const restore = async (r: Revision) => {
    if (dirty.value && !confirm("Restoring replaces your unsaved changes. Go ahead?")) return;
    try {
      onRestore(await api.restore(doc.id, r.id, doc.version));
    } catch (e) {
      error.value = message(e);
    }
  };

  return (
    <aside class="history" aria-label="History">
      <h2>History</h2>
      {error.value && <p class="field-error">{error.value}</p>}
      {!revs.value ? (
        <p class="meta">Loading…</p>
      ) : (
        <ol>
          {revs.value.map((r, i) => (
            <li key={r.id} class={open.value === r.id ? "is-open" : ""}>
              <button type="button" class="rev" onClick={() => (open.value = open.value === r.id ? null : r.id)}>
                <span>{i === 0 ? "Current" : `Version ${r.version}`}</span>
                <span class="meta" title={full(r.updated_at)}>
                  {r.changed_by}, {ago(r.updated_at)}
                </span>
              </button>
              {open.value === r.id && (
                <div class="rev-body">
                  <div
                    class="prose small"
                    dangerouslySetInnerHTML={{ __html: renderMarkdown(stripFrontmatter(r.content)) }}
                  />
                  {i > 0 && (
                    <button type="button" onClick={() => restore(r)}>
                      Restore this version
                    </button>
                  )}
                </div>
              )}
            </li>
          ))}
        </ol>
      )}
    </aside>
  );
}

// ---------- Trash ----------

function Trash({ slug, onChange }: { slug: string; onChange: () => void }) {
  const docs = useSignal<Summary[] | null>(null);
  const error = useSignal("");
  useEffect(() => {
    api.trash(slug).then(
      (r) => (docs.value = r.documents),
      (e) => (error.value = message(e)),
    );
  }, [slug]);

  const undelete = async (d: Summary) => {
    error.value = "";
    try {
      const back = await api.undelete(d.id);
      onChange();
      navigate(docUrl(slug, back.id));
    } catch (e) {
      error.value = message(e);
    }
  };

  return (
    <div class="trash">
      <h2>Trash</h2>
      <p class="meta">Documents you've moved to the trash. Putting one back keeps its history.</p>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {!docs.value ? (
        <p class="meta">Loading…</p>
      ) : docs.value.length === 0 ? (
        <p class="empty">The trash is empty.</p>
      ) : (
        <ul class="trash-list">
          {docs.value.map((d) => (
            <li key={d.id}>
              <span>
                {d.title} <span class="meta">{d.path}</span>
              </span>
              <span class="meta">{d.deleted_at && `trashed ${ago(d.deleted_at)}`}</span>
              <button type="button" class="quiet" onClick={() => undelete(d)}>
                Put back
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ---------- Signing in ----------

function returnPath(): string {
  const r = new URLSearchParams(location.search).get("return") ?? "/";
  return r.startsWith("/") && !r.startsWith("//") && !r.startsWith("/login") ? r : "/";
}

function LoginPage() {
  const busy = useSignal(false);
  const error = useSignal("");
  const supported = passkeysSupported();
  const ret = returnPath();

  const passkey = async () => {
    busy.value = true;
    error.value = "";
    try {
      await signInWithPasskey();
      location.href = ret;
    } catch (e) {
      error.value = explain(e);
      busy.value = false;
    }
  };

  return (
    <main class="login">
      <h1>Hub</h1>
      <p class="lede">What you know about each client, and where every job starts.</p>
      {supported ? (
        <button type="button" class="primary big" disabled={busy.value} onClick={passkey}>
          {busy.value ? "Waiting for your passkey…" : "Sign in with a passkey"}
        </button>
      ) : (
        <p class="meta">This browser can't use passkeys.</p>
      )}
      {error.value && (
        <p class="field-error" role="alert">
          {error.value}
        </p>
      )}
      <p class="alt">
        No passkey on this device yet? <a href={`/auth/clients?return=${encodeURIComponent(ret)}`}>Sign in with Clients</a>,
        then add one.
      </p>
    </main>
  );
}

function PasskeysPage() {
  const list = useSignal<Passkey[] | null>(null);
  const error = useSignal("");
  const name = useSignal(deviceName());
  const busy = useSignal(false);
  const note = useSignal("");

  const load = () =>
    api.passkeys().then(
      (r) => (list.value = r.passkeys),
      (e) => (error.value = message(e)),
    );
  useEffect(() => void load(), []);

  const add = async (e: Event) => {
    e.preventDefault();
    busy.value = true;
    error.value = "";
    note.value = "";
    try {
      await addPasskey(name.value.trim() || deviceName());
      note.value = "Passkey added. Next time, sign in with it.";
      await load();
    } catch (err) {
      error.value = explain(err);
    } finally {
      busy.value = false;
    }
  };

  const remove = async (p: Passkey) => {
    if (!confirm(`Remove the passkey "${p.name}"? It won't sign you in to Hub any more.`)) return;
    try {
      await api.removePasskey(p.id);
      await load();
    } catch (err) {
      error.value = message(err);
    }
  };

  return (
    <main class="spaces">
      <Link href="/" class="back">
        All spaces
      </Link>
      <h1>Passkeys</h1>
      <p class="lede">
        Sign in to Hub with Face ID, Touch ID or your phone. A passkey made on your iPhone syncs to your other Apple
        devices. To use your phone from a computer, add it here and choose "use a phone" when the browser asks.
      </p>
      {!list.value ? (
        <p class="meta">Loading…</p>
      ) : list.value.length === 0 ? (
        <p class="empty">No passkeys yet.</p>
      ) : (
        <ul class="trash-list">
          {list.value.map((p) => (
            <li key={p.id}>
              <span>{p.name}</span>
              <span class="meta" title={full(p.created_at)}>
                {p.last_used_at ? `last used ${ago(p.last_used_at)}` : `added ${ago(p.created_at)}`}
              </span>
              <button type="button" class="quiet" onClick={() => remove(p)}>
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      {passkeysSupported() ? (
        <form class="add-passkey" onSubmit={add}>
          <label for="pk-name">Name</label>
          <div class="row">
            <input
              id="pk-name"
              value={name.value}
              maxLength={60}
              onInput={(e) => (name.value = e.currentTarget.value)}
            />
            <button type="submit" class="primary" disabled={busy.value}>
              {busy.value ? "Waiting…" : "Add a passkey"}
            </button>
          </div>
        </form>
      ) : (
        <p class="meta">This browser can't make passkeys.</p>
      )}
      {note.value && <p role="status">{note.value}</p>}
      {error.value && (
        <p class="field-error" role="alert">
          {error.value}
        </p>
      )}
    </main>
  );
}

// ---------- App ----------

function App() {
  const r = route(path.value);
  if (r.page === "login") return <LoginPage />;
  return (
    <>
      <header class="top">
        <Link href="/" class="brand">
          Hub
        </Link>
        <div class="row">
          <Link href="/library" class="top-link">
            Library
          </Link>
          <Link href="/approvals" class="top-link">
            Approvals{pendingCount.value > 0 && <span class="count">{pendingCount.value}</span>}
          </Link>
          <Link href="/passkeys" class="top-link">
            Passkeys
          </Link>
          <button
            type="button"
            class="quiet"
            onClick={async () => {
              if (dirty.value && !confirm("Sign out without saving?")) return;
              await api.logout();
              location.href = "/login";
            }}
          >
            Sign out
          </button>
        </div>
      </header>
      {r.page === "spaces" ? (
        <SpacesPage />
      ) : r.page === "passkeys" ? (
        <PasskeysPage />
      ) : r.page === "search" ? (
        <SearchPage />
      ) : r.page === "approvals" ? (
        <ApprovalsPage />
      ) : r.page === "library" ? (
        <LibraryPage />
      ) : r.page === "libraryItem" ? (
        <LibraryItemPage kind={r.kind} scope={r.scope} name={r.name} />
      ) : r.page === "settings" ? (
        <SettingsPage />
      ) : r.page === "space" ? (
        <SpacePage slug={r.slug} {...(r.doc ? { doc: r.doc } : {})} {...(r.view ? { view: r.view } : {})} />
      ) : (
        <main class="page-note">
          <p>There's nothing here.</p>
          <Link href="/">All spaces</Link>
        </main>
      )}
    </>
  );
}

const root = document.getElementById("app");
if (root) {
  render(<App />, root);
  if (route(path.value).page !== "login") watchPending();
}
