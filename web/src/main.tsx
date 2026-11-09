// Hub's UI. Phase 0: the app shell and a status line; spaces and documents
// arrive in phase 1.

import { render } from "preact";
import { signal } from "@preact/signals";

interface Ready {
  database: boolean;
  vector: boolean;
  clients: boolean | null;
}

const ready = signal<Ready | null>(null);
const failed = signal(false);

async function check(): Promise<void> {
  try {
    const res = await fetch("/readyz");
    ready.value = (await res.json()) as Ready;
  } catch {
    failed.value = true;
  }
}

function Item({ label, ok }: { label: string; ok: boolean | null }) {
  const text = ok === null ? "not set up" : ok ? "ok" : "not reachable";
  return (
    <li class={ok === false ? "is-bad" : ""}>
      <span>{label}</span>
      <span>{text}</span>
    </li>
  );
}

function App() {
  const r = ready.value;
  return (
    <main class="shell">
      <h1>Hub</h1>
      <p class="lede">What you know about each client, and where every job starts.</p>
      {failed.value ? (
        <p class="is-bad">Can't reach Hub's server.</p>
      ) : r ? (
        <ul class="status" aria-label="Status">
          <Item label="Database" ok={r.database} />
          <Item label="pgvector" ok={r.vector} />
          <Item label="Clients" ok={r.clients} />
        </ul>
      ) : (
        <p>Checking…</p>
      )}
    </main>
  );
}

const root = document.getElementById("app");
if (root) {
  render(<App />, root);
  void check();
}
