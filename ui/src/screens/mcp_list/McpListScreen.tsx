/**
 * MCP servers list screen (TM-020) with reviewed row actions (TM-025) and Global lock controls (TA-097).
 *
 * Virtualized DataTable with per-agent matrix, health status, six filter chips
 * (All, Global, User, Project, Not in AgenTuna, Failing) with live counts, debounced filter box,
 * native row adopt/resolve actions, TC-025 Not in AgenTuna banner, and row actions menu.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ColumnDef } from "@tanstack/react-table";
import { Button, Input, Badge, Chip } from "../../components";
import { useCompactLayout } from "../../components/use-compact-layout";
import { DataTable } from "../../components/table";
import { agentColumns } from "../../components/table";
import type { AgentCellState, AgentDescriptor } from "../../components/table";
import type { DataTableFeatures } from "../../components/table/features";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { McpActionPreviewView } from "../../generated/McpActionPreviewView";
import type { McpServerActionOutcome } from "../../generated/McpServerActionOutcome";
import type { McpServerActionRequest } from "../../generated/McpServerActionRequest";
import type { McpServerListView } from "../../generated/McpServerListView";
import type { McpServerRowView } from "../../generated/McpServerRowView";
import type { McpServerTestView } from "../../generated/McpServerTestView";
import { Modal, WhyTraceDialog } from "../../overlays";
import { AdoptModal } from "../../overlays/dialogs/AdoptModal";
import { McpServerActions } from "../../overlays/menus";
import type { McpServerAction } from "../../overlays/menus";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";

/** A command rejection as the toast service needs it. */
function commandError(cause: unknown): { code: string; message: string } {
  if (typeof cause === "object" && cause !== null && "code" in cause && "message" in cause) {
    const value = cause as { code: unknown; message: unknown };
    return { code: String(value.code), message: String(value.message) };
  }
  return { code: "error", message: String(cause) };
}

export type McpFilterChip = "all" | "global" | "user" | "project" | "native" | "failing";

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
  local: "bg-scope-project/15 text-scope-project",
};

/** Which dialog the screen is showing. */
type DialogState =
  | { readonly kind: "disable-repo"; readonly row: McpServerRowView }
  | { readonly kind: "disable-agent"; readonly row: McpServerRowView; readonly agent: string }
  | { readonly kind: "enable-agent"; readonly row: McpServerRowView; readonly agent: string }
  | { readonly kind: "delete"; readonly row: McpServerRowView }
  | { readonly kind: "lock"; readonly row: McpServerRowView }
  | { readonly kind: "unlock"; readonly row: McpServerRowView }
  | null;

/** Column labels for the known agents; unknown ids fall back to the detected name. */
const AGENT_LABEL: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  codex: "Codex",
  omp: "omp",
  cursor: "Cursor",
};

/** Display order used by the design for the per-agent columns. */
const AGENT_ORDER: readonly string[] = ["claude_code", "codex", "omp", "cursor"];

/** Health presentation: text plus a tone dot, never colour alone. */
const HEALTH_META: Readonly<Record<string, { readonly label: string; readonly tone: "success" | "danger" | "muted" }>> = {
  ok: { label: "Healthy", tone: "success" },
  failed: { label: "Failing", tone: "danger" },
  unknown: { label: "Not tested", tone: "muted" },
};

/** Capitalised scope name for lock tooltips and status text. */
function scopeLabel(scope: string | null): string {
  if (scope === null) return "Global";
  return scope.charAt(0).toUpperCase() + scope.slice(1);
}

/** MCP server list with per-agent matrix, health status, filter chips, and row actions. */
export function McpListScreen({
  localEnabled: _localEnabled = false,
  globalLocked: _globalLocked = false,
}: {
  readonly localEnabled?: boolean;
  readonly globalLocked?: boolean;
} = {}) {
  const { scope, activeProject, externalRefreshVersion } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();

  const [list, setList] = useState<McpServerListView | null>(null);
  const [agentCards, setAgentCards] = useState<readonly AgentCardView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [activeChip, setActiveChip] = useState<McpFilterChip>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [bannerHidden, setBannerHidden] = useState(false);
  const [adoptModalTarget, setAdoptModalTarget] = useState<McpServerRowView | null>(null);

  const [menuRow, setMenuRow] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState>(null);
  const [preview, setPreview] = useState<McpActionPreviewView | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [typedName, setTypedName] = useState("");
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [why, setWhy] = useState<{ itemId: string; agent: string } | null>(null);
  /** Below the breakpoint the Health column leaves the model. */
  const compactLayout = useCompactLayout();

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [view, settingsResult, cards] = await Promise.all([
        invoke<McpServerListView>("mcp_server_list", {
          request: { scope: "", project_id: activeProject },
        }),
        invoke<Record<string, unknown>>("settings_load").catch(() => null),
        invoke<AgentCardView[]>("agents_list").catch(() => [] as AgentCardView[]),
      ]);
      setList(view);
      setAgentCards(Array.isArray(cards) ? cards : []);
      if (settingsResult !== null) {
        setBannerHidden(settingsResult["native_banner_hidden.mcp_server"] === true);
      }
    } catch (cause) {
      setError(commandError(cause).message);
    } finally {
      setLoading(false);
    }
  }, [activeProject, externalRefreshVersion]);

  useEffect(() => {
    void load();
  }, [load]);

  const closeDialog = useCallback(() => {
    setDialog(null);
    setPreview(null);
    setPreviewError(null);
    setTypedName("");
    setReason("");
  }, []);

  const requestPreview = useCallback(
    async (
      command: "mcp_server_disable" | "mcp_server_delete" | "mcp_server_enable",
      request: McpServerActionRequest,
    ) => {
      setPreview(null);
      setPreviewError(null);
      try {
        const outcome = await invoke<McpServerActionOutcome>(command, {
          request: { ...request, apply: false, preview_id: null },
        });
        if (outcome.stage === "preview") setPreview(outcome.preview);
      } catch (cause) {
        setPreviewError(commandError(cause).message);
      }
    },
    [],
  );

  const applyAction = useCallback(
    async (
      command: "mcp_server_disable" | "mcp_server_delete" | "mcp_server_enable",
      request: McpServerActionRequest,
      name: string,
    ) => {
      if (preview === null) return;
      setBusy(true);
      try {
        await invoke<McpServerActionOutcome>(command, {
          request: { ...request, apply: true, preview_id: preview.preview_id },
        });
        toast.success(
          command === "mcp_server_delete"
            ? `Deleted ${name}`
            : command === "mcp_server_enable"
              ? `Enabled ${name} for ${request.agent ?? "this agent"}`
              : request.in_repo
                ? `Disabled ${name} in this repo`
                : `Disabled ${name} for ${request.agent ?? "this agent"}`,
        );
        closeDialog();
        await load();
      } catch (cause) {
        setPreviewError(commandError(cause).message);
      } finally {
        setBusy(false);
      }
    },
    [closeDialog, load, preview, toast],
  );

  /**
   * Opens the reviewed enable/disable plan for one agent switch.
   *
   * A switch never writes by itself: the typed command returns a preview of
   * the exact agent files, and only the dialog's confirm applies it.
   */
  const requestAgentChange = useCallback(
    (row: McpServerRowView, agent: AgentDescriptor, enabled: boolean) => {
      if (row.id === "") {
        toast.info(`${row.name} must be adopted into AgenTuna before it can change.`);
        return;
      }
      if (row.locked) {
        toast.warning(`${row.name} is locked: ${row.lock_reason ?? "locked"}`, { title: "Locked" });
        return;
      }
      setDialog({ kind: enabled ? "enable-agent" : "disable-agent", row, agent: agent.id });
      void requestPreview(enabled ? "mcp_server_enable" : "mcp_server_disable", {
        item_id: row.id,
        scope: row.scope,
        project_id: activeProject,
        agent: agent.id,
        in_repo: false,
        apply: false,
        preview_id: null,
      });
    },
    [activeProject, requestPreview, toast],
  );

  const openAction = useCallback(
    (row: McpServerRowView, action: McpServerAction) => {
      setMenuRow(null);
      const base: McpServerActionRequest = {
        item_id: row.id,
        scope: row.scope,
        project_id: activeProject,
        agent: null,
        in_repo: false,
        apply: false,
        preview_id: null,
      };
      switch (action) {
        case "adopt":
          setAdoptModalTarget(row);
          return;
        case "open": {
          const detailId = row.id !== "" ? row.id : `native:${row.name}`;
          app.recordVisitedItem(`server:${detailId}`);
          app.setSelection("mcp-detail", [detailId]);
          app.navigate("mcp-detail");
          return;
        }
        case "edit":
          app.setSelection("mcp-form", [row.id]);
          app.navigate("mcp-form");
          return;
        case "why":
          setWhy({ itemId: row.id, agent: row.agents[0]?.agent ?? "claude_code" });
          return;
        case "test":
          void invoke<McpServerTestView>("mcp_server_test", {
            request: { item_id: row.id, scope: row.scope, project_id: activeProject },
          }).then(
            (result) =>
              toast.success(
                `${row.name}: ${result.tools_count} tool(s) in ${result.latency_ms} ms`,
                { title: "Connection OK" },
              ),
            (cause) => {
              const failure = commandError(cause);
              toast.warning(failure.message, { title: "Test failed" });
            },
          );
          return;
        case "disable-repo":
          setDialog({ kind: "disable-repo", row });
          void requestPreview("mcp_server_disable", { ...base, in_repo: true });
          return;
        case "disable-agent": {
          const agent = row.agents.find((cell) => cell.enabled)?.agent;
          if (agent === undefined) {
            toast.info(`${row.name} is not enabled for any agent`);
            return;
          }
          setDialog({ kind: "disable-agent", row, agent });
          void requestPreview("mcp_server_disable", { ...base, agent });
          return;
        }
        case "delete":
          setDialog({ kind: "delete", row });
          void requestPreview("mcp_server_delete", base);
          return;
        case "lock":
          setDialog({ kind: "lock", row });
          return;
        case "unlock":
          setDialog({ kind: "unlock", row });
          return;
        default:
          return;
      }
    },
    [activeProject, app, requestPreview, toast],
  );

  const applyLock = useCallback(async () => {
    if (dialog === null || (dialog.kind !== "lock" && dialog.kind !== "unlock")) return;
    const { row } = dialog;
    setBusy(true);
    try {
      if (dialog.kind === "lock") {
        await invoke("effective_lock", {
          request: { item_id: row.id, scope: "global", reason: reason.trim() },
        });
        toast.success(`Locked ${row.name}`);
      } else {
        await invoke("effective_unlock", {
          request: { item_id: row.id, scope: "global" },
        });
        toast.success(`Unlocked ${row.name}`);
      }
      closeDialog();
      await load();
    } catch (cause) {
      const failure = commandError(cause);
      toast.warning(failure.message, { title: "Lock change failed" });
    } finally {
      setBusy(false);
    }
  }, [dialog, reason, toast, closeDialog, load]);

  const handleHideBanner = useCallback(async () => {
    try {
      await invoke("settings_set", { key: "native_banner_hidden.mcp_server", value: true });
      setBannerHidden(true);
    } catch {
      setBannerHidden(true);
    }
  }, []);

  const allServers = useMemo(() => list?.servers ?? [], [list]);

  // All six chip counts computed from the exact same row set loaded by the query
  const counts = useMemo(() => {
    return {
      all: allServers.length,
      global: allServers.filter((s) => s.scope === "global").length,
      user: allServers.filter((s) => s.scope === "user").length,
      project: allServers.filter((s) => s.scope === "project").length,
      native: allServers.filter((s) => s.native).length,
      // Locked servers are excluded from failing count (TM-033 contract)
      failing: allServers.filter((s) => s.health === "failed" && !s.locked).length,
    };
  }, [allServers]);

  const filteredServers = useMemo(() => {
    let servers = allServers;
    if (activeChip === "global") servers = servers.filter((s) => s.scope === "global");
    else if (activeChip === "user") servers = servers.filter((s) => s.scope === "user");
    else if (activeChip === "project") servers = servers.filter((s) => s.scope === "project");
    else if (activeChip === "native") servers = servers.filter((s) => s.native);
    else if (activeChip === "failing") servers = servers.filter((s) => s.health === "failed" && !s.locked);

    const query = searchQuery.trim().toLowerCase();
    if (query !== "") {
      servers = servers.filter((s) => {
        return (
          s.name.toLowerCase().includes(query) ||
          s.transport.toLowerCase().includes(query) ||
          (s.source_path !== null && s.source_path.toLowerCase().includes(query))
        );
      });
    }
    return servers;
  }, [allServers, activeChip, searchQuery]);

  // Columns come from detection, so an installed agent always gets a switch even when no
  // server currently targets it. Row cells fill in the rest for agents detection missed.
  const detectedAgents = useMemo<readonly AgentDescriptor[]>(() => {
    const ids = new Set<string>();
    const installed = agentCards.filter((c) => c.installed || c.managed || c.presence !== "not_installed");
    const pool = installed.length > 0 ? installed : agentCards;
    for (const card of pool) ids.add(card.id);
    for (const server of allServers) {
      for (const cell of server.agents) {
        if (cell.state !== "not_installed" && cell.state !== "none") ids.add(cell.agent);
      }
    }
    if (ids.size === 0) for (const id of AGENT_ORDER) ids.add(id);
    const ordered = AGENT_ORDER.filter((id) => ids.has(id));
    for (const id of ids) {
      if (!ordered.includes(id)) ordered.push(id);
    }
    return ordered.map((id) => ({
      id,
      label: AGENT_LABEL[id] ?? agentCards.find((card) => card.id === id)?.name ?? id,
    }));
  }, [agentCards, allServers]);

  /**
   * Maps one MCP agent cell onto the shared cell vocabulary.
   *
   * Locked and native cells render a disabled switch with the reason in the
   * tooltip; only `linked` and `off` are switchable, and the switch opens the
   * reviewed enable/disable plan rather than writing from the render pass.
   */
  const agentStateFor = useCallback((row: McpServerRowView, agent: AgentDescriptor): AgentCellState => {
    const cell = row.agents.find((candidate) => candidate.agent === agent.id);
    const presence = agentCards.find((card) => card.id === agent.id)?.presence ?? null;
    const missing = `${agent.label} is not installed on this Mac`;
    if (presence === "not_installed") {
      return { kind: "off", readOnly: true, disabledReason: missing };
    }
    if (row.locked) {
      return {
        kind: "locked",
        lockedBy:
          row.lock_reason === null
            ? scopeLabel(row.lock_scope)
            : `${scopeLabel(row.lock_scope)} · ${row.lock_reason}`,
        enabled: cell?.enabled ?? false,
      };
    }
    if (cell === undefined) {
      return { kind: "none" };
    }
    switch (cell.state) {
      case "linked":
        return { kind: "linked" };
      case "off":
        return { kind: "off" };
      case "locked":
        return { kind: "locked", lockedBy: scopeLabel(row.lock_scope), enabled: cell.enabled };
      case "shadowed":
        return { kind: "shadowed", shadowedBy: "a higher scope" };
      case "native": {
        const sharedWith = row.agents
          .filter(
            (other) =>
              other.agent !== agent.id &&
              other.state === "native" &&
              other.config_path !== null &&
              other.config_path === cell.config_path,
          )
          .map((other) => AGENT_LABEL[other.agent] ?? other.agent);
        return {
          kind: "native",
          path: cell.config_path ?? row.source_path ?? "",
          readOnly: true,
          sharedWith: sharedWith.length > 0 ? sharedWith : undefined,
        };
      }
      case "not_installed":
        return { kind: "off", readOnly: true, disabledReason: `${agent.label} is not installed on this Mac` };
      default:
        return { kind: "none" };
    }
  }, [agentCards]);

  const columns: ReadonlyArray<ColumnDef<DataTableFeatures, McpServerRowView>> = useMemo(() => {
    const nameCol: ColumnDef<DataTableFeatures, McpServerRowView> = {
      id: "name",
      accessorKey: "name",
      header: "Server",
      // Compact mode declares a smaller minimum: the column grows to fill, and
      // the row then fits a 960px window without scrolling sideways.
      size: compactLayout ? 160 : 260,
      meta: { grow: true },
      cell: ({ row }) => {
        const item = row.original;
        const isAdopted = item.id !== "";

        return (
          <div className="flex min-w-0 flex-col gap-2 py-4">
            <div className="flex min-w-0 flex-wrap items-center gap-6">
              <button
                type="button"
                onClick={() => {
                  const detailId = item.id !== "" ? item.id : `native:${item.name}`;
                  app.recordVisitedItem(`server:${detailId}`);
                  app.setSelection("mcp-detail", [detailId]);
                  app.navigate("mcp-detail");
                }}
                className="min-w-0 max-w-full truncate font-medium text-text hover:underline text-left cursor-pointer"
                title={item.name}
              >
                {item.name}
              </button>
              {item.locked && (
                <span role="status" aria-label={`Locked: ${item.lock_reason ?? "locked"}`} className="text-12">
                  🔒 {item.lock_reason ?? "Locked"}
                </span>
              )}
              {item.native && (
                <Badge tone="muted">
                  native · not in AgenTuna
                </Badge>
              )}
            </div>
            <span className="truncate text-12 text-text-secondary">{item.transport}</span>
            {item.native && item.source_path && (
              <div className="flex items-center gap-6">
                <span className="min-w-0 flex-1 truncate font-mono text-11 text-text-muted" title={item.source_path}>
                  {item.source_path}
                </span>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={isAdopted}
                  onClick={(e) => {
                    e.stopPropagation();
                    setAdoptModalTarget(item);
                  }}
                >
                  {isAdopted ? "Adopted" : "Adopt"}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={(e) => {
                    e.stopPropagation();
                    app.navigate("migrate");
                  }}
                >
                  Resolve
                </Button>
              </div>
            )}
          </div>
        );
      },
    };

    const scopeCol: ColumnDef<DataTableFeatures, McpServerRowView> = {
      id: "scope",
      accessorKey: "scope",
      header: "Scope",
      size: 110,
      meta: { align: "center" },
      cell: ({ row }) => {
        const item = row.original;
        const toneClass = SCOPE_BADGE[item.scope] ?? "bg-surface text-text-secondary";
        const lockTooltip = item.locked
          ? item.lock_reason ?? `Locked at ${item.lock_scope ?? "global"} scope`
          : undefined;

        return (
          <span
            title={lockTooltip}
            className={`inline-flex items-center gap-4 rounded-4 px-6 py-2 text-11 font-medium capitalize ${toneClass}`}
          >
            {item.scope}
            {item.locked && <span aria-hidden="true">🔒</span>}
          </span>
        );
      },
    };

    const agentCols = agentColumns<McpServerRowView>({
      agents: detectedAgents,
      getState: agentStateFor,
      // Design 06 gives the three agent columns ~150px so a full config path
      // fits; the compact set cannot afford that and ellipsizes the path.
      width: compactLayout ? 112 : 148,
      onToggle: (row, agent, next) => requestAgentChange(row, agent, next),
    });

    const healthCol: ColumnDef<DataTableFeatures, McpServerRowView> = {
      id: "health",
      accessorKey: "health",
      header: "Health",
      size: 150,
      enableSorting: false,
      cell: ({ row }) => {
        const item = row.original;
        const meta = HEALTH_META[item.health] ?? { label: "Not tested", tone: "muted" as const };
        return (
          <span data-slot="mcp-health" className="flex min-w-0 flex-col items-start gap-2 py-4">
            <Badge tone={meta.tone} dot title={item.locked ? `Locked in ${scopeLabel(item.lock_scope)}` : undefined}>
              {meta.label}
            </Badge>
          </span>
        );
      },
    };

    // Health is the secondary column; the row menu still offers the live test.
    return compactLayout
      ? [nameCol, scopeCol, ...agentCols]
      : [nameCol, scopeCol, ...agentCols, healthCol];
  }, [agentStateFor, compactLayout, detectedAgents, requestAgentChange, app]);

  const projectAgents = list?.project_agents ?? [];
  const repoAgents = useMemo(
    () =>
      dialog?.kind === "disable-repo"
        ? dialog.row.agents.filter((cell) => projectAgents.includes(cell.agent))
        : [],
    [dialog, projectAgents],
  );
  const deleteNeedsName = (preview?.files.length ?? 0) >= 3;
  const deleteConfirmed =
    !deleteNeedsName || typedName === (dialog?.kind === "delete" ? dialog.row.name : "");

  if (loading && list === null) {
    return (
      <main aria-label="MCP Servers" className="mx-auto max-w-[1200px] p-24">
        <p role="status">Loading MCP servers…</p>
      </main>
    );
  }

  if (error !== null && list === null) {
    return (
      <main aria-label="MCP Servers" className="mx-auto max-w-[1200px] p-24">
        <div role="alert" className="flex flex-col items-start gap-8 rounded-8 border border-danger/30 bg-danger/10 p-16 text-text">
          <p className="m-0">{error}</p>
          <Button onClick={() => void load()}>Retry</Button>
        </div>
      </main>
    );
  }

  const managedCount = counts.all - counts.native;

  return (
    <main aria-label="MCP Servers" className="mx-auto flex max-w-[1200px] flex-col gap-16 p-24">
      {/* Header with Title, Count line, and Add server CTA */}
      <header className="flex flex-col gap-12">
        <div className="flex flex-wrap items-center justify-between gap-12">
          <h1 className="m-0 text-22 font-semibold text-text">MCP Servers</h1>
          <div className="flex items-center gap-8">
            <Button
              variant="secondary"
              onClick={() => {
                app.setScreenState("migrate:kind", "mcp_server");
                app.navigate("migrate");
              }}
            >
              Import from apps
            </Button>
            <Button
              variant="primary"
              onClick={() => app.navigate("mcp-form")}
            >
              + Add server
            </Button>
          </div>
        </div>
        <div className="text-12 text-text-muted">
          {counts.all} servers, {managedCount} managed by AgenTuna, {counts.native} live only in agent config files
        </div>
      </header>

      {/* TC-025 Native banner */}
      {counts.native > 0 && !bannerHidden && (
        <section
          role="region"
          aria-label="Unmanaged servers banner"
          className="flex flex-wrap items-center justify-between gap-12 rounded-8 border border-border bg-raised p-12 text-13"
        >
          <span className="text-text">
            <strong>{counts.native}</strong> server{counts.native === 1 ? "" : "s"} in agent folders {counts.native === 1 ? "is" : "are"} not in AgenTuna
          </span>
          <div className="flex items-center gap-8">
            <Button size="sm" onClick={() => app.navigate("migrate")}>
              Review &amp; migrate
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void handleHideBanner()}>
              Hide
            </Button>
          </div>
        </section>
      )}

      {/* Filter chips & Search */}
      <div className="flex flex-wrap items-center justify-between gap-12">
        <div className="flex flex-wrap items-center gap-8" role="group" aria-label="Filter chips">
          <Chip
            pressed={activeChip === "all"}
            onPressedChange={() => setActiveChip("all")}
            count={counts.all}
          >
            All
          </Chip>
          <Chip
            pressed={activeChip === "global"}
            onPressedChange={() => setActiveChip("global")}
            count={counts.global}
            dotTone="scope-global"
          >
            Global
          </Chip>
          <Chip
            pressed={activeChip === "user"}
            onPressedChange={() => setActiveChip("user")}
            count={counts.user}
            dotTone="scope-user"
          >
            User
          </Chip>
          <Chip
            pressed={activeChip === "project"}
            onPressedChange={() => setActiveChip("project")}
            count={counts.project}
            dotTone="scope-project"
          >
            Project
          </Chip>
          <Chip
            pressed={activeChip === "native"}
            onPressedChange={() => setActiveChip("native")}
            count={counts.native}
            dotTone="neutral"
          >
            Not in AgenTuna
          </Chip>
          <Chip
            pressed={activeChip === "failing"}
            onPressedChange={() => setActiveChip("failing")}
            count={counts.failing}
            dotTone="danger"
          >
            Failing
          </Chip>
        </div>

        <div className="w-full sm:w-[240px]">
          <Input
            aria-label="Filter servers"
            placeholder="Search servers…"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>
      </div>

      {/* Table via TU-003 component */}
      <DataTable<McpServerRowView>
        ariaLabel="MCP Servers"
        data={filteredServers}
        columns={columns}
        getRowId={(row) => (row.id === "" ? `native:${row.name}` : row.id)}
        viewportHeight={600}
        rowHeight={88}
        selectionMode="none"
        getRowLabel={(row) => row.name}
        loading={loading}
        emptyTitle="No MCP servers yet"
        emptyBody="No MCP servers in this scope yet."
        emptyAction={{
          label: "Add server",
          onClick: () => app.navigate("mcp-form"),
        }}
        rowActions={(row) => (
          <McpServerActions
            name={row.name}
            native={row.native}
            locked={row.locked}
            canLock={scope === "global"}
            readOnlyReason={
              list?.global_writable === false
                ? list.global_reason ?? "Global store is read-only"
                : null
            }
            open={menuRow === (row.id === "" ? `native:${row.name}` : row.id)}
            onOpenChange={(open) =>
              setMenuRow(open ? (row.id === "" ? `native:${row.name}` : row.id) : null)
            }
            onSelect={(action) => openAction(row, action)}
          />
        )}
      />

      {/* Action Dialogs */}
      <Modal
        open={dialog?.kind === "disable-repo" || dialog?.kind === "disable-agent" || dialog?.kind === "enable-agent"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={
          dialog?.kind === "disable-repo"
            ? `Disable ${dialog.row.name} in ${list?.project_name ?? "this repo"}?`
            : dialog?.kind === "enable-agent"
              ? `Enable ${dialog.row.name} for ${AGENT_LABEL[dialog.agent] ?? dialog.agent}?`
              : `Disable ${dialog?.kind === "disable-agent" ? dialog.row.name : ""} for ${dialog?.kind === "disable-agent" ? AGENT_LABEL[dialog.agent] ?? dialog.agent : ""}?`
        }
        description={
          dialog?.kind === "disable-repo"
            ? repoAgents.length === 0
              ? `${dialog.row.name} stays available for every agent because none has a project-level MCP scope. Disable it for one agent instead.`
              : `It stops loading in ${list?.project_name ?? "this repo"} for ${repoAgents.map((cell) => cell.agent).join(", ")}. Other agents keep loading it.`
            : dialog?.kind === "enable-agent"
              ? `The reviewed change writes this server into ${AGENT_LABEL[dialog.agent] ?? dialog.agent}'s own config file after a backup. It does not start the server.`
              : "The server keeps its definition and stays available for other agents."
        }
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button
              disabled={
                busy ||
                preview === null ||
                (dialog?.kind === "disable-repo" && repoAgents.length === 0)
              }
              onClick={() => {
                if (dialog === null) return;
                if (dialog.kind === "disable-repo") {
                  void applyAction(
                    "mcp_server_disable",
                    {
                      item_id: dialog.row.id,
                      scope: dialog.row.scope,
                      project_id: activeProject,
                      agent: null,
                      in_repo: true,
                      apply: true,
                      preview_id: preview?.preview_id ?? null,
                    },
                    dialog.row.name,
                  );
                } else if (dialog.kind === "disable-agent") {
                  void applyAction(
                    "mcp_server_disable",
                    {
                      item_id: dialog.row.id,
                      scope: dialog.row.scope,
                      project_id: activeProject,
                      agent: dialog.agent,
                      in_repo: false,
                      apply: true,
                      preview_id: preview?.preview_id ?? null,
                    },
                    dialog.row.name,
                  );
                } else if (dialog.kind === "enable-agent") {
                  void applyAction(
                    "mcp_server_enable",
                    {
                      item_id: dialog.row.id,
                      scope: dialog.row.scope,
                      project_id: activeProject,
                      agent: dialog.agent,
                      in_repo: false,
                      apply: true,
                      preview_id: preview?.preview_id ?? null,
                    },
                    dialog.row.name,
                  );
                }
              }}
            >
              {dialog?.kind === "enable-agent" ? "Enable" : "Disable"}
            </Button>
          </>
        }
      >
        {previewError !== null ? (
          <p role="alert" className="text-12 text-danger">
            {previewError}
          </p>
        ) : preview === null ? (
          <p className="text-12 text-text-muted">Calculating file changes…</p>
        ) : (
          <ul className="m-0 list-none p-0 text-12 text-text-secondary">
            {preview.files.map((file) => (
              <li key={file.path}>
                {file.agent}: {file.path}
              </li>
            ))}
          </ul>
        )}
      </Modal>

      <Modal
        open={dialog?.kind === "delete"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={`Delete ${dialog?.kind === "delete" ? dialog.row.name : ""}?`}
        description={
          deleteNeedsName
            ? `This server is referenced by ${preview?.files.length ?? 0} files. Type its name to confirm deletion.`
            : "The server and its configuration entries in agent directories will be removed."
        }
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button
              variant="destructive"
              disabled={busy || preview === null || !deleteConfirmed}
              onClick={() => {
                if (dialog?.kind !== "delete") return;
                void applyAction(
                  "mcp_server_delete",
                  {
                    item_id: dialog.row.id,
                    scope: dialog.row.scope,
                    project_id: activeProject,
                    agent: null,
                    in_repo: false,
                    apply: true,
                    preview_id: preview?.preview_id ?? null,
                  },
                  dialog.row.name,
                );
              }}
            >
              Delete server
            </Button>
          </>
        }
      >
        {deleteNeedsName ? (
          <div className="flex flex-col gap-8">
            <label htmlFor="confirm-delete-name" className="text-12 text-text-secondary">
              Server name
            </label>
            <Input
              id="confirm-delete-name"
              aria-label="Server name"
              value={typedName}
              onChange={(event) => setTypedName(event.target.value)}
              placeholder={dialog?.kind === "delete" ? dialog.row.name : ""}
            />
          </div>
        ) : null}
      </Modal>

      <Modal
        open={dialog?.kind === "lock" || dialog?.kind === "unlock"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={
          dialog?.kind === "lock"
            ? `Lock ${dialog.row.name}?`
            : `Unlock ${dialog?.kind === "unlock" ? dialog.row.name : ""}?`
        }
        description={
          dialog?.kind === "lock"
            ? "A locked server cannot be modified or overridden from lower scopes."
            : "Lower scopes will be allowed to override or modify this server."
        }
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button
              disabled={busy || (dialog?.kind === "lock" && reason.trim().length === 0)}
              onClick={() => void applyLock()}
            >
              {dialog?.kind === "lock" ? "Lock item" : "Unlock item"}
            </Button>
          </>
        }
      >
        {dialog?.kind === "lock" ? (
          <div className="flex flex-col gap-8">
            <label htmlFor="lock-reason" className="text-12 text-text-secondary">
              Lock reason
            </label>
            <Input
              id="lock-reason"
              aria-label="Lock reason"
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              placeholder="e.g. Approved production configuration"
            />
          </div>
        ) : null}
      </Modal>

      {why !== null ? (
        <WhyTraceDialog
          open={true}
          itemId={why.itemId}
          agent={why.agent}
          onClose={() => setWhy(null)}
        />
      ) : null}

      {adoptModalTarget !== null && adoptModalTarget.source_path && (
        <AdoptModal
          path={adoptModalTarget.source_path}
          name={adoptModalTarget.name}
          onAdoptSuccess={() => {
            toast.success(`Adopted ${adoptModalTarget.name} into AgenTuna`);
            setAdoptModalTarget(null);
            void load();
          }}
          onRescan={() => void load()}
          onClose={() => setAdoptModalTarget(null)}
        />
      )}
    </main>
  );
}
