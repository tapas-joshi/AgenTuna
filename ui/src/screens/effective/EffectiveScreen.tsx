import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAppSnapshot } from "../../state/context";
import { WhyTraceDialog } from "../../overlays/dialogs/WhyTraceDialog";
import type { EffectiveQueryView } from "../../generated/EffectiveQueryView";

/** Effective configuration view; query data is supplied by the typed TA-006 command. */
export function EffectiveScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const [tab, setTab] = useState("Skills");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [why, setWhy] = useState<{ itemId: string; agent: string } | null>(null);
  const [rows, setRows] = useState<EffectiveQueryView["items"]>([]);
  async function refresh() {
    setLoading(true); setError(null);
    try {
      const kind = tab === "Skills" ? "skill" : tab === "MCP servers" ? "mcp_server" : "rule";
      const result = await invoke<EffectiveQueryView>("effective_query", { request: { scope, kind, agent: null } });
      setRows(result.items);
    } catch (cause) { setError(String(cause)); }
    finally { setLoading(false); }
  }
  useEffect(() => { void refresh(); }, [scope, activeProject, tab]);
  return <main aria-label="Effective config" className="mx-auto flex max-w-[1100px] flex-col gap-16 p-24">
    <h1 className="m-0 text-22 font-semibold text-text">Effective config</h1>
    <p>Everything your agents load in this scope, and which scope it came from.</p>
    <nav aria-label="Effective config categories" className="flex gap-8">{["Skills", "MCP servers", "Instructions"].map((item) => <button key={item} aria-pressed={tab === item} onClick={() => setTab(item)}>{item}</button>)}</nav>
    {loading ? <p role="status">Resolving effective configuration…</p> : error ? <div role="alert"><p>{error}</p><button onClick={() => void refresh()}>Retry</button></div> : rows.length === 0 ? <p>Nothing configured at this scope.</p> : <ul className="m-0 list-none p-0">{rows.map((row) => <li key={`${row.name}:${row.winner_scope}`} className="border-b border-border p-12"><strong>{row.name}</strong><span className="ml-12 text-text-secondary">{row.winner_scope}</span><span className="ml-12 text-text-secondary">{row.loads_in_agents} agents</span><ul>{row.agents.map((agent) => <li key={agent.agent}>{agent.agent}: {agent.state} — {row.source_path} {row.item_id && <button onClick={() => setWhy({ itemId: row.item_id as string, agent: agent.agent })}>Why is this on?</button>}</li>)}</ul></li>)}</ul>}
    {why && <WhyTraceDialog itemId={why.itemId} scope={scope} agent={why.agent} onClose={() => setWhy(null)} />}
  </main>;
}
