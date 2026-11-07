/**
 * Skills list screen (TS-028).
 *
 * Virtualized DataTable with per-agent state cells over `at_db::skills_index`,
 * filter chips (All, Global, User, Project, Not in AgenTuna, Shadowed) with live counts,
 * debounced search, shadow/native/lock badges, row actions menu, shared-root coupling
 * explanation, and a mount point for TC-025's Not in AgenTuna banner.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import { Input } from "../../components/input";
import { Badge } from "../../components/badge";
import type { BadgeTone } from "../../components/badge";
import { Chip } from "../../components/chip";
import { PresetsScreen } from "../presets/PresetsScreen";
import { SkillUpdateBadge } from "./SkillUpdateBadge";
import { Modal } from "../../overlays/dialogs/Modal";
import { DiffViewer } from "../../components/diff";
import { ScopeSwitcher } from "../../components/scope_switcher/ScopeSwitcher";
import {
  DataTable,
  agentColumns,
  useRowMenu,
  RowMenuTrigger,
  type AgentDescriptor,
} from "../../components/table";
import type { ScopeToken } from "../../theme";
import type { ColumnDef } from "@tanstack/react-table";
import type { DataTableFeatures } from "../../components/table/features";
import type { SkillsListView } from "../../generated/SkillsListView";
import type { SkillRowView } from "../../generated/SkillRowView";
import type { BulkSkillApplyRequest } from "../../generated/BulkSkillApplyRequest";
import type { BulkSkillBatchView } from "../../generated/BulkSkillBatchView";
import { BulkSkillTransferDialog } from "../../overlays/dialogs/SkillActionDialogs";
import { SkillActionDialogs } from "../../overlays/dialogs/SkillActionDialogs";
import type { SkillActionDialogRequest } from "../../overlays/dialogs/SkillActionDialogs";
import { WhyTraceDialog } from "../../overlays/dialogs/WhyTraceDialog";
import { AdoptModal } from "../../overlays/dialogs/AdoptModal";
import type { SkillAgentPreviewRequest } from "../../generated/SkillAgentPreviewRequest";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import type { AdoptPreviewView } from "../../generated/AdoptPreviewView";
import type { AdoptApplyView } from "../../generated/AdoptApplyView";
import type { SkillUpdatePreviewView } from "../../generated/SkillUpdatePreviewView";
import type { SkillUpdateApplyView } from "../../generated/SkillUpdateApplyView";
import { useToast } from "../../overlays/toasts";
import { showAdoptFirstToast } from "../../overlays/toasts/catalogue";
import { useAppSnapshot, useAppState } from "../../state/context";

export type FilterChip = "all" | "global" | "user" | "project" | "native" | "shadowed";

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
  local: "bg-scope-project/15 text-scope-project",
};

/** Precedence order used to name the copy a row overrides. */
const SCOPE_RANK: Readonly<Record<string, number>> = { global: 0, user: 1, project: 2, local: 3 };

/** Display names for scope values in status copy. */
const STATUS_SCOPE_NAME: Readonly<Record<string, string>> = {
  global: "Global",
  user: "User",
  project: "Project",
  local: "Local",
};

/** Human label for a scope wire name. */
function statusScope(scope: string | null): string {
  if (scope === null) return "another scope";
  return STATUS_SCOPE_NAME[scope] ?? scope;
}

/** Everything the STATUS column renders for one row. */
export interface SkillStatus {
  /** Visible text: never colour alone. */
  readonly label: string;
  /** Badge tone for the semantic colour. */
  readonly tone: BadgeTone;
  /** Optional tooltip with the lock reason or the conflicting paths. */
  readonly title?: string;
}

/** Distinct native folder paths recorded for one native skill name. */
function nativeCopyPaths(row: SkillRowView, allRows: readonly SkillRowView[]): readonly string[] {
  const paths = new Set<string>();
  for (const other of allRows) {
    if (!other.native || other.name !== row.name) continue;
    if (other.native_path) paths.add(other.native_path);
    for (const cell of other.agents) {
      if (cell.state === "native" && cell.config_path) paths.add(cell.config_path);
    }
  }
  return [...paths];
}

/**
 * Derives the STATUS column for one skill row.
 *
 * Every branch names real state read from the list projection: a lock, a
 * higher-precedence copy, the native copies found in agent folders, or a
 * lower-scope copy this row overrides. Nothing is inferred from colour.
 */
export function skillStatus(
  row: SkillRowView,
  allRows: readonly SkillRowView[],
  agentLabel: (id: string) => string,
): SkillStatus {
  if (row.locked) {
    return {
      label: `Locked by ${statusScope(row.lock_scope)}`,
      tone: "locked",
      title: row.lock_reason ?? `Locked at ${statusScope(row.lock_scope)} scope`,
    };
  }
  if (row.shadowed_by) {
    return { label: `Shadowed by ${statusScope(row.shadowed_by)}`, tone: "neutral" };
  }
  if (row.native) {
    const paths = nativeCopyPaths(row, allRows);
    const ownersPerPath = new Map<string, Set<string>>();
    for (const other of allRows) {
      if (!other.native || other.name !== row.name) continue;
      for (const cell of other.agents) {
        if (cell.state !== "native" || cell.config_path === null) continue;
        const owners = ownersPerPath.get(cell.config_path) ?? new Set<string>();
        owners.add(agentLabel(cell.agent));
        ownersPerPath.set(cell.config_path, owners);
      }
    }
    const sharedRoot = [...ownersPerPath.values()].some((owners) => owners.size > 1);
    if (paths.length > 1) {
      return sharedRoot
        ? { label: `${paths.length} copies · 1 shared root`, tone: "info", title: paths.join("\n") }
        : {
            label: `${paths.length} copies in different folders`,
            tone: "warning",
            title: paths.join("\n"),
          };
    }
    if (sharedRoot) {
      return { label: "1 copy · shared root", tone: "info", title: paths.join("\n") };
    }
    const owners = [...ownersPerPath.values()].flatMap((set) => [...set]);
    return {
      label: owners.length > 0 ? `${owners.join(" + ")} only` : "Native only",
      tone: "info",
    };
  }
  const overridden = allRows.find(
    (other) =>
      !other.native &&
      other.scope !== row.scope &&
      other.name === row.name &&
      (SCOPE_RANK[other.scope] ?? 0) < (SCOPE_RANK[row.scope] ?? 0),
  );
  if (overridden !== undefined) {
    return { label: `Overrides ${statusScope(overridden.scope)} copy`, tone: "warning" };
  }
  return { label: "Active", tone: "success" };
}

/** Parses a typed command rejection. */
function parseCommandError(cause: unknown): { code: string; message: string; refusal?: unknown } {
  if (typeof cause === "object" && cause !== null && "code" in cause && "message" in cause) {
    const value = cause as { code: unknown; message: unknown; refusal?: unknown };
    return {
      code: String(value.code),
      message: String(value.message),
      refusal: value.refusal,
    };
  }
  return { code: "error", message: String(cause) };
}
function skillRowId(row: SkillRowView): string {
  return row.item_id !== "" ? row.item_id : `native:${row.native_path ?? row.name}`;
}

export function SkillsScreen({
  localEnabled = false,
  globalLocked = false,
}: {
  readonly localEnabled?: boolean;
  readonly globalLocked?: boolean;
} = {}) {
  const { scope, activeProject, externalRefreshVersion } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const rowMenu = useRowMenu();
  useEffect(() => {
    if (rowMenu.openRowId === null) return;
    const row = Array.from(document.querySelectorAll<HTMLElement>("[data-row-menu-id]"))
      .find((element) => element.dataset["rowMenuId"] === rowMenu.openRowId);
    row?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
  }, [rowMenu.openRowId]);

  const [view, setView] = useState<SkillsListView | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"skills" | "presets">(
    app.getSnapshot().screenState["skills:tab"] === "presets" ? "presets" : "skills"
  );
  const [error, setError] = useState<string | null>(null);

  const [activeChip, setActiveChip] = useState<FilterChip>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [bannerHidden, setBannerHidden] = useState(false);
  const [adoptingPath, setAdoptingPath] = useState<string | null>(null);
  const [adoptModalTarget, setAdoptModalTarget] = useState<SkillRowView | null>(null);
  const [whyTrace, setWhyTrace] = useState<{ itemId: string; scope: string; agent: string } | null>(null);
  const [selectedIds, setSelectedIds] = useState<ReadonlySet<string>>(() => new Set());
  const [actionDialog, setActionDialog] = useState<SkillActionDialogRequest | null>(null);
  const [bulkAgentOpen, setBulkAgentOpen] = useState(false);
  const [bulkMode, setBulkMode] = useState<"enable" | "disable">("enable");
  const [bulkAgentIds, setBulkAgentIds] = useState<ReadonlySet<string>>(() => new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  const [bulkProgress, setBulkProgress] = useState<string | null>(null);
  const [bulkFailures, setBulkFailures] = useState<Array<{ name: string; reason: string }>>([]);
  const [bulkTransfer, setBulkTransfer] = useState<"promote" | "move" | null>(null);
  const [updateReview, setUpdateReview] = useState<{ name: string; preview: SkillUpdatePreviewView } | null>(null);
  const [updateBusy, setUpdateBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [listResult, settingsResult] = await Promise.all([
        invoke<SkillsListView>("skills_list", {
          request: { scope: null, project_id: activeProject },
        }),
        invoke<Record<string, unknown>>("settings_load").catch(() => null),
      ]);
      setView(listResult);
      if (settingsResult !== null) {
        setBannerHidden(settingsResult["native_banner_hidden.skill"] === true);
      }
    } catch (cause) {
      setError(parseCommandError(cause).message);
    } finally {
      setLoading(false);
    }
  }, [activeProject, externalRefreshVersion]);

  useEffect(() => {
    void load();
  }, [load]);

  const allRows = useMemo(() => view?.rows ?? [], [view]);

  // Live chip counts computed directly from the row set so counts and visible rows always match
  const counts = useMemo(() => {
    return {
      all: allRows.length,
      global: allRows.filter((r) => r.scope === "global").length,
      user: allRows.filter((r) => r.scope === "user").length,
      project: allRows.filter((r) => r.scope === "project").length,
      native: allRows.filter((r) => r.native).length,
      shadowed: allRows.filter((r) => Boolean(r.shadowed_by)).length,
    };
  }, [allRows]);

  const filteredRows = useMemo(() => {
    let rows = allRows;
    if (activeChip === "global") rows = rows.filter((r) => r.scope === "global");
    else if (activeChip === "user") rows = rows.filter((r) => r.scope === "user");
    else if (activeChip === "project") rows = rows.filter((r) => r.scope === "project");
    else if (activeChip === "native") rows = rows.filter((r) => r.native);
    else if (activeChip === "shadowed") rows = rows.filter((r) => Boolean(r.shadowed_by));

    const query = searchQuery.trim().toLowerCase();
    if (query !== "") {
      rows = rows.filter((r) => {
        return (
          r.name.toLowerCase().includes(query) ||
          (r.description !== null && r.description.toLowerCase().includes(query))
        );
      });
    }
    return rows;
  }, [allRows, activeChip, searchQuery]);
  const selectedRows = useMemo(
    () => allRows.filter((row) => selectedIds.has(skillRowId(row))),
    [allRows, selectedIds],
  );
  const visibleSelectedCount = useMemo(
    () => filteredRows.filter((row) => selectedIds.has(skillRowId(row))).length,
    [filteredRows, selectedIds],
  );
  useEffect(() => {
    const currentIds = new Set(allRows.map(skillRowId));
    setSelectedIds((selection) => {
      const next = new Set([...selection].filter((id) => currentIds.has(id)));
      return next.size === selection.size ? selection : next;
    });
  }, [allRows]);

  const agents: readonly AgentDescriptor[] = useMemo(() => {
    if (!view?.agents) return [];
    return view.agents.map((a) => ({
      id: a.id,
      label: a.label,
    }));
  }, [view?.agents]);

  const handleAdopt = useCallback(
    async (row: SkillRowView) => {
      if (!row.native_path) return;
      setAdoptingPath(row.native_path);
      try {
        const preview = await invoke<AdoptPreviewView>("adopt_preview", {
          path: row.native_path,
          expectedHash: null,
          handling: "link",
        });
        await invoke<AdoptApplyView>("adopt_apply", {
          path: preview.source,
          expectedHash: null,
          handling: "link",
        });
        toast.success(`Adopted ${row.name} into AgenTuna`);
        await load();
      } catch (cause) {
        const failure = parseCommandError(cause);
        toast.warning(failure.message, { title: "Adoption failed" });
      } finally {
        setAdoptingPath(null);
      }
    },
    [load, toast]
  );
  const handleToggleAgent = useCallback(
    async (row: SkillRowView, agent: AgentDescriptor, next: boolean) => {
      if (row.native || row.item_id === "") return;
      try {
        const previewReq: SkillAgentPreviewRequest = {
          item_id: row.item_id,
          scope: row.scope,
          agent_id: agent.id,
          enabled: next,
          project_id: activeProject,
        };
        const preview = await invoke<SkillAgentPreviewView>("skill_agent_preview", {
          request: previewReq,
        });

        await invoke<SkillAgentApplyView>("skill_agent_apply", {
          previewId: preview.preview_id,
        });

        toast.success(
          next
            ? `Enabled ${row.name} for ${agent.label}`
            : `Disabled ${row.name} for ${agent.label}`
        );
        await load();
      } catch (cause) {
        const failure = parseCommandError(cause);
        if (
          failure.code === "refused_overwrite" ||
          failure.code === "RefusedOverwrite" ||
          failure.message.toLowerCase().includes("native conflict")
        ) {
          showAdoptFirstToast(toast, () => void handleAdopt(row));
        } else {
          toast.warning(failure.message, { title: "Could not update skill" });
        }
        await load();
      }
    },
    [activeProject, handleAdopt, load, toast]
  );


  const handleHideBanner = useCallback(async () => {
    try {
      await invoke("settings_set", { key: "native_banner_hidden.skill", value: true });
      setBannerHidden(true);
    } catch {
      setBannerHidden(true);
    }
  }, []);

  const handleReviewUpdate = useCallback(async (row: SkillRowView) => {
    if (row.item_id === "") return;
    setUpdateBusy(row.item_id);
    try {
      const preview = await invoke<SkillUpdatePreviewView>("skill_update_preview", {
        itemId: row.item_id,
        confirmed: true,
      });
      setUpdateReview({ name: row.name, preview });
    } catch (cause) {
      toast.warning(parseCommandError(cause).message, { title: "Update preview failed" });
    } finally {
      setUpdateBusy(null);
    }
  }, [toast]);

  const handleApplyUpdate = useCallback(async () => {
    if (updateReview === null) return;
    setUpdateBusy("apply");
    try {
      const result = await invoke<SkillUpdateApplyView>("skill_update_apply", {
        previewId: updateReview.preview.preview_id,
        confirmed: true,
      });
      setUpdateReview(null);
      toast.success(
        `${result.files_changed} file${result.files_changed === 1 ? "" : "s"} backed up and updated.`,
        { title: "Skill updated" }
      );
      await load();
    } catch (cause) {
      setUpdateReview(null);
      toast.warning(parseCommandError(cause).message, { title: "Update refused" });
    } finally {
      setUpdateBusy(null);
    }
  }, [load, toast, updateReview]);

  const handleIgnoreUpdate = useCallback(async (row: SkillRowView) => {
    if (row.item_id === "") return;
    setUpdateBusy(row.item_id);
    try {
      await invoke("skill_update_ignore", { itemId: row.item_id });
      toast.info(
        `Ignored upstream ${row.source_remote_sha?.slice(0, 10) ?? "version"} for ${row.name}; the badge stays hidden until a newer commit appears.`
      );
      await load();
    } catch (cause) {
      toast.warning(parseCommandError(cause).message, { title: "Version not ignored" });
    } finally {
      setUpdateBusy(null);
    }
  }, [load, toast]);

  const columns: ReadonlyArray<ColumnDef<DataTableFeatures, SkillRowView>> = useMemo(() => {
    const nameCol: ColumnDef<DataTableFeatures, SkillRowView> = {
      id: "name",
      accessorKey: "name",
      header: "Skill",
      size: 280,
      meta: { grow: true },
      cell: ({ row }) => {
        const item = row.original;
        const isAdopted = item.state === "adopted";
        const isAdopting = adoptingPath === item.native_path;

        return (
          <div className="flex min-w-0 flex-col gap-2 py-4">
            <div className="flex flex-wrap items-center gap-6">
              <span className="font-medium text-text">{item.name}</span>
              {!item.native && (
                <SkillUpdateBadge
                  row={item}
                  busy={updateBusy === item.item_id}
                  onUpdate={(row) => void handleReviewUpdate(row)}
                  onIgnore={(row) => void handleIgnoreUpdate(row)}
                />
              )}
              {item.native && <Badge tone="muted">not in AgenTuna</Badge>}
              {item.state && !item.native && (
                <Badge tone="muted">
                  {item.state}
                </Badge>
              )}
            </div>
            {item.description && (
              <span className="truncate text-12 text-text-muted">{item.description}</span>
            )}
            {item.native && item.native_path && (
              <div className="flex items-center gap-6 pt-2">
                <span className="min-w-0 flex-1 truncate font-mono text-11 text-text-muted" title={item.native_path}>
                  {item.native_path}
                </span>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={isAdopted || isAdopting}
                  onClick={(e) => {
                    e.stopPropagation();
                    setAdoptModalTarget(item);
                  }}
                >
                  {isAdopted ? "Adopted" : isAdopting ? "Adopting…" : "Adopt"}
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

    const scopeCol: ColumnDef<DataTableFeatures, SkillRowView> = {
      id: "scope",
      accessorKey: "scope",
      header: "Scope",
      size: 100,
      meta: { align: "center" },
      cell: ({ row }) => {
        const item = row.original;
        const toneClass = SCOPE_BADGE[item.scope] ?? "bg-surface text-text-secondary";
        return (
          <span
            className={`inline-flex items-center rounded-4 px-6 py-2 text-11 font-medium capitalize ${toneClass}`}
          >
            {item.scope}
            {item.locked ? " · locked" : ""}
          </span>
        );
      },
    };

    const agentCols = agentColumns<SkillRowView>({
      agents,
      getState: (row, agent) => {
        if (row.shadowed_by) {
          return {
            kind: "shadowed",
            shadowedBy: row.shadowed_by,
          };
        }
        const cell = row.agents.find((c) => c.agent === agent.id);
        if (!cell) {
          return { kind: "none" };
        }
        if (cell.state === "linked") {
          return { kind: "linked" };
        }
        if (cell.state === "off") {
          return { kind: "off" };
        }
        if (cell.state === "native") {
          const isShared =
            (agent.id === "codex" || agent.id === "omp") && row.scope === "project";
          return {
            kind: "native",
            path: cell.config_path ?? row.native_path ?? "",
            sharedWith: isShared ? [agent.id === "codex" ? "omp" : "codex"] : undefined,
          };
        }
        return { kind: "none" };
      },
      onToggle: (row, agent, next) => {
        void handleToggleAgent(row, agent, next);
      },
    });

    const statusCol: ColumnDef<DataTableFeatures, SkillRowView> = {
      id: "status",
      header: "Status",
      size: 190,
      enableSorting: false,
      cell: ({ row }) => {
        const item = row.original;
        const status = skillStatus(item, allRows, (id) =>
          agents.find((agent) => agent.id === id)?.label ?? id,
        );
        return (
          <span data-slot="skill-status" className="flex min-w-0 flex-col items-start gap-2 py-4">
            <Badge tone={status.tone} dot title={status.title}>
              {status.label}
            </Badge>
          </span>
        );
      },
    };

    return [nameCol, scopeCol, ...agentCols, statusCol];
  }, [agents, allRows, adoptingPath, handleAdopt, handleIgnoreUpdate, handleReviewUpdate, handleToggleAgent, app, updateBusy]);

  const handleBulkAgentChange = useCallback(async () => {
    const selected = selectedRows;
    const selectedAgents = agents.filter((agent) => bulkAgentIds.has(agent.id));
    if (selectedAgents.length === 0 || selected.length === 0) return;
    const enabled = bulkMode === "enable";
    let previewIds: string[] = [];
    setBulkBusy(true);
    setBulkProgress(`Reviewing ${selected.length} skills for ${selectedAgents.length} agent(s)…`);
    setBulkFailures([]);
    try {
      const results = await Promise.all(
        selected.flatMap((row) =>
          selectedAgents.map(async (agent) => {
            if (row.native || row.item_id === "") {
              return { row, agent, preview: null, reason: "Native skill is not managed by AgenTuna." };
            }
            const request: SkillAgentPreviewRequest = {
              item_id: row.item_id,
              scope: row.scope,
              agent_id: agent.id,
              enabled,
              project_id: activeProject,
            };
            try {
              return {
                row,
                agent,
                preview: await invoke<SkillAgentPreviewView>("skill_agent_preview", { request }),
                reason: null,
              };
            } catch (cause) {
              return { row, agent, preview: null, reason: parseCommandError(cause).message };
            }
          }),
        ),
      );
      const failures = results.flatMap((result) =>
        result.reason === null
          ? []
          : [{ name: `${result.row.name} · ${result.agent.label}`, reason: result.reason }],
      );
      previewIds = results.flatMap((result) =>
        result.preview === null ? [] : [result.preview.preview_id],
      );
      if (failures.length > 0) {
        setBulkFailures(failures);
        await Promise.all(previewIds.map((previewId) =>
          invoke("skill_agent_discard", { previewId }).catch(() => undefined),
        ));
        previewIds = [];
        return;
      }
      const previews = results.flatMap((result) => result.preview === null ? [] : [result.preview]);
      if (previews.every((preview) => preview.no_op)) {
        await Promise.all(previewIds.map((previewId) =>
          invoke("skill_agent_discard", { previewId }).catch(() => undefined),
        ));
        previewIds = [];
        toast.info(`All selected skills already have the requested state for ${selectedAgents.length} agent(s).`);
        setSelectedIds(new Set());
        setBulkAgentOpen(false);
        return;
      }
      const label = `${enabled ? "Enable" : "Disable"} ${selected.length} skills for ${selectedAgents.length} agent(s)`;
      setBulkProgress(`${enabled ? "Enabling" : "Disabling"} ${selected.length} skills…`);
      const request: BulkSkillApplyRequest = { preview_ids: previewIds, label };
      const result = await invoke<BulkSkillBatchView>("bulk_skill_apply", { request });
      if (!result.applied) {
        const reason = result.refusal ?? "The batch was refused before any files changed.";
        setBulkFailures(selected.flatMap((row) =>
          selectedAgents.map((agent) => ({ name: `${row.name} · ${agent.label}`, reason })),
        ));
        await Promise.all(previewIds.map((previewId) =>
          invoke("skill_agent_discard", { previewId }).catch(() => undefined),
        ));
        previewIds = [];
        return;
      }
      toast.success(`${selected.length} skills updated for ${selectedAgents.length} agent(s).`);
      setSelectedIds(new Set());
      setBulkAgentOpen(false);
      await load();
    } catch (cause) {
      const reason = parseCommandError(cause).message;
      setBulkFailures(selected.flatMap((row) =>
        selectedAgents.map((agent) => ({ name: `${row.name} · ${agent.label}`, reason })),
      ));
      await Promise.all(previewIds.map((previewId) =>
        invoke("skill_agent_discard", { previewId }).catch(() => undefined),
      ));
    } finally {
      setBulkBusy(false);
      setBulkProgress(null);
    }
  }, [activeProject, agents, bulkAgentIds, bulkMode, load, selectedRows, toast]);

  const handleRowAction = useCallback(
    (row: SkillRowView, action: string) => {
      const id = skillRowId(row);
      if (["promote", "move", "duplicate", "delete", "disable-in-repo"].includes(action)) {
        const trigger = Array.from(document.querySelectorAll<HTMLElement>("[data-row-menu-id]"))
          .find((element) => element.dataset["rowMenuId"] === id)
          ?.querySelector<HTMLButtonElement>('[data-slot="row-menu-trigger"]');
        trigger?.focus();
      }
      rowMenu.close();
      if (bulkBusy) return;
      switch (action) {
        case "open":
          if (row.native) {
            app.navigate("migrate");
          } else {
            app.recordVisitedItem(`skill:${row.item_id}`);
            app.setScreenState("detail:itemId", row.item_id);
            app.navigate("skill-detail");
          }
          break;
        case "promote":
          if (!row.native) setActionDialog({ kind: "transfer", itemId: row.item_id, transfer: "promote" });
          break;
        case "move":
          if (!row.native) setActionDialog({ kind: "transfer", itemId: row.item_id, transfer: "move" });
          break;
        case "duplicate":
          if (!row.native) setActionDialog({ kind: "transfer", itemId: row.item_id, transfer: "duplicate" });
          break;
        case "delete":
          if (!row.native) setActionDialog({ kind: "delete", itemId: row.item_id });
          break;
        case "disable-in-repo":
          if (!row.native && activeProject !== null) {
            setActionDialog({ kind: "disable-in-repo", itemId: row.item_id });
          }
          break;
        case "why":
          setWhyTrace({ itemId: row.item_id, scope: row.scope, agent: "claude_code" });
          break;
        default:
          break;
      }
    },
    [activeProject, app, bulkBusy, rowMenu, toast]
  );

  const openBulkAgent = (mode: "enable" | "disable") => {
    setBulkMode(mode);
    setBulkAgentIds(new Set());
    setBulkFailures([]);
    setBulkAgentOpen(true);
  };

  const openBulkTransfer = (transfer: "promote" | "move") => {
    setBulkAgentOpen(false);
    setBulkFailures([]);
    setActionDialog(null);
    setBulkTransfer(transfer);
  };

  if (activeTab === "presets") {
    return (
      <div className="flex flex-col">
        <nav aria-label="Skill views" className="mx-auto flex w-full max-w-[1200px] items-center justify-between px-24 pt-16">
          <div role="tablist" className="flex items-center gap-4 rounded-8 border border-border p-2">
            <button
              role="tab"
              aria-selected={false}
              onClick={() => setActiveTab("skills")}
              className="rounded-6 px-12 py-4 text-13 font-medium text-text-muted hover:text-text"
            >
              Skills
            </button>
            <button
              role="tab"
              aria-selected={true}
              onClick={() => setActiveTab("presets")}
              className="rounded-6 bg-raised px-12 py-4 text-13 font-medium text-text"
            >
              Presets
            </button>
          </div>
        </nav>
        <PresetsScreen />
      </div>
    );
  }

  if (loading && view === null) {
    return (
      <main aria-label="Skills" className="mx-auto flex max-w-[1200px] flex-col gap-16 p-24">
        <header className="flex items-center justify-between">
          <h1 className="m-0 text-22 font-semibold text-text">Skills</h1>
        </header>
        <p role="status" className="text-13 text-text-muted">Loading skills…</p>
      </main>
    );
  }

  if (error !== null && view === null) {
    return (
      <main aria-label="Skills" className="mx-auto flex max-w-[1200px] flex-col gap-16 p-24">
        <header className="flex items-center justify-between">
          <h1 className="m-0 text-22 font-semibold text-text">Skills</h1>
        </header>
        <div role="alert" className="flex flex-col items-start gap-8 rounded-8 border border-danger/30 bg-danger/10 p-16 text-text">
          <p className="m-0">{error}</p>
          <Button onClick={() => void load()}>Retry</Button>
        </div>
      </main>
    );
  }
  return (
    <main
      aria-label="Skills"
      className="mx-auto flex max-w-[1200px] flex-col gap-16 p-24"
      onKeyDown={(event) => {
        if (event.key === "Escape" && actionDialog === null && bulkTransfer === null) {
          setSelectedIds(new Set());
          setBulkAgentOpen(false);
          setBulkFailures([]);
        }
      }}
    >
      {/* Top bar with ScopeSwitcher, Title, Counts, Add skill CTA */}
      <header className="flex flex-col gap-12">
        <div className="flex flex-wrap items-center justify-between gap-12">
          <div className="flex items-center gap-12">
            <h1 className="m-0 text-22 font-semibold text-text">Skills</h1>
            <div role="tablist" className="flex items-center gap-4 rounded-8 border border-border p-2">
              <button
                role="tab"
                aria-selected={true}
                onClick={() => setActiveTab("skills")}
                className="rounded-6 bg-raised px-12 py-4 text-13 font-medium text-text"
              >
                Skills
              </button>
              <button
                role="tab"
                aria-selected={false}
                onClick={() => setActiveTab("presets")}
                className="rounded-6 px-12 py-4 text-13 font-medium text-text-muted hover:text-text"
              >
                Presets
              </button>
            </div>
            <ScopeSwitcher
              scope={scope as ScopeToken}
              localEnabled={localEnabled}
              hasProject={Boolean(activeProject)}
              globalLocked={globalLocked}
              onScopeSelected={(s) => app.receiveScope(s)}
            />
          </div>
          <Button onClick={() => app.navigate("add-skill")}>Add skill</Button>
        </div>
        <div className="text-12 text-text-muted">
          {counts.all} skill{counts.all === 1 ? "" : "s"} ({counts.all - counts.native} managed by AgenTuna, {counts.native} live only in agent folders)
        </div>
      </header>

      {/* TC-025 Mount point: Not in AgenTuna banner */}
      {counts.native > 0 && !bannerHidden && (
        <section
          role="region"
          aria-label="Unmanaged skills banner"
          className="flex flex-wrap items-center justify-between gap-12 rounded-8 border border-border bg-raised p-12 text-13"
        >
          <span className="text-text">
            <strong>{counts.native}</strong> skill{counts.native === 1 ? "" : "s"} in agent folders {counts.native === 1 ? "is" : "are"} not in AgenTuna
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
            pressed={activeChip === "shadowed"}
            onPressedChange={() => setActiveChip("shadowed")}
            count={counts.shadowed}
            dotTone="warning"
          >
            Shadowed
          </Chip>
        </div>

        <div className="w-full sm:w-[240px]">
          <Input
            aria-label="Filter skills"
            placeholder="Search skills…"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>
      </div>

      {selectedRows.length > 0 && (
        <section
          aria-label="Bulk skill actions"
          className="fixed bottom-16 left-[calc(50%+100px)] z-40 grid w-max max-w-[calc(100vw-240px)] -translate-x-1/2 gap-8 rounded-8 border border-accent/40 bg-raised p-12 shadow-lg"
        >
          <div className="flex flex-wrap items-center gap-8">
            <strong className="mr-4 text-13 text-text">{selectedRows.length} selected</strong>
            {selectedRows.length > visibleSelectedCount ? (
              <span className="mr-4 text-12 text-text-muted">
                {selectedRows.length - visibleSelectedCount} hidden by filter
              </span>
            ) : null}
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkTransfer("promote")}>Promote</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkAgent("enable")}>Enable for agent</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkTransfer("move")}>Move to scope</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkAgent("disable")}>Disable</Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={bulkBusy}
              onClick={() => {
                setSelectedIds(new Set());
                setBulkAgentOpen(false);
                setBulkFailures([]);
              }}
            >
              Clear
            </Button>
            {bulkProgress !== null ? <span role="status" className="text-12 text-text-muted">{bulkProgress}</span> : null}
          </div>
          {bulkAgentOpen && (
            <div role="dialog" aria-label={`${bulkMode === "enable" ? "Enable" : "Disable"} skills for agents`} className="grid gap-8 rounded-6 border border-border bg-surface p-12">
              <fieldset className="m-0 grid gap-6 border-0 p-0">
                <legend className="mb-4 text-12 font-semibold text-text">
                  Choose agents for all {selectedRows.length} selected skills
                </legend>
                {agents.map((agent) => (
                  <label key={agent.id} className="flex items-center gap-8 text-12 text-text">
                    <input
                      type="checkbox"
                      checked={bulkAgentIds.has(agent.id)}
                      disabled={bulkBusy}
                      onChange={(event) => {
                        setBulkAgentIds((current) => {
                          const next = new Set(current);
                          if (event.target.checked) next.add(agent.id);
                          else next.delete(agent.id);
                          return next;
                        });
                      }}
                    />
                    {agent.label}
                  </label>
                ))}
              </fieldset>
              <div className="flex flex-wrap gap-8">
                <Button
                  size="sm"
                  variant="primary"
                  disabled={bulkBusy || bulkAgentIds.size === 0}
                  onClick={() => void handleBulkAgentChange()}
                >
                  {bulkBusy ? "Applying…" : `${bulkMode === "enable" ? "Enable" : "Disable"} for selected agents`}
                </Button>
                <Button size="sm" variant="ghost" disabled={bulkBusy} onClick={() => setBulkAgentOpen(false)}>Cancel</Button>
              </div>
            </div>
          )}
          {bulkFailures.length > 0 && (
            <section aria-label="Bulk skill failures" className="max-h-[180px] overflow-auto rounded-6 border border-danger/30 bg-danger/5 p-8">
              <h2 className="m-0 text-12 font-semibold text-danger">No changes applied</h2>
              <ul className="m-0 mt-6 grid gap-4 p-0 text-11 text-text">
                {bulkFailures.map((failure, index) => (
                  <li key={`${failure.name}-${index}`} className="break-words">
                    <strong>{failure.name}</strong>: {failure.reason}
                  </li>
                ))}
              </ul>
            </section>
          )}
        </section>
      )}


      {/* Virtualized DataTable */}
      <DataTable<SkillRowView>
        ariaLabel="Skills"
        data={filteredRows}
        columns={columns}
        viewportHeight={600}
        selectionMode="multiple"
        selectedIds={selectedIds}
        onSelectionChange={setSelectedIds}
        rowActionsOpen={(row) => rowMenu.isOpen(skillRowId(row))}
        getRowId={skillRowId}
        getRowLabel={(r) => r.name}
        loading={loading}
        rowTone={(r) => (r.shadowed_by ? "dimmed" : "default")}
        rowTooltip={(r) =>
          r.shadowed_by
            ? `Shadowed by ${r.shadowed_by}${r.native_path ? ` (${r.native_path})` : ""}`
            : undefined
        }
        emptyTitle="No skills found"
        emptyBody="Add a skill or adjust your filters."
        emptyAction={{
          label: "Add skill",
          onClick: () => app.navigate("add-skill"),
        }}
        rowActions={(row) => (
          <div className="relative flex items-center justify-center" data-row-menu data-row-menu-id={skillRowId(row)}>
            <RowMenuTrigger
              label={`Actions for ${row.name}`}
              open={rowMenu.isOpen(skillRowId(row))}
              disabled={bulkBusy}
              onToggle={() => rowMenu.toggle(skillRowId(row))}
            />
            {rowMenu.isOpen(skillRowId(row)) && (
              <div
                role="menu"
                aria-label={`Actions for ${row.name}`}
                data-row-menu
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    rowMenu.close();
                    event.currentTarget.parentElement
                      ?.querySelector<HTMLButtonElement>('[data-slot="row-menu-trigger"]')
                      ?.focus();
                    return;
                  }
                  const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
                  const current = items.indexOf(document.activeElement as HTMLButtonElement);
                  let targetIndex: number | null = null;
                  if (event.key === "ArrowDown") targetIndex = current < 0 ? 0 : (current + 1) % items.length;
                  if (event.key === "ArrowUp") targetIndex = current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length;
                  if (event.key === "Home") targetIndex = 0;
                  if (event.key === "End") targetIndex = items.length - 1;
                  if (targetIndex !== null) {
                    event.preventDefault();
                    items[targetIndex]?.focus();
                  }
                }}
                onBlur={(event) => {
                  const next = event.relatedTarget;
                  if (!(next instanceof Node) || !event.currentTarget.contains(next)) rowMenu.close();
                }}
                className="absolute right-0 top-full z-50 mt-4 min-w-[140px] rounded-6 border border-border bg-raised p-4 shadow-lg text-12 text-text"
              >
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                  onClick={() => handleRowAction(row, "open")}
                >
                  Open
                </button>
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                  onClick={() => handleRowAction(row, "why")}
                >
                  Why is this on?
                </button>
                {!row.native && (
                  <>
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                      onClick={() => handleRowAction(row, "promote")}
                    >
                      Promote
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                      onClick={() => handleRowAction(row, "move")}
                    >
                      Move to scope
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                      onClick={() => handleRowAction(row, "duplicate")}
                    >
                      Duplicate
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      className="w-full text-left rounded-4 px-8 py-4 text-danger hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                      onClick={() => handleRowAction(row, "delete")}
                    >
                      Delete
                    </button>
                    {activeProject !== null && (
                      <button
                        type="button"
                        role="menuitem"
                        tabIndex={-1}
                        className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                        onClick={() => handleRowAction(row, "disable-in-repo")}
                      >
                        Disable in this repo
                      </button>
                    )}
                  </>
                )}
              </div>
            )}
          </div>
        )}
      />
      <Modal
        open={updateReview !== null}
        title="Review skill update"
        description={
          updateReview === null
            ? undefined
            : `${updateReview.name} · every changed file is backed up before it is written`
        }
        onOpenChange={(open) => {
          if (!open && updateBusy !== "apply") setUpdateReview(null);
        }}
        footer={
          <>
            <Button variant="ghost" disabled={updateBusy !== null} onClick={() => setUpdateReview(null)}>
              Cancel
            </Button>
            <Button variant="primary" disabled={updateBusy !== null} onClick={() => void handleApplyUpdate()}>
              {updateBusy === "apply" ? "Updating…" : "Apply update"}
            </Button>
          </>
        }
      >
        {updateReview !== null && (
          <ul aria-label="Changed files" className="m-0 grid max-h-[420px] list-none gap-8 overflow-auto p-0">
            {updateReview.preview.files.map((file) => (
              <li key={file.path} className="rounded-6 border border-border p-8">
                <strong className="block break-all font-mono text-11 text-text">{file.path}</strong>
                {file.backup_required && <span className="text-10 text-text-muted">Will be backed up</span>}
                <DiffViewer diff={file.diff} displayPath={file.path} />
              </li>
            ))}
          </ul>
        )}
      </Modal>
      {bulkTransfer !== null && (
        <BulkSkillTransferDialog
          itemIds={selectedRows.filter((row) => !row.native && row.item_id !== "").map((row) => row.item_id)}
          names={selectedRows.map((row) => row.name)}
          blockedItems={selectedRows
            .filter((row) => row.native || row.item_id === "")
            .map((row) => ({
              item_id: skillRowId(row),
              name: row.name,
              reason: "Native skill is not managed by AgenTuna.",
            }))}
          transfer={bulkTransfer}
          projectId={activeProject}
          onClose={() => setBulkTransfer(null)}
          onSuccess={(result) => {
            const verb = bulkTransfer === "promote" ? "promoted" : "moved";
            setBulkTransfer(null);
            setSelectedIds(new Set());
            toast.success(`${result.applied_count} skills ${verb}. Backups: ${result.backup_location}`);
            void load();
          }}
        />
      )}
      <SkillActionDialogs
        action={actionDialog}
        projectId={activeProject}
        onClose={() => setActionDialog(null)}
        onTransferSuccess={(result) => {
          setActionDialog(null);
          toast.success(`Skill transfer complete. Backup: ${result.backup_location}`);
          void load();
        }}
        onDeleteSuccess={(result) => {
          setActionDialog(null);
          toast.success(`Skill deleted. Backup: ${result.backup_folder}`, {
            title: "Skill deleted",
            action: {
              label: "Undo",
              run: () => {
                void invoke("operation_undo", {
                  operationId: result.operation_id,
                  force: false,
                })
                  .then(() => {
                    toast.success("Skill deletion undone");
                    void load();
                  })
                  .catch((cause: unknown) =>
                    toast.warning(parseCommandError(cause).message, { title: "Undo failed" }),
                  );
              },
            },
          });
          void load();
        }}
        onDisableSuccess={(result) => {
          setActionDialog(null);
          toast.success(
            `Disabled in this repo for ${result.agents_disabled} agent(s). Backup: ${result.backup_location}`,
          );
          void load();
        }}
      />
      {whyTrace !== null && (
        <WhyTraceDialog
          open={true}
          itemId={whyTrace.itemId}
          scope={whyTrace.scope}
          agent={whyTrace.agent}
          onClose={() => setWhyTrace(null)}
          onOpenEffective={() => app.navigate("effective")}
        />
      )}
      {adoptModalTarget !== null && adoptModalTarget.native_path && (
        <AdoptModal
          path={adoptModalTarget.native_path}
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
