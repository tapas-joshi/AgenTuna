import { useContext, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import type { SkillAgentPreviewRequest } from "../../generated/SkillAgentPreviewRequest";
import { ToastContext } from "../toasts";
import { moveRefusedItemAside, previewAdoption, applyAdoption } from "../dialogs/overwrite-commands";

export type AgentChoice = {
  readonly id: string;
  readonly label: string;
  readonly enabled: boolean;
  readonly disabledReason?: string;
  /**
   * Agents that read the same discovery folder as `id`. The choice is one row
   * and one write; the popover never renders a second switch for them.
   */
  readonly coupledWith?: readonly string[];
  /** Folder a coupled group writes, shown in the coupling warning. */
  readonly sharedPath?: string | null;
};

export interface EnableSkillPopoverProps {
  readonly itemId: string;
  readonly scope: string;
  readonly projectId: string | null;
  readonly agents: readonly AgentChoice[];
  readonly onClose: () => void;
  readonly onApplied: () => void;
}

interface NativeConflictInfo {
  readonly path: string;
  readonly message: string;
}

/** Lets the caller adjust enabled agent copies through reviewed TS-010 plans. */
export function EnableSkillPopover({
  itemId,
  scope,
  projectId,
  agents,
  onClose,
  onApplied,
}: EnableSkillPopoverProps) {
  const toastContext = useContext(ToastContext);
  const toast = toastContext ?? {
    show: () => "",
    success: () => "",
    info: () => "",
    warning: () => "",
    danger: () => "",
    error: () => "",
    commandError: () => "",
    jobError: () => "",
    dismiss: () => {},
    dismissAll: () => {},
  };
  const [selected, setSelected] = useState(
    () => new Set(agents.filter((agent) => agent.enabled).map((agent) => agent.id)),
  );
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [conflict, setConflict] = useState<NativeConflictInfo | null>(null);
  const containerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  async function apply(): Promise<void> {
    setBusy(true);
    setMessage(null);
    setConflict(null);
    try {
      const changed = agents.filter((agent) => selected.has(agent.id) !== agent.enabled);
      if (changed.length === 0) {
        toast.info("No changes to agent deployments.");
        onClose();
        return;
      }
      // A coupled group writes one folder; the representative's plan covers every reader.
      const coupled = new Set(changed.flatMap((agent) => [...(agent.coupledWith ?? [])]));

      const outcomes: string[] = [];
      for (const agent of changed) {
        if (coupled.has(agent.id)) continue;
        const request: SkillAgentPreviewRequest = {
          item_id: itemId,
          scope,
          agent_id: agent.id,
          enabled: selected.has(agent.id),
          project_id: projectId,
        };
        const preview = await invoke<SkillAgentPreviewView>("skill_agent_preview", { request });
        if (preview.warnings.length > 0) {
          setMessage(preview.warnings.join(" · "));
        }
        if (preview.no_op) continue;
        const outcome = await invoke<SkillAgentApplyView>("skill_agent_apply", {
          previewId: preview.preview_id,
        });
        outcomes.push(`${agent.label}: ${outcome.changed ? "updated" : "unchanged"}`);
      }

      const summary = outcomes.length > 0 ? outcomes.join(" · ") : "No changes";
      toast.success(summary, { title: "Deployments updated" });
      onApplied();
      onClose();
    } catch (cause) {
      const err = cause as {
        code?: string;
        message?: string;
        refusal?: { path?: string; remedies?: string[] };
      };
      const isConflict =
        err?.code === "refused_overwrite" ||
        err?.code === "RefusedOverwrite" ||
        err?.code === "Conflict" ||
        Boolean(err?.refusal) ||
        (typeof err?.message === "string" && err.message.toLowerCase().includes("adopt"));

      if (isConflict) {
        // Leave toggle unchanged by resetting selection to current server state
        setSelected(new Set(agents.filter((a) => a.enabled).map((a) => a.id)));
        setConflict({
          path: err.refusal?.path ?? "native folder",
          message:
            err.message ??
            "An unmanaged copy exists at this destination. Adopt it or move it aside first.",
        });
        return;
      }
      setMessage(String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function handleAdopt(path: string): Promise<void> {
    setBusy(true);
    try {
      await previewAdoption(path, null, "link");
      await applyAdoption(path, null, "link");
      toast.success("Adopted unmanaged skill folder");
      setConflict(null);
      onApplied();
    } catch (cause) {
      toast.warning(String(cause), { title: "Could not adopt item" });
    } finally {
      setBusy(false);
    }
  }

  async function handleMoveAside(path: string): Promise<void> {
    setBusy(true);
    try {
      const result = await moveRefusedItemAside(path);
      toast.success(result.summary);
      setConflict(null);
      onApplied();
    } catch (cause) {
      toast.warning(String(cause), { title: "Could not move aside item" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section
      ref={containerRef}
      tabIndex={-1}
      role="dialog"
      aria-label="Enable for agents"
      className="grid gap-12 rounded-8 border border-border bg-raised p-16 shadow-lg outline-none"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="flex items-center justify-between border-b border-border pb-8">
        <h3 className="m-0 text-13 font-semibold text-text">Enable for agents</h3>
        <button
          type="button"
          aria-label="Close"
          className="cursor-pointer border-none bg-transparent text-14 text-text-muted hover:text-text"
          onClick={onClose}
        >
          ✕
        </button>
      </div>

      <div className="grid gap-8">
        {agents.map((agent) => {
          const isChecked = selected.has(agent.id);
          const isDisabled = Boolean(agent.disabledReason) || busy;

          return (
            <label
              key={agent.id}
              className={`flex items-center justify-between gap-8 rounded-4 p-4 text-12 ${
                isDisabled ? "cursor-not-allowed opacity-50" : "cursor-pointer hover:bg-surface"
              }`}
            >
              <span className="flex items-center gap-8">
                <input
                  type="checkbox"
                  checked={isChecked}
                  disabled={isDisabled}
                  onChange={(event) => {
                    setSelected((current) => {
                      const next = new Set(current);
                      if (event.target.checked) next.add(agent.id);
                      else next.delete(agent.id);
                      return next;
                    });
                  }}
                  className="rounded border-border text-accent focus:ring-accent"
                />
                <span className="text-text">{agent.label}</span>
                {(agent.coupledWith?.length ?? 0) > 0 && (
                  <span className="text-11 text-scope-user">one folder</span>
                )}
              </span>
              {agent.disabledReason && (
                <span className="text-11 text-text-muted">{agent.disabledReason}</span>
              )}
            </label>
          );
        })}
      </div>

      {agents.some((agent) => (agent.coupledWith?.length ?? 0) > 0) && (
        <div role="note" className="rounded-6 border border-warning/40 bg-warning/10 p-8 text-11 text-text">
          <strong className="block text-12">Shared discovery root</strong>
          {agents
            .filter((agent) => (agent.coupledWith?.length ?? 0) > 0)
            .map(
              (agent) =>
                `${agent.label} read ${agent.sharedPath ?? "one folder"} — one folder on disk. This switch changes every reader; AgenTuna never offers a separate one.`,
            )
            .join(" ")}
        </div>
      )}

      {conflict !== null && (
        <div role="alert" className="grid gap-8 rounded-6 border border-warning/40 bg-warning/10 p-8 text-11">
          <p className="m-0 text-warning">{conflict.message}</p>
          <div className="flex gap-8">
            <Button size="sm" variant="secondary" onClick={() => void handleAdopt(conflict.path)}>
              Adopt
            </Button>
            <Button size="sm" variant="secondary" onClick={() => void handleMoveAside(conflict.path)}>
              Move aside
            </Button>
          </div>
        </div>
      )}

      {message && <p role="status" className="m-0 text-11 text-text-secondary">{message}</p>}

      <div className="flex justify-end gap-8 border-t border-border pt-8">
        <Button size="sm" variant="ghost" disabled={busy} onClick={onClose}>
          Cancel
        </Button>
        <Button size="sm" variant="primary" disabled={busy} onClick={() => void apply()}>
          {busy ? "Applying…" : "Apply"}
        </Button>
      </div>
    </section>
  );
}
