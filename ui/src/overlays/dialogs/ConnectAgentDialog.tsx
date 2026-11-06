/**
 * Connect agent / Set path dialog (TA-059).
 *
 * Modal over `at_engine::agent_paths` via the typed `agent_path_*` commands.
 * Opening lists the adapter's candidate paths with their probe results and
 * focuses the path input. Typing validates on change: a missing path or a
 * non-directory keeps Save disabled with a reason under the field, while an
 * existing directory without a skills folder or config file only shows a soft
 * warning. Browse fills the input through the native OS picker. Saving writes
 * the override through `agent_path_set` and refreshes detection; Esc and Cancel
 * write nothing and reopening shows the saved override prefilled.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, Input } from "../../components";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { AgentPathCandidateView } from "../../generated/AgentPathCandidateView";
import type { AgentPathValidation } from "../../generated/AgentPathValidation";
import { useToast } from "../toasts";
import { Modal } from "./Modal";

export interface ConnectAgentDialogProps {
  /** The agent being connected; `null` closes the dialog. */
  readonly agent: AgentCardView | null;
  /** Called after a close with nothing written. */
  readonly onClose: () => void;
  /** Called with the refreshed card after a successful save. */
  readonly onSaved: (updated: AgentCardView) => void | Promise<void>;
}

/** Probe marker text for one candidate row. */
function candidateMarker(candidate: AgentPathCandidateView): string {
  if (candidate.is_override) return "Saved by you";
  if (candidate.verified) return "Verified";
  if (candidate.exists) return "Exists, unexpected shape";
  return "Not found";
}

/**
 * Renders the connect-agent dialog. All state is local; Esc and Cancel never
 * write, and the parent owns the refreshed card through {@link onSaved}.
 */
export function ConnectAgentDialog({ agent, onClose, onSaved }: ConnectAgentDialogProps) {
  const open = agent !== null;
  const toast = useToast();
  const [candidates, setCandidates] = useState<readonly AgentPathCandidateView[]>([]);
  const [pathValue, setPathValue] = useState("");
  const [validation, setValidation] = useState<AgentPathValidation | null>(null);
  const [validateBusy, setValidateBusy] = useState(false);
  const [saveBusy, setSaveBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const requestId = useRef(0);

  useEffect(() => {
    if (!open || agent === null) return;
    setPathValue(agent.primary_path ?? "");
    setValidation(null);
    setError(null);
    inputRef.current?.focus();
    let cancelled = false;
    invoke<readonly AgentPathCandidateView[]>("agent_path_candidates", {
      agentId: agent.id,
    })
      .then((result) => {
        if (!cancelled) setCandidates(result);
      })
      .catch((cause) => {
        if (!cancelled) setError(String(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [agent, open]);

  useEffect(() => {
    if (!open) return;
    const trimmed = pathValue.trim();
    const current = ++requestId.current;
    setValidateBusy(true);
    invoke<AgentPathValidation>("agent_path_validate", {
      agentId: agent?.id ?? "",
      path: trimmed,
    })
      .then((result) => {
        if (requestId.current === current) setValidation(result);
      })
      .catch((cause) => {
        if (requestId.current === current) setError(String(cause));
      })
      .finally(() => {
        if (requestId.current === current) setValidateBusy(false);
      });
  }, [agent?.id, open, pathValue]);

  const browse = useCallback(async () => {
    try {
      const selected = await invoke<string | null>("agent_path_choose", {
        title: `Choose the ${agent?.name ?? "agent"} configuration folder`,
      });
      if (selected !== null) setPathValue(selected);
    } catch (cause) {
      setError(String(cause));
    }
  }, [agent?.name]);

  const invalid = validation === null || validation.reason !== null;
  const warning = validation?.structure_warning ?? null;

  async function save(): Promise<void> {
    if (agent === null || invalid || saveBusy) return;
    setSaveBusy(true);
    setError(null);
    try {
      await invoke<AgentCardView>("agent_path_set", {
        agentId: agent.id,
        path: pathValue.trim(),
      });
      // Re-run detection so presence flips to Installed without a restart.
      const agents = await invoke<AgentCardView[]>("agents_rescan");
      const updated = agents.find((row) => row.id === agent.id) ?? null;
      onClose();
      toast.success(`${agent.name} is connected`, {
        title: updated?.primary_path ?? pathValue.trim(),
      });
      if (updated !== null) await onSaved(updated);
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Path not saved" });
    } finally {
      setSaveBusy(false);
    }
  }

  return (
    <Modal
      open={open}
      title={`Set path for ${agent?.name ?? "agent"}`}
      description="Point AgenTuna at this agent's configuration folder. The path is verified before it is saved."
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      initialFocusRef={inputRef}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="secondary" onClick={() => void browse()}>
            Choose folder…
          </Button>
          <Button
            variant="primary"
            disabled={invalid || validateBusy || saveBusy}
            onClick={() => void save()}
          >
            {saveBusy ? "Saving…" : "Save"}
          </Button>
        </>
      }
    >
      <div className="grid gap-14">
        {candidates.length > 0 && (
          <fieldset className="m-0 grid gap-6 rounded-8 border border-border p-12">
            <legend className="px-4 text-12 text-text-secondary">
              Locations {agent?.name ?? "this agent"} usually uses
            </legend>
            <ul className="m-0 grid gap-4 p-0 text-12">
              {candidates.map((candidate) => (
                <li key={`${candidate.scope}-${candidate.kind}-${candidate.path}`} className="flex list-none items-center justify-between gap-8">
                  <button
                    type="button"
                    className="min-w-0 cursor-pointer truncate border-none bg-transparent p-0 text-left font-mono text-12 text-text underline decoration-dotted"
                    onClick={() => setPathValue(candidate.path)}
                  >
                    {candidate.path}
                  </button>
                  <span
                    className={`shrink-0 text-11 ${candidate.exists ? "text-text-secondary" : "text-text-muted"}`}
                  >
                    {candidate.exists ? candidateMarker(candidate) : "not found"}
                  </span>
                </li>
              ))}
            </ul>
          </fieldset>
        )}
        <label className="grid gap-6 text-13">
          Configuration path
          <Input
            ref={inputRef}
            value={pathValue}
            onChange={(event) => setPathValue(event.target.value)}
            placeholder="/Users/you/.myagent"
            aria-label="Agent configuration path"
            aria-invalid={invalid}
          />
        </label>
        <div aria-live="polite" className="grid gap-4">
          {validation?.reason != null && (
            <p role="alert" className="m-0 text-12 text-danger">
              {validation.reason}
            </p>
          )}
          {warning !== null && (
            <p role="status" className="m-0 text-12 text-warning">
              {warning}
            </p>
          )}
          {validation !== null && validation.reason === null && warning === null && !validateBusy && (
            <p role="status" className="m-0 text-12 text-success">
              Looks like a valid {agent?.name ?? "agent"} folder.
            </p>
          )}
        </div>
        {error !== null && (
          <p role="alert" className="m-0 text-12 text-danger">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
