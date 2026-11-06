import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button } from "../../components";
import type { MigrateConflictView } from "../../generated/MigrateConflictView";
import type { MigratePlanView } from "../../generated/MigratePlanView";
import type { MigrateScanItemView } from "../../generated/MigrateScanItemView";
import type { MigrateScanView } from "../../generated/MigrateScanView";
import { useAppSnapshot } from "../../state/context";
import { MigratePlanScreen } from "./MigratePlanScreen";
import { MigrateResolveScreen, resolutionPayload } from "./MigrateResolveScreen";
import type { ConflictChoices, ConflictNames } from "./MigrateResolveScreen";

type Step = "scan" | "resolve" | "plan";
type Strategy = "replace_with_links" | "leave_and_watch";

const STEPS: readonly { readonly id: Step | "done"; readonly label: string }[] = [
  { id: "scan", label: "Scan" },
  { id: "resolve", label: "Resolve" },
  { id: "plan", label: "Plan" },
  { id: "done", label: "Done" },
];

function itemKey(item: MigrateScanItemView): string {
  return `${item.kind}:${item.path}:${item.content_hash ?? ""}`;
}

/** Short state label for one scanned row. */
function itemState(item: MigrateScanItemView): { readonly label: string; readonly tone: "success" | "warning" | "danger" | "info" } {
  if (item.parse_error !== null) return { label: "Unreadable", tone: "danger" };
  if (item.ownership === "managed" || item.store_link) return { label: "In sync", tone: "success" };
  if (item.agents.length > 1) return { label: "Check copies", tone: "warning" };
  return { label: "Ready", tone: "success" };
}

/** Migration wizard: scan, resolve conflicts, then the reviewed plan. */
export function MigrateScanScreen() {
  const { activeProject } = useAppSnapshot();
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

  const scan = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<MigrateScanView>("migrate_scan", { projectId: activeProject });
      const keys = new Set(next.items.map(itemKey));
      const retained = new Set([...selected].filter((key) => keys.has(key)));
      setStaleSelectionCount(selected.size - retained.size);
      setSelected(retained);
      setReport(next);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [activeProject, selected]);

  useEffect(() => {
    void scan();
    // The initial scan runs once per project; reselection is a user action.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProject]);

  const items = report?.items ?? [];
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
      setPlanError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  const stepIndex = step === "scan" ? 0 : step === "resolve" ? 1 : 2;

  return (
    <main aria-label="Move into AgenTuna" className="mx-auto flex min-h-full max-w-[1180px] flex-col gap-16 p-24">
      {step !== "scan" ? null : (
        <header className="flex flex-col gap-4">
          <p className="m-0 text-12 uppercase tracking-wide text-text-muted">Migrate to AgenTuna</p>
          <h1 className="m-0 text-26 font-semibold text-text">
            {items.length === 0
              ? "No items outside AgenTuna"
              : `Found ${items.length} item${items.length === 1 ? "" : "s"} outside AgenTuna`}
          </h1>
          <p className="m-0 text-13 text-text-secondary">
            They live only in agent folders. Pick what to move into the AgenTuna store.
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
                      ? "border-accent bg-accent text-surface"
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
          onBack={() => setStep("resolve")}
          onCancel={() => setStep("scan")}
        />
      ) : (
        <>
          {busy && (
            <p role="status" className="m-0 text-13 text-text-secondary">
              Scanning agent folders…
            </p>
          )}
          {error !== null && (
            <div role="alert" className="flex items-center justify-between gap-12 rounded-8 border border-danger/40 bg-danger/10 p-12">
              <p className="m-0 text-13 text-danger">{error}</p>
              <Button size="sm" onClick={() => void scan()}>
                Retry
              </Button>
            </div>
          )}
          {report?.partial === true && (
            <p role="status" className="m-0 text-12 text-warning">
              Some agent folders could not be read; found items are still available.
            </p>
          )}
          {staleSelectionCount > 0 && (
            <p role="status" className="m-0 text-12 text-warning">
              {staleSelectionCount} selection{staleSelectionCount === 1 ? "" : "s"} changed or disappeared and
              were cleared.
            </p>
          )}

          <div className="grid min-h-[520px] grid-cols-[320px_1fr] gap-16">
            <aside aria-label="Scanned folders" className="rounded-10 border border-border bg-raised p-12">
              <h2 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">
                Scanned folders
              </h2>
              {report === null || report.roots.length === 0 ? (
                <p className="m-0 mt-8 text-13 text-text-muted">Nothing scanned yet.</p>
              ) : (
                <ul className="m-0 mt-8 flex list-none flex-col gap-10 p-0">
                  {report.roots.map((root) => (
                    <li key={root.path}>
                      <p className="m-0 truncate font-mono text-12 text-text" title={root.path}>
                        {root.path}
                      </p>
                      <div className="flex items-center justify-between gap-6">
                        <span className="text-11 text-text-muted">{root.agents.join(", ")}</span>
                        <span className={`text-11 ${root.new_items > 0 ? "text-warning" : "text-success"}`}>
                          {root.new_items > 0 ? `${root.new_items} new` : `${root.in_sync} in sync`}
                        </span>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
              {report !== null && report.skipped_roots.length > 0 && (
                <div className="mt-12 border-t border-border pt-8">
                  <h3 className="m-0 text-11 uppercase tracking-wide text-text-muted">Not found</h3>
                  <ul className="m-0 mt-4 list-none p-0">
                    {report.skipped_roots.map((path) => (
                      <li key={path} className="truncate font-mono text-11 text-text-muted" title={path}>
                        {path}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </aside>

            <section aria-label="Scanned items" className="flex flex-col rounded-10 border border-border bg-raised">
              {items.length === 0 ? (
                <div className="p-24">
                  <h2 className="m-0 text-16 font-semibold">Nothing to migrate</h2>
                  <p className="mt-8 text-13 text-text-secondary">
                    No native items were found in agent folders.
                  </p>
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
                            <Badge tone={item.scope === "project" ? "warning" : item.scope === "global" ? "locked" : "info"} dot>
                              {item.scope}
                            </Badge>
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

          <footer className="mt-auto flex items-center justify-between gap-8 border-t border-border pt-16">
            <Button variant="ghost" onClick={() => setStep("scan")}>
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
