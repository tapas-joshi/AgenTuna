import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import type { SkillAgentPreviewRequest } from "../../generated/SkillAgentPreviewRequest";

type AgentChoice = { readonly id: string; readonly label: string; readonly enabled: boolean; readonly disabledReason?: string };

/** Lets the caller adjust enabled agent copies through reviewed TS-010 plans. */
export function EnableSkillPopover({ itemId, scope, projectId, agents, onClose, onApplied }: { readonly itemId: string; readonly scope: string; readonly projectId: string | null; readonly agents: readonly AgentChoice[]; readonly onClose: () => void; readonly onApplied: () => void }) {
  const [selected, setSelected] = useState(() => new Set(agents.filter((agent) => agent.enabled).map((agent) => agent.id)));
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  async function apply() {
    setBusy(true);
    setMessage(null);
    try {
      const changed = agents.filter((agent) => selected.has(agent.id) !== agent.enabled);
      const outcomes: string[] = [];
      for (const agent of changed) {
        const request: SkillAgentPreviewRequest = { item_id: itemId, scope, agent_id: agent.id, enabled: selected.has(agent.id), project_id: projectId };
        const preview = await invoke<SkillAgentPreviewView>("skill_agent_preview", { request });
        if (preview.warnings.length > 0) setMessage(preview.warnings.join(" · "));
        if (preview.no_op) continue;
        const outcome = await invoke<SkillAgentApplyView>("skill_agent_apply", { previewId: preview.preview_id });
        outcomes.push(`${agent.label}: ${outcome.changed ? "updated" : "unchanged"}`);
      }
      setMessage(outcomes.length ? outcomes.join(" · ") : "No changes");
      onApplied();
    } catch (cause) { setMessage(String(cause)); }
    finally { setBusy(false); }
  }
  return <section role="dialog" aria-label="Enable for agents" className="rounded-lg border border-border bg-surface p-16"><h2>Enable for agents</h2>{agents.map((agent) => <label key={agent.id} className="block"><input type="checkbox" checked={selected.has(agent.id)} disabled={Boolean(agent.disabledReason) || busy} onChange={(event) => setSelected((current) => { const next = new Set(current); if (event.target.checked) next.add(agent.id); else next.delete(agent.id); return next; })} /> {agent.label}{agent.disabledReason && <small> — {agent.disabledReason}</small>}</label>)}{message && <p role="status">{message}</p>}<button disabled={busy} onClick={() => void apply()}>{busy ? "Applying…" : "Apply"}</button><button disabled={busy} onClick={onClose}>Cancel</button></section>;
}
