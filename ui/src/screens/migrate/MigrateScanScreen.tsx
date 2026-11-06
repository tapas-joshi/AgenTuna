import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { MigrateScanView } from "../../generated/MigrateScanView";
import { useAppSnapshot } from "../../state/context";

/** First migration wizard step; scans native definitions without executing servers. */
export function MigrateScanScreen() {
  const { activeProject } = useAppSnapshot();
  const [report, setReport] = useState<MigrateScanView | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [staleSelectionCount, setStaleSelectionCount] = useState(0);
  async function scan() {
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<MigrateScanView>("migrate_scan", { projectId: activeProject });
      const keys = new Set(next.items.map((item) => `${item.kind}:${item.path}:${item.content_hash ?? ""}`));
      const retained = new Set([...selected].filter((key) => keys.has(key)));
      setStaleSelectionCount(selected.size - retained.size);
      setSelected(retained);
      setReport(next);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }
  const items = report?.items ?? [];
  return <main aria-label="Migrate scan" className="mx-auto flex max-w-[1100px] flex-col gap-16 p-24">
    <h1 className="m-0 text-22 font-semibold text-text">Move into AgenTuna</h1>
    <p>Scan agent folders for skills, MCP servers and rules. Scanning does not run servers or change files.</p>
    <button onClick={() => void scan()} disabled={busy}>{busy ? "Scanning…" : "Rescan"}</button>
    {busy && <p role="status">Scanning agent folders…</p>}
    {error && <div role="alert"><p>{error}</p><button onClick={() => void scan()}>Retry</button></div>}
    {report?.partial && <p role="status">Some agent folders could not be read; found items are still available.</p>}
    {report && report.skipped_roots.length > 0 && <section aria-label="Missing agent folders"><h2 className="text-13">Missing candidate folders</h2><ul>{report.skipped_roots.map((path) => <li key={path} className="font-mono text-11">{path} · 0 items</li>)}</ul></section>}
    {report && items.length === 0 && <p>Nothing to migrate. No native items found.</p>}
    {staleSelectionCount > 0 && <p role="status">{staleSelectionCount} selection{staleSelectionCount === 1 ? "" : "s"} changed or disappeared and were cleared.</p>}
    {items.length > 0 && <><p>{items.length} found · {selected.size} selected</p><ul className="m-0 list-none p-0">{items.map((item) => {
      const key = `${item.kind}:${item.path}:${item.content_hash ?? ""}`;
      return <li key={key} className="border-b border-border p-12"><label className="flex gap-8"><input type="checkbox" checked={selected.has(key)} onChange={(event) => setSelected((current) => { const next = new Set(current); if (event.target.checked) next.add(key); else next.delete(key); return next; })} /><span><strong>{item.name}</strong><span className="ml-8 text-text-secondary">{item.kind} · {item.scope} · {item.ownership}</span><span className="block font-mono text-11 text-text-muted">{item.agents.join(", ")} — {item.path}</span>{item.store_link && <span className="text-text-secondary">Symlink</span>}{item.parse_error && <span role="alert">{item.parse_error}</span>}</span></label></li>;
    })}</ul><button disabled={selected.size === 0}>Next</button></>}
  </main>;
}
