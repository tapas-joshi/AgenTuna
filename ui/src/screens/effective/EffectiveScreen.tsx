import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAppSnapshot } from "../../state/context";
import { useToast } from "../../overlays/toasts";
import { WhyTraceDialog } from "../../overlays/dialogs/WhyTraceDialog";
import type { EffectiveQueryView } from "../../generated/EffectiveQueryView";

/** Effective configuration view; query data is supplied by the typed TA-006 command. */
export function EffectiveScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const toast = useToast();
  const [tab, setTab] = useState("Skills");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [why, setWhy] = useState<{ itemId: string; agent: string } | null>(null);
  const [rows, setRows] = useState<EffectiveQueryView["items"]>([]);
  const [disabledHere, setDisabledHere] = useState<EffectiveQueryView["disabled_here"]>([]);
  const [diagnostics, setDiagnostics] = useState<EffectiveQueryView["diagnostics"]>([]);
  const [loadDiagnostics, setLoadDiagnostics] = useState<EffectiveQueryView["load_diagnostics"]>([]);
  const [globalWritable, setGlobalWritable] = useState(false);
  const [globalReason, setGlobalReason] = useState("Global store is read-only");
  const [lockTarget, setLockTarget] = useState<{ itemId: string; name: string; locked: boolean; reason: string } | null>(null);
  const [lockReason, setLockReason] = useState("");
  async function refresh() {
    setLoading(true); setError(null);
    try {
      const kind = tab === "Skills" ? "skill" : tab === "MCP servers" ? "mcp_server" : "rule";
      const result = await invoke<EffectiveQueryView>("effective_query", { request: { scope, kind, agent: null } });
      const overview = await invoke<{ global_writable: boolean; global_reason: string | null }>("overview_load", { activeScope: scope });
      setGlobalWritable(overview.global_writable);
      setGlobalReason(overview.global_reason ?? "Global store is read-only");
      setRows(result.items);
      setDisabledHere(result.disabled_here);
      setDiagnostics(result.diagnostics);
      setLoadDiagnostics(result.load_diagnostics);
    } catch (cause) { setError(String(cause)); }
    finally { setLoading(false); }
  }
  useEffect(() => { void refresh(); }, [scope, activeProject, tab]);
  const restoreFocus = useRef<HTMLButtonElement | null>(null);
  const [lockBusy, setLockBusy] = useState(false);
  function closeLockDialog() {
    setLockTarget(null);
    queueMicrotask(() => restoreFocus.current?.focus());
  }
  async function applyLock() {
    if (!lockTarget) return;
    const reason = lockReason.trim();
    if (!lockTarget.locked && (reason.length === 0 || reason.length > 200)) return;
    setLockBusy(true);
    try {
      if (lockTarget.locked) {
        await invoke("effective_unlock", { request: { item_id: lockTarget.itemId, scope } });
        toast.success(`Unlocked ${lockTarget.name}`);
      } else {
        await invoke("effective_lock", { request: { item_id: lockTarget.itemId, scope, reason } });
        toast.success(`Locked ${lockTarget.name}`);
      }
      closeLockDialog();
      await refresh();
    } catch (cause) {
      toast.warning(String(cause), { title: "Lock change failed" });
    } finally {
      setLockBusy(false);
    }
  }
  return <main aria-label="Effective config" className="mx-auto flex max-w-[1100px] flex-col gap-16 p-24">
    <h1 className="m-0 text-22 font-semibold text-text">Effective config</h1>
    <p>Everything your agents load in this scope, and which scope it came from.</p>
    <nav aria-label="Effective config categories" className="flex gap-8">{["Skills", "MCP servers", "Instructions"].map((item) => <button key={item} aria-pressed={tab === item} onClick={() => setTab(item)}>{item}</button>)}</nav>
    {loading ? <p role="status">Resolving effective configuration…</p> : error ? <div role="alert"><p>{error}</p><button onClick={() => void refresh()}>Retry</button></div> : <>{rows.length === 0 && disabledHere.length === 0 && <p>Nothing configured at this scope.</p>}
      {rows.length > 0 && <ul className="m-0 list-none p-0">{rows.map((row) => <li key={row.logical_id} className="border-b border-border p-12">
        <div className="flex flex-wrap items-center gap-8"><strong>{row.name}</strong><span className="text-text-secondary">Winning scope: {row.winner_scope}</span><span className="text-text-secondary">{row.loads_in_agents} agents</span>{row.lock && <span role="status" aria-label={`Locked: ${row.lock.reason}`}>🔒 Locked: {row.lock.reason}</span>}
          {scope === "global" && row.item_id && (row.lock
            ? <button ref={restoreFocus} type="button" onClick={(event) => { restoreFocus.current = event.currentTarget; setLockTarget({ itemId: row.item_id as string, name: row.name, locked: true, reason: row.lock?.reason ?? "" }); }}>Unlock</button>
            : <button ref={restoreFocus} type="button" disabled={!globalWritable} title={!globalWritable ? globalReason : undefined} onClick={(event) => { restoreFocus.current = event.currentTarget; setLockReason(""); setLockTarget({ itemId: row.item_id as string, name: row.name, locked: false, reason: "" }); }}>Lock</button>)}
        </div>
        <p className="font-mono text-11 text-text-muted">{row.source_path}</p>
        <ul>{row.agents.map((agent) => <li key={agent.agent}><span>{agent.agent}: {agent.state} {agent.loads ? "· loaded" : "· not loaded"}</span>{row.item_id && <><br /><button onClick={() => setWhy({ itemId: row.item_id as string, agent: agent.agent })}>Why is this on?</button></>}</li>)}</ul>
      </li>)}</ul>}
      {disabledHere.length > 0 && <section aria-label="Disabled at this scope"><h2>Disabled here</h2><ul>{disabledHere.map((item) => <li key={`${item.logical_id}:${item.scope}:${item.agent ?? "all"}`}>{item.name} · {item.scope}{item.agent ? ` · ${item.agent}` : ""}{item.reason && ` — ${item.reason}`}</li>)}</ul></section>}
      {diagnostics.length + loadDiagnostics.length > 0 && <section aria-label="Configuration diagnostics"><h2>Diagnostics</h2><ul>{diagnostics.map((item, index) => <li key={`resolution-${index}`}>{item.message}</li>)}{loadDiagnostics.map((item, index) => <li key={`load-${index}`}>{item.message}</li>)}</ul></section>}
    </>}
    {why && <WhyTraceDialog itemId={why.itemId} scope={scope} agent={why.agent} onClose={() => setWhy(null)} />}
    {lockTarget && <div className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16" onMouseDown={(event) => { if (event.target === event.currentTarget) closeLockDialog(); }}>
      <section role="dialog" aria-modal="true" aria-labelledby="effective-lock-title" className="w-full max-w-[420px] rounded-12 border border-border bg-surface p-20" onKeyDown={(event) => { if (event.key === "Escape" && !lockBusy) closeLockDialog(); }}>
        <h2 id="effective-lock-title" className="m-0 text-18 font-semibold">{lockTarget.locked ? `Unlock ${lockTarget.name}?` : `Lock ${lockTarget.name}`}</h2>
        {lockTarget.locked ? <p className="text-13 text-text-secondary">Remove the Global lock? Lower-scope changes will be allowed again.</p> : <label className="mt-12 grid gap-6 text-13">Reason<input autoFocus maxLength={200} value={lockReason} onChange={(event) => setLockReason(event.target.value)} aria-label="Lock reason" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
        <div className="mt-16 flex justify-end gap-8">
          <button type="button" disabled={lockBusy} onClick={closeLockDialog}>Cancel</button>
          <button type="button" disabled={lockBusy || (!lockTarget.locked && (lockReason.trim().length === 0 || lockReason.trim().length > 200))} onClick={() => void applyLock()}>{lockBusy ? "Saving…" : lockTarget.locked ? "Unlock" : "Lock item"}</button>
        </div>
      </section>
    </div>}
  </main>;
}
