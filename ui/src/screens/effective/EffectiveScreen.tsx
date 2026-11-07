import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, Input } from "../../components";
import type { EffectiveCountsView } from "../../generated/EffectiveCountsView";
import type { EffectiveQueryView } from "../../generated/EffectiveQueryView";
import type { WhyTraceView } from "../../generated/WhyTraceView";
import { Modal } from "../../overlays/dialogs";
import { WhyTraceDialog } from "../../overlays/dialogs/WhyTraceDialog";
import { McpCopyDialog } from "../../overlays/dialogs/McpCopyDialog";
import { SkillActionDialogs } from "../../overlays/dialogs/SkillActionDialogs";
import type { SkillActionDialogRequest } from "../../overlays/dialogs/SkillActionDialogs";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";
import type { McpActionPreviewView } from "../../generated/McpActionPreviewView";
import type { McpServerActionOutcome } from "../../generated/McpServerActionOutcome";

type Kind = "skill" | "mcp_server" | "rule";

const TABS: readonly { readonly label: string; readonly kind: Kind }[] = [
  { label: "Skills", kind: "skill" },
  { label: "MCP servers", kind: "mcp_server" },
  { label: "Instructions", kind: "rule" },
];
const SCOPE_LABEL: Readonly<Record<string, string>> = {
  global: "Global",
  user: "User",
  project: "Project",
  local: "Local",
};
const SCOPE_TONE: Readonly<Record<string, "locked" | "info" | "warning" | "success">> = {
  global: "locked",
  user: "info",
  project: "warning",
  local: "success",
};
const SCOPES = ["global", "user", "project", "local"] as const;
const AGENT_NAME: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  codex: "Codex",
  omp: "omp",
  cursor: "Cursor",
};
function formatPath(path: string): string {
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

/** Human label for one scope's why-trace step. */
function stepLabel(scope: string, status: unknown): string {
  const text = typeof status === "string" ? status : Object.keys(status as object)[0] ?? "Unknown";
  if (text === "Winning") return "Defined, enabled ✓ wins";
  if (text === "Absent") return scope === "global" ? "Not defined" : "Not overridden";
  if (text === "Disabled") return "Disabled here";
  if (text === "Unreadable") return "Unreadable";
  return text;
}

function stepTone(status: unknown): "success" | "warning" | "danger" | "neutral" {
  const text = typeof status === "string" ? status : Object.keys(status as object)[0] ?? "";
  if (text === "Winning") return "success";
  if (text === "Disabled") return "warning";
  if (text === "Unreadable") return "danger";
  return "neutral";
}

/** Effective configuration: kind tabs, filterable table and a scope-trace detail panel. */
export function EffectiveScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const [tab, setTab] = useState<Kind>("skill");
  const [filter, setFilter] = useState("");
  const [counts, setCounts] = useState<EffectiveCountsView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<EffectiveQueryView["items"]>([]);
  const [disabledHere, setDisabledHere] = useState<EffectiveQueryView["disabled_here"]>([]);
  const [diagnostics, setDiagnostics] = useState<EffectiveQueryView["diagnostics"]>([]);
  const [loadDiagnostics, setLoadDiagnostics] = useState<EffectiveQueryView["load_diagnostics"]>([]);
  const [globalWritable, setGlobalWritable] = useState(false);
  const [globalReason, setGlobalReason] = useState("Global store is read-only");
  const [selectedLogicalId, setSelectedLogicalId] = useState<string | null>(null);
  const [trace, setTrace] = useState<WhyTraceView | null>(null);
  const [traceAgent, setTraceAgent] = useState<string | null>(null);
  const [traceError, setTraceError] = useState<string | null>(null);
  const [why, setWhy] = useState<{ itemId: string; agent: string } | null>(null);
  const [lockTarget, setLockTarget] = useState<{ itemId: string; name: string; locked: boolean; reason: string } | null>(null);
  const [lockReason, setLockReason] = useState("");
  const [lockBusy, setLockBusy] = useState(false);
  const [actionDialog, setActionDialog] = useState<SkillActionDialogRequest | null>(null);
  const [mcpCopy, setMcpCopy] = useState<{ itemId: string; scope: string } | null>(null);
  const [repoDisable, setRepoDisable] = useState<{
    readonly preview: McpActionPreviewView;
    readonly itemId: string;
    readonly scope: string;
    readonly name: string;
  } | null>(null);
  const [repoBusy, setRepoBusy] = useState(false);
  const [repoError, setRepoError] = useState<string | null>(null);
  const detailRef = useRef<HTMLElement | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [result, overview, nextCounts] = await Promise.all([
        invoke<EffectiveQueryView>("effective_query", { request: { scope, kind: tab, agent: null } }),
        invoke<{ global_store: { status: string; reason: string | null } }>("overview_load", {
          activeScope: scope,
        }),
        invoke<EffectiveCountsView>("effective_counts"),
      ]);
      setGlobalWritable(overview.global_store.status === "available");
      setGlobalReason(overview.global_store.reason ?? "Global store is read-only");
      setRows(result.items);
      setDisabledHere(result.disabled_here);
      setDiagnostics(result.diagnostics);
      setLoadDiagnostics(result.load_diagnostics);
      setCounts(nextCounts);
      setSelectedLogicalId((old) =>
        result.items.some((row) => row.logical_id === old)
          ? old
          : (result.items[0]?.logical_id ?? result.disabled_here[0]?.logical_id ?? null),
      );
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, [scope, tab]);

  useEffect(() => {
    void refresh();
  }, [refresh, activeProject]);

  const visibleRows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (needle.length === 0) return rows;
    return rows.filter(
      (row) =>
        row.name.toLowerCase().includes(needle) || row.source_path.toLowerCase().includes(needle),
    );
  }, [filter, rows]);
  const visibleDisabled = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (needle.length === 0) return disabledHere;
    return disabledHere.filter(
      (row) => row.name.toLowerCase().includes(needle) || row.scope.includes(needle),
    );
  }, [disabledHere, filter]);

  const selected = useMemo(
    () =>
      rows.find((row) => row.logical_id === selectedLogicalId) ??
      disabledHere.find((row) => row.logical_id === selectedLogicalId) ??
      null,
    [disabledHere, rows, selectedLogicalId],
  );
  const selectedDisabled = selected !== null && !rows.some((row) => row.logical_id === selected.logical_id);
  const selectedItemId = selected !== null && "item_id" in selected ? selected.item_id : null;
  const locked = selected !== null && "lock" in selected ? selected.lock : null;

  const loadTrace = useCallback(
    async (itemId: string, agent: string) => {
      setTraceError(null);
      setTraceAgent(agent);
      try {
        setTrace(await invoke<WhyTraceView>("why_trace", { request: { item_id: itemId, scope, agent } }));
      } catch (cause) {
        setTrace(null);
        setTraceError(String(cause));
      }
    },
    [scope],
  );

  useEffect(() => {
    if (selectedItemId === null) {
      setTrace(null);
      setTraceAgent(null);
      return;
    }
    const firstAgent = selected !== null && "agents" in selected
      ? selected.agents.find((agent) => agent.loads)?.agent ?? selected.agents[0]?.agent
      : undefined;
    if (firstAgent === undefined) {
      setTrace(null);
      setTraceAgent(null);
      return;
    }
    void loadTrace(selectedItemId, firstAgent);
  }, [loadTrace, selected, selectedItemId]);

  function closeLockDialog() {
    setLockTarget(null);
    queueMicrotask(() => detailRef.current?.focus());
  }

  async function applyLock(): Promise<void> {
    if (lockTarget === null) return;
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

  const countFor = (kind: Kind): number => {
    if (counts === null) return 0;
    const value = kind === "skill" ? counts.skills : kind === "mcp_server" ? counts.mcp_servers : counts.rules;
    return Number.isFinite(value) ? value : 0;
  };

  /** Opens the reviewed "Disable in this repo" plan for the selected item. */
  async function disableInRepo(item: EffectiveQueryView["items"][number]): Promise<void> {
    if (activeProject === null || item.item_id === null) return;
    if (item.kind === "skill") {
      setActionDialog({ kind: "disable-in-repo", itemId: item.item_id });
      return;
    }
    if (item.kind !== "mcp_server") return;
    setRepoBusy(true);
    setRepoError(null);
    setRepoDisable(null);
    try {
      const outcome = await invoke<McpServerActionOutcome>("mcp_server_disable", {
        request: {
          item_id: item.item_id,
          scope: item.winner_scope,
          project_id: activeProject,
          agent: null,
          in_repo: true,
          apply: false,
          preview_id: null,
        },
      });
      if (outcome.stage === "preview") {
        setRepoDisable({ preview: outcome.preview, itemId: item.item_id, scope: item.winner_scope, name: item.name });
      }
    } catch (cause) {
      setRepoError(String(cause));
    } finally {
      setRepoBusy(false);
    }
  }

  /** Applies the reviewed project tombstone after the user confirms the listed files. */
  async function applyRepoDisable(): Promise<void> {
    if (repoDisable === null) return;
    setRepoBusy(true);
    setRepoError(null);
    try {
      await invoke<McpServerActionOutcome>("mcp_server_disable", {
        request: {
          item_id: repoDisable.itemId,
          scope: repoDisable.scope,
          project_id: activeProject,
          agent: null,
          in_repo: true,
          apply: true,
          preview_id: repoDisable.preview.preview_id,
        },
      });
      toast.success(`Disabled ${repoDisable.name} in this repo`);
      setRepoDisable(null);
      await refresh();
    } catch (cause) {
      setRepoError(String(cause));
      setRepoDisable(null);
    } finally {
      setRepoBusy(false);
    }
  }
  const columnLabel = tab === "skill" ? "Skill" : tab === "mcp_server" ? "Server" : "File";
  const loadedAgents =
    selected !== null && "agents" in selected
      ? selected.agents
          .filter((agent) => agent.loads)
          .map((agent) => AGENT_NAME[agent.agent] ?? agent.agent.replaceAll("_", " "))
      : [];

  return (
    <main aria-label="Effective config" className="mx-auto flex max-w-[1180px] flex-col gap-16 p-24">
      <header>
        <h1 className="m-0 text-26 font-semibold text-text">Effective config</h1>
        <p className="m-0 mt-4 text-13 text-text-secondary">
          Everything your agents load in {SCOPE_LABEL[scope] ?? scope}, and which scope it came from.
        </p>
      </header>

      <div className="flex flex-wrap items-center justify-between gap-12">
        <div role="tablist" aria-label="Effective config categories" className="inline-flex items-center gap-4 rounded-8 border border-border bg-raised p-4">
          {TABS.map((entry) => (
            <button
              key={entry.kind}
              type="button"
              role="tab"
              aria-selected={tab === entry.kind}
              className={`inline-flex items-center gap-6 rounded-6 px-10 py-6 text-13 transition-colors max-[1199px]:gap-8 max-[1199px]:px-16 max-[1199px]:py-8 ${
                tab === entry.kind
                  ? "bg-overlay font-semibold text-text"
                  : "text-text-secondary hover:text-text"
              }`}
              onClick={() => setTab(entry.kind)}
            >
              {entry.label}
              <span className="font-mono text-11 text-text-muted">{countFor(entry.kind)}</span>
            </button>
          ))}
        </div>
        <Input
          aria-label="Filter effective items"
          placeholder="Filter"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          className="w-256"
        />
      </div>

      {loading ? (
        <p role="status" className="text-13 text-text-secondary">
          Resolving effective configuration…
        </p>
      ) : error !== null ? (
        <div role="alert" className="rounded-8 border border-danger/40 bg-danger/10 p-12">
          <p className="m-0 text-13 text-danger">{error}</p>
          <Button size="sm" className="mt-8" onClick={() => void refresh()}>
            Retry
          </Button>
        </div>
      ) : rows.length === 0 && disabledHere.length === 0 ? (
        <section className="rounded-10 border border-border bg-raised p-24">
          <h2 className="m-0 text-16 font-semibold">Nothing configured at this scope</h2>
          <p className="mt-8 text-13 text-text-secondary">
            Add a {columnLabel.toLowerCase()} in {SCOPE_LABEL[scope] ?? scope} or a broader scope to see it here.
          </p>
        </section>
      ) : (
        <div className="grid min-h-[560px] grid-cols-[1.05fr_1fr] gap-16">
          <section aria-label="Effective items" className="flex flex-col rounded-10 border border-border bg-raised">
            <div className="grid grid-cols-[1.3fr_0.9fr_0.5fr] gap-8 border-b border-border px-12 py-8 text-11 uppercase tracking-wide text-text-muted">
              <span>{columnLabel}</span>
              <span>From</span>
              <span className="text-right">Agents</span>
            </div>
            <ul className="m-0 list-none p-0">
              {visibleRows.map((row) => {
                const active = row.logical_id === selectedLogicalId;
                return (
                  <li key={row.logical_id}>
                    <button
                      type="button"
                      aria-pressed={active}
                      className={`grid w-full grid-cols-[1.3fr_0.9fr_0.5fr] items-center gap-8 border-b border-border px-12 py-10 text-left transition-colors focus-visible:outline-2 focus-visible:outline-focus ${
                        active ? "border-l-2 border-l-accent bg-overlay" : "border-l-2 border-l-transparent hover:bg-overlay/60"
                      }`}
                      onClick={() => setSelectedLogicalId(row.logical_id)}
                    >
                      <span className="min-w-0 truncate text-13 font-semibold text-text" title={row.source_path}>
                        {row.name}
                      </span>
                      <Badge className="justify-self-start" tone={row.lock === null ? (SCOPE_TONE[row.winner_scope] ?? "neutral") : "locked"} dot>
                        {SCOPE_LABEL[row.winner_scope] ?? row.winner_scope}
                        {row.lock === null ? "" : " · locked"}
                      </Badge>
                      <span className="text-right font-mono text-12 text-text-secondary">
                        {row.agents.filter((agent) => agent.loads).length}/{row.agents.length}
                      </span>
                    </button>
                  </li>
                );
              })}
              {visibleDisabled.map((row) => {
                const active = row.logical_id === selectedLogicalId;
                return (
                  <li key={`disabled-${row.logical_id}-${row.agent ?? "all"}`}>
                    <button
                      type="button"
                      aria-pressed={active}
                      className={`grid w-full grid-cols-[1.3fr_0.9fr_0.5fr] items-center gap-8 border-b border-border px-12 py-10 text-left opacity-70 transition-colors focus-visible:outline-2 focus-visible:outline-focus ${
                        active ? "border-l-2 border-l-warning bg-overlay" : "border-l-2 border-l-transparent hover:bg-overlay/60"
                      }`}
                      onClick={() => setSelectedLogicalId(row.logical_id)}
                    >
                      <span className="min-w-0 truncate text-13 text-text-secondary line-through" title={row.reason ?? undefined}>
                        {row.name}
                      </span>
                      <Badge className="justify-self-start" tone="muted" dot>
                        {SCOPE_LABEL[row.scope] ?? row.scope}
                      </Badge>
                      <span className="text-right text-11 text-warning">Disabled here</span>
                    </button>
                  </li>
                );
              })}
              {visibleRows.length === 0 && visibleDisabled.length === 0 && (
                <li className="px-12 py-16 text-13 text-text-muted">No items match “{filter}”.</li>
              )}
            </ul>
            {visibleDisabled.length > 0 && (
              <p className="m-0 mt-auto border-t border-border p-12 text-12 text-text-muted">
                {visibleDisabled.map((row) => row.name).join(", ")}{" "}
                {visibleDisabled.length === 1 ? "is" : "are"} defined in a broader scope and turned off for this
                project.
              </p>
            )}
          </section>

          <section
            ref={detailRef}
            tabIndex={-1}
            aria-label="Effective item detail"
            className="flex flex-col gap-12 rounded-10 border border-border bg-raised p-16 focus-visible:outline-2 focus-visible:outline-focus"
          >
            {selected === null ? (
              <p className="m-0 text-13 text-text-secondary">Select an item to see why it is on.</p>
            ) : (
              <>
                <div>
                  <div className="flex items-start justify-between gap-8">
                    <h2 className="m-0 text-18 font-semibold text-text">{selected.name}</h2>
                    {selectedItemId !== null && scope === "global" && (
                      locked === null ? (
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={!globalWritable}
                          title={globalWritable ? undefined : globalReason}
                          onClick={() => {
                            setLockReason("");
                            setLockTarget({ itemId: selectedItemId, name: selected.name, locked: false, reason: "" });
                          }}
                        >
                          Lock
                        </Button>
                      ) : (
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={() => {
                            setLockTarget({
                              itemId: selectedItemId,
                              name: selected.name,
                              locked: true,
                              reason: locked.reason ?? "",
                            });
                          }}
                        >
                          Unlock
                        </Button>
                      )
                    )}
                  </div>
                  <p className="m-0 mt-4 flex items-center gap-6 text-12 text-text-secondary">
                    <span aria-hidden="true" className={`size-8 rounded-full ${selectedDisabled ? "bg-warning" : "bg-success"}`} />
                    {selectedDisabled
                      ? "Defined in a broader scope and disabled for this project"
                      : loadedAgents.length === 0
                        ? "No agent loads this item at this scope"
                        : `Loaded for ${loadedAgents.join(", ")}`}
                  </p>
                  {locked !== null && (
                    <p className="m-0 mt-4 text-12 text-warning">🔒 Locked: {locked.reason}</p>
                  )}
                </div>

                {!selectedDisabled && selectedItemId !== null && "agents" in selected && (
                  <>
                    <h3 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Why is this on?
                    </h3>
                    {traceError !== null ? (
                      <p role="alert" className="m-0 text-12 text-danger">
                        {traceError}
                      </p>
                    ) : trace === null || !trace.trace ? (
                      <p role="status" className="m-0 text-12 text-text-muted">
                        Resolving the scope trace…
                      </p>
                    ) : (
                      <ol className="m-0 flex list-none flex-col gap-2 p-0">
                        {SCOPES.map((entry) => {
                          const step = trace.trace.steps?.find((candidate) => candidate.scope === entry);
                          const winning = trace.trace.verdict?.on && trace.trace.steps?.[trace.trace.verdict.decisive_step ?? 0]?.scope === entry;
                          return (
                            <li
                              key={entry}
                              className={`flex items-center justify-between gap-8 rounded-6 border px-8 py-6 ${
                                winning ? "border-success/50 bg-success/10" : "border-transparent"
                              }`}
                            >
                              <span className="inline-flex items-center gap-6 text-13 text-text">
                                <span
                                  aria-hidden="true"
                                  className={`size-8 rounded-full ${
                                    winning
                                      ? "bg-success"
                                      : step === undefined
                                        ? "bg-border"
                                        : stepTone(step.status) === "warning"
                                          ? "bg-warning"
                                          : "bg-border"
                                  }`}
                                />
                                {SCOPE_LABEL[entry] ?? entry}
                              </span>
                              <span className="text-right text-11 text-text-muted">
                                {step === undefined ? "No trace step" : stepLabel(entry, step.status)}
                              </span>
                            </li>
                          );
                        })}
                      </ol>
                    )}
                    <div className="flex flex-wrap items-center gap-8">
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => {
                          if (selectedItemId === null) return;
                          const agent = traceAgent ?? selected.agents[0]?.agent;
                          if (agent !== undefined) {
                            setWhy({ itemId: selectedItemId, agent });
                          }
                        }}
                      >
                        Why is this on?
                      </Button>
                      <span className="min-w-0 flex-1 truncate font-mono text-11 text-text-muted" title={selected.source_path}>
                        {selected.source_path}
                      </span>
                    </div>
                    <h3 className="m-0 mt-4 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Per agent
                    </h3>
                    <div className="grid grid-cols-3 gap-8">
                      {selected.agents.map((agent) => (
                        <div key={agent.agent} className="rounded-6 border border-border p-8">
                          <p className="m-0 truncate text-12 font-semibold text-text">
                            {AGENT_NAME[agent.agent] ?? agent.agent.replaceAll("_", " ")}
                          </p>
                          <p className="m-0 truncate font-mono text-11 text-text-muted" title={agent.path ?? undefined}>
                            {agent.path ? formatPath(agent.path) : agent.state}
                          </p>
                          <p className={`m-0 text-11 ${agent.loads ? "text-success" : "text-text-muted"}`}>
                            {agent.loads ? (agent.path ? agent.state : "loaded") : "not loaded"}
                          </p>
                        </div>
                      ))}
                    </div>
                    {activeProject !== null && selectedItemId !== null && selected.winner_scope !== "project" && selected.winner_scope !== "local" && (
                      <section aria-label="Change it for this repo" className="mt-4 grid gap-8 rounded-8 border border-border bg-surface p-12">
                        <div>
                          <h3 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">Change it for this repo</h3>
                          <p className="mb-0 mt-4 text-11 text-text-muted">
                            {selected.kind === "mcp_server"
                              ? `Writes the project MCP configuration. Your ${SCOPE_LABEL[selected.winner_scope] ?? selected.winner_scope} definition stays the same.`
                              : selected.kind === "skill"
                                ? `Writes a Project-scope copy. Your ${SCOPE_LABEL[selected.winner_scope] ?? selected.winner_scope} copy stays the same.`
                                : `Manage this rule in this repo or edit the original ${SCOPE_LABEL[selected.winner_scope] ?? selected.winner_scope} rule.`}
                          </p>
                        </div>
                        <div className="flex flex-wrap items-center gap-8">
                          {selected.kind !== "rule" && (
                            <>
                              <Button
                                size="sm"
                                variant="secondary"
                                disabled={locked !== null || repoBusy}
                                title={locked === null ? undefined : locked.reason}
                                onClick={() => void disableInRepo(selected)}
                              >
                                {repoBusy ? "Reviewing…" : "Disable in this repo"}
                              </Button>
                              {selected.kind === "skill" ? (
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  disabled={locked !== null}
                                  title={locked === null ? undefined : locked.reason}
                                  onClick={() => setActionDialog({ kind: "transfer", itemId: selectedItemId, transfer: "promote", targetScope: "project" })}
                                >
                                  Override in Project
                                </Button>
                              ) : (
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  disabled={locked !== null}
                                  title={locked === null ? undefined : locked.reason}
                                  onClick={() => {
                                    if (selectedItemId === null) return;
                                    setMcpCopy({ itemId: selectedItemId, scope: selected.winner_scope });
                                  }}
                                >
                                  Override in Project
                                </Button>
                              )}
                            </>
                          )}
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={
                              locked !== null ||
                              (selected.winner_scope === "global" && !globalWritable)
                            }
                            title={
                              locked !== null
                                ? locked.reason
                                : selected.winner_scope === "global" && !globalWritable
                                  ? globalReason
                                  : undefined
                            }
                            aria-label="Edit User definition"
                            onClick={() => {
                              app.openItem(selectedItemId, {
                                kind: selected.kind,
                                 scope: selected.winner_scope,
                                project_id: selected.winner_scope === "project" ? activeProject : null,
                              });
                            }}
                          >
                            Edit User definition
                          </Button>
                        </div>
                        {locked !== null && <p className="m-0 text-11 text-warning">Locked: {locked.reason}</p>}
                        {repoError !== null && <p role="alert" className="m-0 text-11 text-danger">{repoError}</p>}
                      </section>
                    )}
                  </>
                )}
              </>
            )}
            {(diagnostics.length > 0 || loadDiagnostics.length > 0) && (
              <section aria-label="Configuration diagnostics" className="mt-auto border-t border-border pt-8">
                <h3 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">Diagnostics</h3>
                <ul className="m-0 list-none p-0 text-12 text-text-secondary">
                  {diagnostics.map((item, index) => (
                    <li key={`resolution-${index}`}>{item.message}</li>
                  ))}
                  {loadDiagnostics.map((item, index) => (
                    <li key={`load-${index}`}>{item.message}</li>
                  ))}
                </ul>
              </section>
            )}
          </section>
        </div>
      )}

      <Modal
        open={repoDisable !== null}
        onOpenChange={(open) => { if (!open && !repoBusy) setRepoDisable(null); }}
        title={`Disable ${repoDisable?.name ?? "server"} in this repo?`}
        description="Every listed agent configuration is backed up before the project tombstone is written."
        footer={<><Button variant="ghost" disabled={repoBusy} onClick={() => setRepoDisable(null)}>Cancel</Button><Button variant="primary" disabled={repoBusy} onClick={() => void applyRepoDisable()}>{repoBusy ? "Disabling…" : "Disable in repo"}</Button></>}
      >
        <ul className="m-0 grid max-h-[320px] list-none gap-6 overflow-auto p-0 text-12 text-text-secondary">
          {(repoDisable?.preview.files ?? []).map((file) => (
            <li key={file.path} className="break-all font-mono text-11">{file.agent}: {file.path}</li>
          ))}
        </ul>
      </Modal>
      <SkillActionDialogs
        action={actionDialog}
        projectId={activeProject}
        onClose={() => setActionDialog(null)}
        onTransferSuccess={(result) => {
          setActionDialog(null);
          toast.success(`Project copy written. Backup: ${result.backup_location}`);
          void refresh();
        }}
        onDeleteSuccess={() => setActionDialog(null)}
        onDisableSuccess={(result) => {
          setActionDialog(null);
          toast.success(`Disabled in this repo for ${result.agents_disabled} agent(s).`);
          void refresh();
        }}
      />
      {mcpCopy !== null && (
        <McpCopyDialog
          itemId={mcpCopy.itemId}
          scope={mcpCopy.scope}
          projectId={activeProject}
          targetScope="project"
          onClose={() => setMcpCopy(null)}
          onSuccess={(result) => {
            setMcpCopy(null);
            toast.success(`Copied into Project as ${result.target_slug}. Backup: ${result.backup_location}`);
            void refresh();
          }}
        />
      )}
      {why !== null && (
        <WhyTraceDialog itemId={why.itemId} scope={scope} agent={why.agent} onClose={() => setWhy(null)} />
      )}
      {lockTarget !== null && (
        <div
          className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) closeLockDialog();
          }}
        >
          <section
            role="dialog"
            aria-modal="true"
            aria-labelledby="effective-lock-title"
            className="w-full max-w-[420px] rounded-12 border border-border bg-surface p-20"
            onKeyDown={(event) => {
              if (event.key === "Escape" && !lockBusy) closeLockDialog();
            }}
          >
            <h2 id="effective-lock-title" className="m-0 text-18 font-semibold">
              {lockTarget.locked ? `Unlock ${lockTarget.name}?` : `Lock ${lockTarget.name}`}
            </h2>
            {lockTarget.locked ? (
              <p className="text-13 text-text-secondary">
                Remove the Global lock? Lower-scope changes will be allowed again.
              </p>
            ) : (
              <label className="mt-12 grid gap-6 text-13">
                Reason
                <input
                  autoFocus
                  maxLength={200}
                  value={lockReason}
                  onChange={(event) => setLockReason(event.target.value)}
                  aria-label="Lock reason"
                  className="rounded-8 border border-border bg-surface px-10 py-8"
                />
              </label>
            )}
            <div className="mt-16 flex justify-end gap-8">
              <Button variant="ghost" disabled={lockBusy} onClick={closeLockDialog}>
                Cancel
              </Button>
              <Button
                variant="primary"
                disabled={
                  lockBusy ||
                  (!lockTarget.locked && (lockReason.trim().length === 0 || lockReason.trim().length > 200))
                }
                onClick={() => void applyLock()}
              >
                {lockBusy ? "Saving…" : lockTarget.locked ? "Unlock" : "Lock item"}
              </Button>
            </div>
          </section>
        </div>
      )}
    </main>
  );
}
