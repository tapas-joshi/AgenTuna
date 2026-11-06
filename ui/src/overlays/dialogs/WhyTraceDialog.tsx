import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Modal } from "./Modal";
import type { WhyTraceView } from "../../generated/WhyTraceView";

/** Displays the scope trace returned by the TA-007 engine query. */
export function WhyTraceDialog({ itemId, scope, agent, onClose }: { readonly itemId: string; readonly scope: string; readonly agent: string; readonly onClose: () => void }) {
  const [trace, setTrace] = useState<WhyTraceView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  async function load() { setError(null); try { setTrace(await invoke<WhyTraceView>("why_trace", { request: { item_id: itemId, scope, agent } })); } catch (cause) { setError(String(cause)); } }
  useEffect(() => { void load(); }, [itemId, scope, agent]);
  const path = trace?.trace.steps.slice().reverse().find((step) => step.location !== null)?.location?.path ?? null;
  async function reveal() {
    try { await invoke<void>("reveal_item", { itemId }); }
    catch (cause) { setActionError(String(cause)); }
  }
  async function copyPath() {
    if (path === null) return;
    try { await navigator.clipboard.writeText(path); setActionError("Path copied."); }
    catch (cause) { setActionError(String(cause)); }
  }
  return <Modal open title="Why is this on?" description={trace?.trace.verdict.on ? `On, from ${trace.trace.steps[trace.trace.verdict.decisive_step ?? 0]?.scope ?? "unknown"}` : "Off, the decisive scope keeps this item disabled."} onOpenChange={(open) => { if (!open) onClose(); }} footer={<><button disabled={!trace} onClick={() => void reveal()}>Open file</button><button disabled={path === null} onClick={() => void copyPath()}>Copy path</button><button onClick={onClose}>Open Effective config</button><button onClick={onClose}>Close</button></>}>
    {error && <div role="alert"><p>{error}</p><button onClick={() => void load()}>Retry</button></div>}
    {actionError && <p role="status">{actionError}</p>}
    {trace === null && !error && <p role="status">Loading scope trace…</p>}
    {trace !== null && <><ol>{trace.trace.steps.map((step, index) => <li key={`${step.scope}-${index}`} className={index === trace.trace.verdict.decisive_step ? "font-semibold text-success" : "text-text-secondary"}><strong>{step.scope}</strong> — {typeof step.status === "string" ? step.status : Object.keys(step.status)[0]} — {step.reason}{step.location && <small className="block font-mono">{step.location.path}{step.location.line === null ? "" : `:${step.location.line}`}</small>}</li>)}</ol>{trace.load_diagnostics.length > 0 && <section aria-label="Load diagnostics"><h2>Load diagnostics</h2><ul>{trace.load_diagnostics.map((item, index) => <li key={`${item.scope}:${item.path}:${index}`}>{item.scope}: {item.message} · {item.path}</li>)}</ul></section>}</>}
  </Modal>;
}
