/**
 * Why-is-this-on overlay (TA-055).
 *
 * Modal dialog displaying the 4-scope decision chain for an item (Global, User,
 * Project, Local), highlighting the decisive winner, dimming absent scopes,
 * rendering error chips for unreadable scopes, and offering Open file, Copy path,
 * and Open Effective config actions.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button } from "../../components";
import type { BadgeTone } from "../../components";
import type { WhyTraceView } from "../../generated/WhyTraceView";
import { useToast } from "../toasts";
import { Modal } from "./Modal";
import { commandErrorMessage } from "../../lib/commandError";

export interface WhyTraceDialogProps {
  readonly open?: boolean;
  readonly itemId: string;
  readonly scope?: string;
  readonly agent: string;
  readonly onClose: () => void;
  readonly onOpenEffective?: () => void;
}

interface StatusDisplay {
  readonly label: string;
  readonly tone: BadgeTone;
  readonly osError?: string;
}

function parseStepStatus(
  status: WhyTraceView["trace"]["steps"][0]["status"],
): StatusDisplay {
  if (typeof status === "string") {
    switch (status) {
      case "Absent":
        return { label: "Absent", tone: "muted" };
      case "Inherited":
        return { label: "Inherited", tone: "info" };
      case "Defined":
        return { label: "Defined", tone: "neutral" };
      case "Disabled":
        return { label: "Disabled", tone: "warning" };
      case "Shadowed":
        return { label: "Shadowed", tone: "muted" };
      case "Winning":
        return { label: "Winning", tone: "success" };
      default:
        return { label: status, tone: "neutral" };
    }
  }
  if (typeof status === "object" && status !== null) {
    if ("Locked" in status) {
      return { label: "Locked", tone: "locked" };
    }
    if ("Unreadable" in status) {
      const err =
        typeof status.Unreadable === "object" &&
        status.Unreadable !== null &&
        "os_error" in status.Unreadable
          ? String(status.Unreadable.os_error)
          : "Permission denied";
      return { label: "Unreadable", tone: "danger", osError: err };
    }
  }
  return { label: "Unknown", tone: "neutral" };
}

/**
 * Renders the Why-is-this-on dialog with 4-scope explanation chain.
 */
export function WhyTraceDialog({
  open = true,
  itemId,
  scope = "user",
  agent,
  onClose,
  onOpenEffective,
}: WhyTraceDialogProps) {
  const toast = useToast();
  const [trace, setTrace] = useState<WhyTraceView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<WhyTraceView>("why_trace", {
        request: {
          item_id: itemId,
          scope,
          agent,
        },
      });
      setTrace(result);
    } catch (cause) {
      setError(commandErrorMessage(cause));
    } finally {
      setLoading(false);
    }
  }, [agent, itemId, scope]);

  useEffect(() => {
    if (open) void load();
  }, [load, open]);

  const decisiveIndex = trace?.trace.verdict.decisive_step;
  const decisiveStep =
    decisiveIndex !== undefined && decisiveIndex !== null
      ? trace?.trace.steps[decisiveIndex] ?? null
      : null;

  const description = trace?.trace.verdict.on
    ? `On, from ${decisiveStep?.scope ?? "unknown"}`
    : `Off, ${decisiveStep?.reason ?? "the decisive scope keeps this item disabled."}`;

  // Find most relevant recorded document path
  const bestLocation =
    trace?.trace.steps
      .slice()
      .reverse()
      .find((step) => step.location !== null)?.location ?? null;
  const recordedPath = bestLocation?.path ?? null;

  async function reveal(): Promise<void> {
    if (recordedPath === null) return;
    try {
      await invoke<void>("reveal_item", { itemId });
    } catch (cause) {
      toast.warning(commandErrorMessage(cause), { title: "Could not open file" });
    }
  }

  async function copyPath(): Promise<void> {
    if (recordedPath === null) return;
    try {
      await navigator.clipboard.writeText(recordedPath);
      toast.success("Path copied to clipboard", { title: recordedPath });
    } catch (cause) {
      toast.warning(commandErrorMessage(cause), { title: "Could not copy path" });
    }
  }

  const composedSteps =
    trace?.trace.steps.filter((s) => {
      const parsed = parseStepStatus(s.status);
      return parsed.label === "Winning" || parsed.label === "Defined" || parsed.label === "Inherited";
    }) ?? [];
  const isComposedRule = composedSteps.length > 1;

  return (
    <Modal
      open={open}
      title="Why is this on?"
      description={description}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      size="md"
      footer={
        <>
          <Button
            variant="ghost"
            onClick={onClose}
          >
            Close
          </Button>
          {onOpenEffective && (
            <Button
              variant="secondary"
              onClick={() => {
                onOpenEffective();
                onClose();
              }}
            >
              Open Effective config
            </Button>
          )}
          <Button
            variant="secondary"
            disabled={recordedPath === null}
            onClick={() => void copyPath()}
          >
            Copy path
          </Button>
          <Button
            variant="primary"
            disabled={recordedPath === null}
            onClick={() => void reveal()}
          >
            Open file
          </Button>
        </>
      }
    >
      <div className="grid gap-16">
        {loading && <p role="status" className="m-0 text-13 text-text-secondary">Loading scope trace…</p>}

        {error !== null && (
          <div role="alert" className="rounded-8 border border-border bg-raised p-12">
            <p className="m-0 text-13 text-danger">{error}</p>
            <Button size="sm" className="mt-8" onClick={() => void load()}>
              Retry
            </Button>
          </div>
        )}

        {trace !== null && (
          <div className="grid gap-12">
            <div className="rounded-8 border border-border bg-raised p-12">
              <span className="text-11 font-semibold uppercase tracking-wide text-text-muted">
                Verdict
              </span>
              <p className="m-0 mt-4 text-14 font-medium text-text">
                {description}
              </p>
              {isComposedRule && (
                <p className="m-0 mt-4 text-12 text-text-secondary">
                  Rule content is composed from {composedSteps.length} active scopes in precedence order.
                </p>
              )}
            </div>

            <ol aria-label="Scope evaluation steps" className="m-0 grid list-none gap-8 p-0">
              {trace.trace.steps.map((step, index) => {
                const parsed = parseStepStatus(step.status);
                const isWinning =
                  index === decisiveIndex ||
                  parsed.label === "Winning" ||
                  (trace.trace.verdict.on && index === decisiveIndex);
                const isAbsent = parsed.label === "Absent";

                return (
                  <li
                    key={`${step.scope}-${index}`}
                    aria-current={isWinning ? "step" : undefined}
                    className={`relative flex flex-col gap-4 rounded-8 border p-12 transition-colors ${
                      isWinning
                        ? "border-accent bg-accent/5 shadow-sm"
                        : isAbsent
                          ? "border-border/50 bg-raised/40 opacity-60"
                          : "border-border bg-raised"
                    }`}
                  >
                    <div className="flex items-center justify-between gap-8">
                      <div className="flex items-center gap-8">
                        <span className="text-13 font-semibold text-text">
                          {step.scope[0]?.toUpperCase() + step.scope.slice(1)}
                        </span>
                        <Badge tone={parsed.tone}>
                          {parsed.label}
                        </Badge>
                      </div>
                      {isWinning && (
                        <span className="text-11 font-medium text-accent">
                          Winner
                        </span>
                      )}
                    </div>

                    <p className="m-0 text-12 text-text-secondary">
                      {step.reason}
                    </p>

                    {parsed.osError && (
                      <p role="alert" className="m-0 text-12 text-danger">
                        {parsed.osError}
                      </p>
                    )}

                    {step.location && (
                      <div className="mt-4 flex items-center gap-6 font-mono text-11 text-text-muted">
                        <span className="truncate">{step.location.path}</span>
                        {step.location.line != null && (
                          <span className="shrink-0 text-text-secondary">
                            :{step.location.line}
                          </span>
                        )}
                      </div>
                    )}
                  </li>
                );
              })}
            </ol>

            {trace.load_diagnostics.length > 0 && (
              <section aria-label="Load diagnostics" className="rounded-8 border border-border p-12">
                <h3 className="m-0 text-12 font-semibold text-text-secondary">
                  Load diagnostics
                </h3>
                <ul className="m-0 mt-6 grid list-none gap-4 p-0 text-11 text-text-muted">
                  {trace.load_diagnostics.map((item, index) => (
                    <li key={`${item.scope}:${item.path}:${index}`}>
                      <span className="font-semibold">{item.scope}</span>: {item.message} · {item.path}
                    </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
