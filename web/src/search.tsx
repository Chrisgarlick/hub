// Search: across everything, or one space (and the studio).

import { useSignal } from "@preact/signals";
import { useEffect } from "preact/hooks";

import { api, type IndexStatus, type SearchResponse, type SearchResult, type Standing } from "./api.ts";
import { navigate, param, spaceUrl } from "./router.ts";
import { Link, message } from "./ui.tsx";

const STANDING: Record<Standing, string> = {
  authoritative: "Authoritative",
  tentative: "Not reviewed",
  outdated: "Outdated",
};

/** A search box; Enter goes to the results page. */
export function SearchBox({ space, label }: { space?: string; label: string }) {
  const q = useSignal("");
  return (
    <form
      class="search-box"
      role="search"
      onSubmit={(e) => {
        e.preventDefault();
        if (!q.value.trim()) return;
        navigate(`/search?q=${encodeURIComponent(q.value.trim())}${space ? `&space=${space}` : ""}`);
      }}
    >
      <input
        type="search"
        aria-label={label}
        placeholder={label}
        value={q.value}
        onInput={(e) => (q.value = e.currentTarget.value)}
      />
    </form>
  );
}

function where(r: SearchResult): string {
  if (r.source === "document") return `${spaceUrl(r.space.slug)}/d/${r.document?.id}${r.anchor ? `#${r.anchor}` : ""}`;
  if (r.source === "rule") return `${spaceUrl(r.space.slug)}/rules`;
  return `${spaceUrl(r.space.slug)}/decisions#${r.decision_id}`;
}

/** The text, cut to a snippet around the first word of the query found. */
function snippet(text: string, q: string): preact.ComponentChildren {
  const words = q
    .toLowerCase()
    .split(/[^\p{L}\p{N}]+/u)
    .filter((w) => w.length > 2);
  const lower = text.toLowerCase();
  const first = words.map((w) => lower.indexOf(w)).filter((i) => i >= 0).sort((a, b) => a - b)[0] ?? 0;
  const start = Math.max(0, first - 120);
  let cut = text.slice(start, start + 420);
  if (start > 0) cut = `…${cut.replace(/^\S*\s/, "")}`;
  if (start + 420 < text.length) cut = `${cut.replace(/\s\S*$/, "")}…`;
  if (!words.length) return cut;
  const re = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "gi");
  return cut.split(re).map((part, i) => (i % 2 ? <mark key={i}>{part}</mark> : part));
}

export function SearchPage() {
  const q = param("q");
  const space = param("space");
  const res = useSignal<SearchResponse | null>(null);
  const status = useSignal<IndexStatus | null>(null);
  const error = useSignal("");
  const input = useSignal(q);

  useEffect(() => {
    input.value = q;
    res.value = null;
    error.value = "";
    if (!q.trim()) return;
    api.search(q, space || undefined).then(
      (r) => (res.value = r),
      (e) => (error.value = message(e)),
    );
    api.indexStatus().then(
      (s) => (status.value = s),
      () => {},
    );
  }, [q, space]);

  const r = res.value;
  const s = status.value;
  return (
    <main class="search-page">
      {space && (
        <Link href={spaceUrl(space)} class="back">
          Back to the space
        </Link>
      )}
      <form
        class="search-box big"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          navigate(`/search?q=${encodeURIComponent(input.value.trim())}${space ? `&space=${space}` : ""}`, true);
        }}
      >
        <input
          type="search"
          aria-label="Search"
          value={input.value}
          placeholder={space ? "Search this client and the studio" : "Search every client"}
          onInput={(e) => (input.value = e.currentTarget.value)}
        />
        <button type="submit" class="primary">
          Search
        </button>
      </form>
      {error.value && <p class="banner is-warn">{error.value}</p>}
      {q && !r && !error.value && <p class="meta">Searching…</p>}
      {r && (
        <>
          <p class="meta">
            {r.results.length ? `${r.results.length} ${r.results.length === 1 ? "result" : "results"}` : "Nothing found."}
            {space ? " in this client and the studio" : " across every client"}
            {r.meaning ? ", by meaning and words." : "."}
          </p>
          {!r.meaning && (
            <p class="note">
              {r.note ??
                (r.embeddings
                  ? "Searched by words only."
                  : "Searched by words only. Add a Voyage key to Hub's config to search by meaning too.")}
            </p>
          )}
          {s && s.queued > 0 && <p class="note">Still indexing {s.queued} recent changes; try again in a moment.</p>}
          <ol class="results">
            {r.results.map((x) => (
              <li key={x.id} class={`result is-${x.standing}`}>
                <div class="result-head">
                  <Link href={where(x)} class="result-title">
                    {x.source === "document" ? x.heading_path : x.source === "rule" ? "Rule" : "Decision"}
                  </Link>
                  {x.standing !== "authoritative" && <span class={`badge is-${x.standing}`}>{STANDING[x.standing]}</span>}
                </div>
                <p class="result-text">{snippet(x.text, q)}</p>
                <p class="meta">
                  {!space && `${x.space.name}, `}
                  {x.source === "document" ? x.document?.path : x.heading_path}
                  {x.space.slug === "_studio" && space && space !== "_studio" && ", from the studio"}
                </p>
                {x.overridden_by?.map((d) => (
                  <p key={d.id} class="overridden">
                    <strong>Overridden by a decision:</strong> {d.statement}{" "}
                    <Link href={`${spaceUrl(x.space.slug)}/decisions#${d.id}`}>Why</Link>
                  </p>
                ))}
              </li>
            ))}
          </ol>
        </>
      )}
    </main>
  );
}

