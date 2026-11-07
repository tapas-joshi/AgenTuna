import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Badge, Button } from "../../components";
import type { JobProgressEvent } from "../../generated/JobProgressEvent";
import type { MigrateApplyResultView } from "../../generated/MigrateApplyResultView";
import type { MigrateConflictView } from "../../generated/MigrateConflictView";
import type { MigratePlanView } from "../../generated/MigratePlanView";
import type { MigrateScanItemView } from "../../generated/MigrateScanItemView";
import type { MigrateScanView } from "../../generated/MigrateScanView";
import { useAppSnapshot, useAppState } from "../../state/context";
import { awaitApplyJob, describeCause, describeFailure, failureFromCommandError } from "./applyJob";
import type { ApplyFailure, ApplyJobBridge } from "./applyJob";
import { MigrateDoneScreen } from "./MigrateDoneScreen";
import { MigratePlanScreen } from "./MigratePlanScreen";
import { MigrateResolveScreen, resolutionPayload } from "./MigrateResolveScreen";
import type { ConflictChoices, ConflictNames } from "./MigrateResolveScreen";

type Step = "scan" | "resolve" | "plan" | "done";
type Strategy = "replace_with_links" | "leave_and_watch";

/** Poll cadence while the cancellable scan job runs. */
const SCAN_POLL_INTERVAL_MS = 50;
/** Upper bound on polls, so a lost job surfaces as an error instead of hanging. */
const SCAN_POLL_ATTEMPTS = 600;

/** The apply wait reads the retained job result and the finished event through this bridge. */
const APPLY_BRIDGE: ApplyJobBridge = {
  invoke: (command, args) => invoke(command, args),
  listen: <T,>(event: string, handler: (payload: T) => void) =>
    listen<T>(event, (message) => handler(message.payload)),
};

const STEPS: readonly { readonly id: Step | "done"; readonly label: string }[] = [
  { id: "scan", label: "Scan" },
  { id: "resolve", label: "Resolve" },
  { id: "plan", label: "Plan" },
  { id: "done", label: "Done" },
];

function itemKey(item: MigrateScanItemView): string {
  return `${item.kind}:${item.path}:${item.content_hash ?? ""}`;
}
export function formatPath(path: string): string {
  if (path.startsWith("~/") || path.startsWith("./")) return path;
  const homeMatch = path.match(/(?:^|\/)(?:Users\/[^/]+|home\/[^/]+|\.agentuna-shots-sandbox\/home)(.*)/);
  if (homeMatch?.[1]) {
    return `~${homeMatch[1]}`;
  }
  const projectMatch = path.match(/(?:^|\/)(?:payments-service|ai-coding-config-studio|AgenTuna)(.*)/);
  if (projectMatch?.[1]) {
    return `.${projectMatch[1]}`;
  }
  return path;
}

/** Human label for the native item kind a scanned root holds. */
function rootKindLabel(kind: string): string {
  if (kind === "mcp_server") return "MCP";
  if (kind === "rule") return "rules";
  if (kind === "skill") return "skills";
  return kind;
}

/** Short state label for one scanned row, from the backend's authoritative label. */
function itemState(item: MigrateScanItemView): { readonly label: string; readonly tone: "success" | "warning" | "danger" | "info" } {
  if (item.parse_error !== null) return { label: "Unreadable", tone: "danger" };
  if (item.state === "in_agentuna") return { label: "In AgenTuna", tone: "success" };
  if (item.state === "conflict") return { label: "Conflict", tone: "warning" };
  return { label: "Not in AgenTuna", tone: "info" };
}

/** Migration wizard: scan, resolve conflicts, then the reviewed plan. */
export function MigrateScanScreen() {
  const { activeProject, screenState } = useAppSnapshot();
  const app = useAppState();
  const kindFilter = screenState["migrate:kind"] as string | undefined;
  const [step, setStep] = useState<Step>("scan");
  const [report, setReport] = useState<MigrateScanView | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [choices, setChoices] = useState<ConflictChoices>({});
  const [names, setNames] = useState<ConflictNames>({});
  const [conflicts, setConflicts] = useState<readonly MigrateConflictView[]>([]);
  const [strategy, setStrategy] = useState<Strategy>("replace_with_links");
  const [plan, setPlan] = useState<MigratePlanView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const [staleSelectionCount, setStaleSelectionCount] = useState(0);
  const [applyResult, setApplyResult] = useState<MigrateApplyResultView | null>(null);
  const [applyFailure, setApplyFailure] = useState<ApplyFailure | null>(null);
  const [scanProgress, setScanProgress] = useState<JobProgressEvent | null>(null);
  const [scanJobId, setScanJobId] = useState<string | null>(null);
  const [scanNotice, setScanNotice] = useState<string | null>(null);
  const scanRunRef = useRef(0);
  const scanJobRef = useRef<string | null>(null);
  const scanCancelledRef = useRef(false);
  const disposedRef = useRef(false);

  const applyReport = useCallback(
    (next: MigrateScanView) => {
      const keys = new Set(next.items.map(itemKey));
      const retained = new Set([...selected].filter((key) => keys.has(key)));
      setStaleSelectionCount(selected.size - retained.size);
      setSelected(retained);
      setReport(next);
    },
    [selected],
  );

  const startScan = useCallback(async () => {
    const run = scanRunRef.current + 1;
    scanRunRef.current = run;
    const isCurrent = () => run === scanRunRef.current && !disposedRef.current;
    scanCancelledRef.current = false;
    setBusy(true);
    setError(null);
    setScanNotice(null);
    setScanProgress(null);
    try {
      const jobId = await invoke<string>("migrate_scan_start", { projectId: activeProject });
      if (!isCurrent()) return;
      scanJobRef.current = jobId;
      setScanJobId(jobId);
      let next: MigrateScanView | null = null;
      let failure: string | null = null;
      for (let attempt = 0; attempt < SCAN_POLL_ATTEMPTS; attempt++) {
        if (!isCurrent()) return;
        try {
          next = await invoke<MigrateScanView | null>("migrate_scan_result", { jobId });
          if (next) break;
        } catch (cause) {
          // A cancelled job records no result, so the failed lookup is the cancel signal.
          failure = scanCancelledRef.current ? null : describeCause(cause);
          break;
        }
        const { promise, resolve } = Promise.withResolvers<void>();
        setTimeout(resolve, SCAN_POLL_INTERVAL_MS);
        await promise;
      }
      if (!isCurrent()) return;
      if (next) {
        applyReport(next);
      } else if (scanCancelledRef.current) {
        setScanNotice("Scan cancelled. The previous results are unchanged.");
      } else {
        setError(failure ?? "The scan did not produce a result.");
      }
    } catch (cause) {
      if (isCurrent()) setError(describeCause(cause));
    } finally {
      if (isCurrent()) {
        scanJobRef.current = null;
        setScanJobId(null);
        setScanProgress(null);
        setBusy(false);
      }
    }
  }, [activeProject, applyReport]);

  const cancelScan = useCallback(async () => {
    const jobId = scanJobRef.current;
    if (jobId === null) return;
    scanCancelledRef.current = true;
    setScanProgress((current) => ({
      id: jobId,
      name: current?.name ?? "Migration scan",
      fraction: current?.fraction ?? null,
      label: "Cancelling scan…",
    }));
    try {
      await invoke<boolean>("jobs_cancel", { jobId });
    } catch (cause) {
      setError(describeCause(cause));
    }
  }, []);

  useEffect(() => {
    disposedRef.current = false;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<JobProgressEvent>("agentuna:job-progress", ({ payload }) => {
      if (scanJobRef.current === payload.id) setScanProgress(payload);
    })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      disposedRef.current = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    void startScan();
    // The initial scan runs once per project; reselection is a user action.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProject]);

  async function handleApply(): Promise<void> {
    if (!plan) return;
    setBusy(true);
    setPlanError(null);
    setApplyFailure(null);
    try {
      const jobId = await invoke<string>("migrate_apply", {
        request: {
          project_id: activeProject,
          selected_paths: selectedPaths,
          resolutions: resolutionPayload(conflicts, choices, names),
          strategy,
          plan_hash: plan.plan_hash,
        },
      });
      // The job registry keeps a finished job's result by id, so the wait ends on the
      // backend's own answer — the reviewed result, or the typed failure the job reported —
      // instead of a deadline the screen invented.
      const outcome = await awaitApplyJob(APPLY_BRIDGE, jobId);
      if (outcome.kind === "applied") {
        setApplyResult(outcome.result);
      } else {
        setApplyFailure(outcome.failure);
      }
      setStep("done");
    } catch (cause) {
      setApplyFailure(failureFromCommandError(cause));
      setStep("done");
    } finally {
      setBusy(false);
    }
  }

  const rawItems = report?.items ?? [];
  const items = kindFilter ? rawItems.filter((i) => i.kind === kindFilter) : rawItems;
  const selectedPaths = useMemo(
    () => items.filter((item) => selected.has(itemKey(item))).map((item) => item.path),
    [items, selected],
  );

  async function buildPlan(nextStrategy: Strategy, resolved: readonly MigrateConflictView[]): Promise<void> {
    setBusy(true);
    setPlanError(null);
    try {
      const built = await invoke<MigratePlanView>("migrate_plan", {
        request: {
          project_id: activeProject,
          selected_paths: selectedPaths,
          resolutions: resolutionPayload(resolved, choices, names),
          strategy: nextStrategy,
        },
      });
      setPlan(built);
    } catch (cause) {
      setPlan(null);
      setPlanError(describeCause(cause));
    } finally {
      setBusy(false);
    }
  }

  const stepIndex = step === "scan" ? 0 : step === "resolve" ? 1 : step === "plan" ? 2 : 3;

  return (
    <main aria-label="Move into AgenTuna" className="mx-auto flex min-h-full max-w-[1180px] flex-col gap-16 p-24">
      {step !== "scan" ? null : (
        <header className="flex flex-col gap-4">
          <p className="m-0 text-12 uppercase tracking-wide text-text-muted">
            {kindFilter === "mcp_server" ? "MCP Servers / Import from apps" : "Migrate to AgenTuna"}
          </p>
          <h1 className="m-0 text-26 font-semibold text-text">
            {kindFilter === "mcp_server"
              ? "Review before import"
              : items.length === 0
                ? "No items outside AgenTuna"
                : `Found ${items.length} item${items.length === 1 ? "" : "s"} outside AgenTuna`}
          </h1>
          <p className="m-0 text-13 text-text-secondary">
            {kindFilter === "mcp_server"
              ? `${items.length} definition${items.length === 1 ? "" : "s"} across User, Project and Local sources. Nothing is merged or written until you resolve conflicts and approve.`
              : "They live only in agent folders. Pick what to move into the AgenTuna store."}
          </p>
        </header>
      )}

      <nav aria-label="Migration steps" className="flex items-center gap-8">
        {STEPS.map((entry, index) => {
          const active = index === stepIndex;
          const complete = index < stepIndex;
          return (
            <div key={entry.id} className="flex items-center gap-8">
              {index > 0 && <span aria-hidden="true" className="h-px w-24 bg-border" />}
              <span
                className={`inline-flex items-center gap-6 text-13 ${
                  active ? "text-text" : complete ? "text-success" : "text-text-muted"
                }`}
                aria-current={active ? "step" : undefined}
              >
                <span
                  aria-hidden="true"
                  className={`grid h-24 w-24 shrink-0 place-items-center rounded-full border text-11 ${
                    active
                      ? "border-accent-fill bg-accent-fill text-on-accent"
                      : complete
                        ? "border-success/60 bg-success/15 text-success"
                        : "border-border text-text-muted"
                  }`}
                >
                  {complete ? "✓" : index + 1}
                </span>
                {entry.label}
              </span>
            </div>
          );
        })}
      </nav>

      {step === "resolve" ? (
        <MigrateResolveScreen
          projectId={activeProject}
          selectedPaths={selectedPaths}
          choices={choices}
          names={names}
          onChoicesChange={setChoices}
          onNamesChange={setNames}
          onBack={() => setStep("scan")}
          onContinue={(resolved) => {
            setConflicts(resolved);
            setStep("plan");
            void buildPlan(strategy, resolved);
          }}
        />
      ) : step === "done" ? (
        <MigrateDoneScreen
          result={applyResult}
          failedStep={applyFailure === null ? null : applyFailure.code === "" ? "apply" : applyFailure.code}
          failureCause={applyFailure === null ? null : describeFailure(applyFailure)}
          onRetry={() => {
            setApplyFailure(null);
            setStep("plan");
            void buildPlan(strategy, conflicts);
          }}
          onFinish={() => {
            setApplyResult(null);
            setApplyFailure(null);
            setStep("scan");
            void startScan();
          }}
          onViewMcp={() => {
            setApplyResult(null);
            setApplyFailure(null);
            setStep("scan");
            void startScan();
          }}
        />
      ) : step === "plan" ? (
        <MigratePlanScreen
          plan={plan}
          strategy={strategy}
          busy={busy}
          error={planError}
          onStrategyChange={(next) => {
            setStrategy(next);
            void buildPlan(next, conflicts);
          }}
          onApply={handleApply}
          onBack={() => setStep("resolve")}
          onCancel={() => setStep("scan")}
        />
      ) : (
        <>
          {busy && (
            <div
              role="status"
              aria-live="polite"
              className="flex flex-wrap items-center gap-10 rounded-8 border border-accent/30 bg-accent/5 p-10 text-13 text-text"
            >
              <span>{scanProgress?.label ?? "Scanning agent folders…"}</span>
              <progress
                aria-label="Scan progress"
                max={1}
                value={scanProgress?.fraction ?? undefined}
                className="min-w-[120px] flex-1"
              />
              {scanJobId !== null && (
                <Button size="sm" onClick={() => void cancelScan()}>
                  Cancel scan
                </Button>
              )}
            </div>
          )}
          {scanNotice !== null && (
            <p role="status" className="m-0 text-12 text-text-secondary">
              {scanNotice}
            </p>
          )}
          {error !== null && (
            <div role="alert" className="flex items-center justify-between gap-12 rounded-8 border border-danger/40 bg-danger/10 p-12">
              <p className="m-0 text-13 text-danger">{error}</p>
              <Button size="sm" onClick={() => void startScan()}>
                Retry
              </Button>
            </div>
          )}
          {report?.partial === true && (
            <p role="status" className="m-0 text-12 text-warning">
              Some agent folders could not be read; found items are still available.
            </p>
          )}
          {report !== null && report.warnings.length > 0 && (
            <ul
              aria-label="Scan warnings"
              className="m-0 flex list-none flex-col gap-4 rounded-8 border border-warning/40 bg-warning/10 p-12 text-12 text-warning"
            >
              {report.warnings.map((warning) => (
                <li key={`${warning.agent}:${warning.kind}:${warning.path}`}>
                  <span className="font-mono">{warning.path}</span> — {warning.message}
                </li>
              ))}
            </ul>
          )}
          {staleSelectionCount > 0 && (
            <p role="status" className="m-0 text-12 text-warning">
              {staleSelectionCount} selection{staleSelectionCount === 1 ? "" : "s"} changed or disappeared and
              were cleared.
            </p>
          )}

          <div className="grid min-h-[520px] grid-cols-[320px_1fr] gap-16 max-[1099px]:grid-cols-[minmax(200px,0.6fr)_minmax(0,1.4fr)]">
            <aside aria-label="Scanned folders" className="rounded-10 border border-border bg-raised p-12">
              <h2 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">
                Scanned folders
              </h2>
              {report === null || report.roots.length === 0 ? (
                <div className="mt-8 flex flex-col gap-8">
                  <p className="m-0 text-13 text-text-muted">Nothing scanned yet.</p>
                  <Button size="sm" disabled={busy} onClick={() => void startScan()}>
                    Rescan
                  </Button>
                </div>
              ) : (
                <ul className="m-0 mt-8 flex list-none flex-col gap-10 p-0">
                  {report.roots.map((root) => (
                    <li key={`${root.kind}:${root.path}`}>
                      <p className="m-0 truncate font-mono text-12 text-text" title={root.path}>
                        {root.path}
                      </p>
                      <div className="flex items-center justify-between gap-6">
                        <span className="text-11 text-text-muted">{root.agents.join(", ")} · {rootKindLabel(root.kind)}</span>
                        <span
                          className={`text-11 ${
                            root.status === "unreadable"
                              ? "text-danger"
                              : root.status === "not_present" || root.status === "empty"
                                ? "text-text-muted"
                                : root.new_items > 0
                                  ? "text-warning"
                                  : "text-success"
                          }`}
                        >
                          {root.status === "not_present"
                            ? "not present"
                            : root.status === "unreadable"
                              ? "unreadable"
                              : root.status === "empty"
                                ? "0 new"
                                : root.new_items > 0
                                  ? `${root.new_items} new`
                                  : `${root.in_sync} in sync`}
                        </span>
                      </div>
                      {root.status === "unreadable" && root.unreadable_reason !== null && (
                        <p className="m-0 text-11 text-danger">{root.unreadable_reason}</p>
                      )}
                      {root.status === "empty" && <p className="m-0 text-11 text-text-muted">empty</p>}
                      {root.status === "scanned" && (
                        <p className="m-0 text-11 text-text-muted">
                          {root.found_items} found{root.skipped_items > 0 ? ` · ${root.skipped_items} skipped` : ""}
                        </p>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </aside>

            <section aria-label="Scanned items" className="flex flex-col rounded-10 border border-border bg-raised">
              {items.length === 0 ? (
                <div className="p-24">
                  <h2 className="m-0 text-16 font-semibold">Nothing to migrate</h2>
                  <p className="mt-8 text-13 text-text-secondary">
                    No native items were found in agent folders.
                  </p>
                  <div className="mt-12">
                    <Button size="sm" disabled={busy} onClick={() => void startScan()}>
                      Rescan
                    </Button>
                  </div>
                </div>
              ) : (
                <>
                  <div className="flex items-center gap-8 border-b border-border p-12">
                    <label className="inline-flex items-center gap-6 text-12 text-text-secondary">
                      <input
                        type="checkbox"
                        aria-label="Select all items"
                        checked={selected.size === items.length && items.length > 0}
                        onChange={(event) =>
                          setSelected(
                            event.target.checked ? new Set(items.map(itemKey)) : new Set(),
                          )
                        }
                      />
                      Select all
                    </label>
                    <span className="ml-auto text-12 text-text-muted">
                      {items.length} found · {selected.size} selected
                    </span>
                    <Button size="sm" disabled={busy} onClick={() => void startScan()}>
                      Rescan
                    </Button>
                  </div>
                  <div className="grid grid-cols-[28px_1.4fr_0.7fr_1.1fr_0.9fr] gap-8 border-b border-border px-12 py-8 text-11 uppercase tracking-wide text-text-muted">
                    <span aria-hidden="true" />
                    <span>Item</span>
                    <span>Type</span>
                    <span>Found in</span>
                    <span>State</span>
                  </div>
                  <ul className="m-0 list-none p-0">
                    {items.map((item) => {
                      const key = itemKey(item);
                      const state = itemState(item);
                      return (
                        <li
                          key={key}
                          className="grid grid-cols-[28px_1.4fr_0.7fr_1.1fr_0.9fr] items-start gap-8 border-b border-border px-12 py-10 last:border-0"
                        >
                          <input
                            type="checkbox"
                            aria-label={`Select ${item.name}`}
                            checked={selected.has(key)}
                            disabled={busy}
                            onChange={(event) =>
                              setSelected((current) => {
                                const next = new Set(current);
                                if (event.target.checked) next.add(key);
                                else next.delete(key);
                                return next;
                              })
                            }
                          />
                          <div className="min-w-0">
                            <p className="m-0 truncate text-13 font-semibold text-text" title={item.path}>
                              {item.name}
                            </p>
                            <div className="flex flex-wrap items-center gap-4">
                              <Badge tone={item.scope === "project" ? "warning" : item.scope === "global" ? "locked" : "info"} dot>
                                {item.scope}
                              </Badge>
                              {item.store_link && <Badge tone="locked">Symlink</Badge>}
                            </div>
                          </div>
                          <span className="text-12 text-text-secondary">
                            {item.kind === "mcp_server" ? "MCP" : item.kind === "skill" ? "Skill" : "Rule"}
                          </span>
                          <div className="min-w-0">
                            <p className="m-0 truncate text-12 text-text" title={item.agents.join(", ")}>
                              {item.agents.map((agent) => agent.replaceAll("_", " ")).join(", ")}
                            </p>
                            <span className="text-11 text-text-muted">native only</span>
                          </div>
                          <span className="inline-flex items-center gap-6 text-12">
                            <span
                              aria-hidden="true"
                              className={`size-8 rounded-full ${
                                state.tone === "success"
                                  ? "bg-success"
                                  : state.tone === "warning"
                                    ? "bg-warning"
                                    : state.tone === "danger"
                                      ? "bg-danger"
                                      : "bg-scope-user"
                              }`}
                            />
                            {state.label}
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                  <p className="m-0 mt-auto border-t border-border p-12 text-12 text-text-muted">
                    Nothing is moved yet. AgenTuna only read these files.
                  </p>
                </>
              )}
            </section>
          </div>

          <footer className="mt-auto flex items-center justify-end gap-12 border-t border-border pt-16">
            <Button
              variant="ghost"
              onClick={() => {
                if (kindFilter) {
                  app.setScreenState("migrate:kind", undefined);
                  app.navigate("mcp-list");
                } else {
                  setStep("scan");
                }
              }}
            >
              Cancel
            </Button>
            <Button
              variant="primary"
              disabled={busy || selected.size === 0}
              onClick={() => setStep("resolve")}
            >
              Continue with {selected.size}
            </Button>
          </footer>
        </>
      )}
    </main>
  );
}
