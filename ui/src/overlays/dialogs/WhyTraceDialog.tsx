import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Modal } from "./Modal";
import type { WhyTraceView } from "../../generated/WhyTraceView";

/** Displays the scope trace returned by the TA-007 engine query. */
export function WhyTraceDialog({ itemId, scope, agent, onClose }: { readonly itemId: string; readonly scope: string; readonly agent: string; readonly onClose: () => void }) {
  const [trace, setTrace] = useState<WhyTraceView | null>(null);
  const [error, setError] = useState<string | null>(null);
  async function load() { try { setTrace(await invoke<WhyTraceView>("why_trace", { request: { item_id: itemId, scope, agent } })); } catch (cause) { setError(String(cause)); } }
  useEffect(() => { void load(); }, [itemId, scope, agent]);
  return <Modal open title="Why is this on?" description={trace?.trace.verdict.on ? `On, from ${trace.trace.steps[trace.trace.verdict.decisive_step ?? 0]?.scope ?? "unknown"}` : "Off, the decisive scope keeps this item disabled."} onOpenChange={(open) => { if (!open) onClose(); }} footer={<button onClick={onClose}>Close</button>}>
    {error && <p role="alert">{error}</p>}
    {trace === null && !error && <p role="status">Loading scope trace…</p>}
    {trace !== null && <ol>{trace.trace.steps.map((step, index) => <li key={`${step.scope}-${index}`} className={index === trace.trace.verdict.decisive_step ? "font-semibold text-success" : "text-text-secondary"}><strong>{step.scope}</strong> — {JSON.stringify(step.status)} — {step.reason}{step.location && <small className="block font-mono">{step.location.path}{step.location.line === null ? "" : `:${step.location.line}`}</small>}</li>)}</ol>}
  </Modal>;
}
