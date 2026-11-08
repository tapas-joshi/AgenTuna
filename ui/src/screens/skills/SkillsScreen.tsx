/**
 * Skills list screen (TS-028).
 *
 * Virtualized DataTable with per-agent state cells over `at_db::skills_index`,
 * filter chips (All, Global, User, Project, Not in AgenTuna, Shadowed) with live counts,
 * debounced search, shadow/native/lock badges, row actions menu, shared-root coupling
 * explanation, and a mount point for TC-025's Not in AgenTuna banner.
 */

import { lazy, startTransition, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import { useCompactLayout } from "../../components/use-compact-layout";
import { Input } from "../../components/input";
import { Badge } from "../../components/badge";
import type { BadgeTone } from "../../components/badge";
import { Chip } from "../../components/chip";
import { LoadingState } from "../../components/states";
import { SkillUpdateBadge } from "./SkillUpdateBadge";
import { Modal } from "../../overlays/dialogs/Modal";
import { commandErrorMessage } from "../../lib/commandError";
import { DiffViewer } from "../../components/diff";
import {
  DataTable,
  agentColumns,
  agentFolderGroups,
  folderSpan,
  useRowMenu,
  useRowMenuPlacement,
  RowMenuTrigger,
  type AgentDescriptor,
  type AgentFolderGroup,
} from "../../components/table";
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
import type { SkillUpdatePreviewView } from "../../generated/SkillUpdatePreviewView";
import type { SkillUpdateApplyView } from "../../generated/SkillUpdateApplyView";
import { useToast } from "../../overlays/toasts";
import { showAdoptFirstToast } from "../../overlays/toasts/catalogue";
import { useAppSnapshot, useAppState } from "../../state/context";

export type FilterChip = "all" | "global" | "user" | "project" | "native" | "shadowed";

/** Presets load on demand: the tab shares the route-level chunk with the Presets route. */
const PresetsScreen = lazy(() => import("../presets/PresetsScreen").then((m) => ({ default: m.PresetsScreen })));

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

/** Short agent name for status copy: `Claude Code` reads as `Claude only`. */
const AGENT_SHORT_NAME: Readonly<Record<string, string>> = {
  "Claude Code": "Claude",
};

/** Distinct native folder paths recorded for one native skill name. */
function nativeCopyPaths(copies: readonly SkillRowView[]): readonly string[] {
  const paths = new Set<string>();
  for (const other of copies) {
    if (other.native_path) paths.add(other.native_path);
    for (const cell of other.agents) {
      if (cell.state === "native" && cell.config_path) paths.add(cell.config_path);
    }
  }
  return [...paths];
}

/** Agent display names per native folder path for one skill name. */
function nativeOwners(
  copies: readonly SkillRowView[],
  agentLabel: (id: string) => string,
): ReadonlyMap<string, readonly string[]> {
  const owners = new Map<string, Set<string>>();
  for (const other of copies) {
    for (const cell of other.agents) {
      if (cell.state !== "native" || cell.config_path === null) continue;
      const labels = owners.get(cell.config_path) ?? new Set<string>();
      labels.add(agentLabel(cell.agent));
      owners.set(cell.config_path, labels);
    }
  }
  return new Map([...owners].map(([path, labels]) => [path, [...labels]]));
}

const NO_COPIES: readonly SkillRowView[] = [];

/**
 * Cross-row lookups one Skills snapshot needs, built once per snapshot.
 *
 * The STATUS and shadowed-agent cells used to scan all rows for every rendered
 * cell: at 3,000 rows that was millions of comparisons per render and every
 * scroll frame paid it again. Every lookup here is a map read.
 */
export interface SkillLookup {
  /** Native discoveries with exactly this display name, in snapshot order. */
  readonly nativeCopies: (row: SkillRowView) => readonly SkillRowView[];
  /** First same-name copy with a lower scope rank, in snapshot order. */
  readonly overriddenBy: (row: SkillRowView) => SkillRowView | undefined;
  /** First non-native same-name row at `scope`, ignoring case. */
  readonly firstAtScope: (row: SkillRowView, scope: string) => SkillRowView | undefined;
}

/** Builds the {@link SkillLookup} for one snapshot; see its docs for the rules. */
export function buildSkillLookup(rows: readonly SkillRowView[]): SkillLookup {
  const nativeByName = new Map<string, SkillRowView[]>();
  // Name (lowercased) → first array index seen at each scope rank. Array order
  // is the snapshot's order, so the lowest index wins exactly as `find` did.
  const firstIndexByRank = new Map<string, Array<number | undefined>>();
  const firstNonNativeByScope = new Map<string, SkillRowView>();

  rows.forEach((row, index) => {
    const plainName = row.name.toLowerCase();
    if (row.native) {
      const copies = nativeByName.get(row.name);
      if (copies === undefined) nativeByName.set(row.name, [row]);
      else copies.push(row);
    } else {
      const scoped = `${row.scope}\u0000${plainName}`;
      if (!firstNonNativeByScope.has(scoped)) firstNonNativeByScope.set(scoped, row);
    }
    const rank = SCOPE_RANK[row.scope] ?? 0;
    const slots = firstIndexByRank.get(plainName) ?? [];
    if (slots[rank] === undefined) slots[rank] = index;
    firstIndexByRank.set(plainName, slots);
  });

  return {
    nativeCopies: (row) => nativeByName.get(row.name) ?? NO_COPIES,
    overriddenBy: (row) => {
      const slots = firstIndexByRank.get(row.name.toLowerCase());
      if (slots === undefined) return undefined;
      const rank = SCOPE_RANK[row.scope] ?? 0;
      let earliest: number | undefined;
      for (let lower = 0; lower < rank; lower += 1) {
        const index = slots[lower];
        if (index !== undefined && (earliest === undefined || index < earliest)) earliest = index;
      }
      return earliest === undefined ? undefined : rows[earliest];
    },
    firstAtScope: (row, scope) =>
      firstNonNativeByScope.get(`${scope}\u0000${row.name.toLowerCase()}`),
  };
}

/**
 * Derives the STATUS column for one skill row.
 *
 * Every branch names real state read from the list projection: a lock, a
 * higher-precedence copy, the native folders found in agent folders, or a
 * lower-scope copy this row overrides. Native copy wording follows the design
 * (`Claude only`, `2 copies · 1 shared root`, `Shadowed by Project`); nothing
 * is inferred from colour.
 */
export function skillStatus(
  row: SkillRowView,
  lookup: SkillLookup,
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
    const copies = lookup.nativeCopies(row);
    const paths = nativeCopyPaths(copies);
    const owners = nativeOwners(copies, agentLabel);
    const sharedRoot = [...owners.values()].some((labels) => labels.length > 1);
    if (paths.length > 1) {
      return sharedRoot
        ? { label: `${paths.length} copies · 1 shared root`, tone: "info", title: paths.join("\n") }
        : { label: `${paths.length} copies`, tone: "warning", title: paths.join("\n") };
    }
    if (sharedRoot) {
      const readers = Math.max(...[...owners.values()].map((labels) => labels.length));
      return {
        label: `${readers} agents · 1 shared root`,
        tone: "info",
        title: paths.join("\n"),
      };
    }
    const only = [...owners.values()].flat();
    const sole = only[0];
    return {
      label: sole === undefined ? "Native only" : `${AGENT_SHORT_NAME[sole] ?? sole} only`,
      tone: "warning",
      title: paths.join("\n"),
    };
  }
  // A canonical row overrides every lower-precedence copy of its name, including
  // a native copy that lives only in an agent folder.
  const overridden = lookup.overriddenBy(row);
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
  return { code: "error", message: commandErrorMessage(cause) };
}
function skillRowId(row: SkillRowView): string {
  return row.item_id !== "" ? row.item_id : `native:${row.native_path ?? row.name}`;
}

/**
 * Applies the filter chip and the search query to a row set.
 *
 * One helper for every filtering call site so the table, the bulk bar's
 * hidden-count and any future derivation cannot drift apart.
 */
export function applySkillFilters(
  rows: readonly SkillRowView[],
  chip: FilterChip,
  query: string,
): readonly SkillRowView[] {
  let result = rows;
  if (chip === "global") result = result.filter((r) => r.scope === "global");
  else if (chip === "user") result = result.filter((r) => r.scope === "user");
  else if (chip === "project") result = result.filter((r) => r.scope === "project");
  else if (chip === "native") result = result.filter((r) => r.native);
  else if (chip === "shadowed") result = result.filter((r) => Boolean(r.shadowed_by));

  const needle = query.trim().toLowerCase();
  if (needle !== "") {
    result = result.filter((r) => {
      return (
        r.name.toLowerCase().includes(needle) ||
        (r.description !== null && r.description.toLowerCase().includes(needle))
      );
    });
  }
  return result;
}

export function SkillsScreen(
  // The routed caller still passes the scope flags; the shell top bar owns the
  // one scope switcher, so this screen keeps the contract without rendering a
  // second control for the same state.
  _props: {
    readonly localEnabled?: boolean;
    readonly globalLocked?: boolean;
  } = {},
) {
  const { activeProject, externalRefreshVersion } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const rowMenu = useRowMenu();
  const menuPlacement = useRowMenuPlacement(rowMenu.anchor, rowMenu.openRowId !== null);
  useEffect(() => {
    if (rowMenu.openRowId === null) return;
    const row = Array.from(document.querySelectorAll<HTMLElement>("[data-row-menu-id]"))
      .find((element) => element.dataset["rowMenuId"] === rowMenu.openRowId);
    row?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
  }, [rowMenu.openRowId]);

  const [view, setView] = useState<SkillsListView | null>(null);
  /**
   * Table rows commit in a transition (TF-035): `view` updates the toolbar,
   * chips, counts and summary urgently, then the 3,000-row TanStack models
   * build at transition priority while the painted toolbar stays responsive.
   * `null` until the first staged commit arrives; the table shows its loading
   * state meanwhile.
   */
  const [tableRows, setTableRows] = useState<readonly SkillRowView[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"skills" | "presets">(
    app.getSnapshot().screenState["skills:tab"] === "presets" ? "presets" : "skills"
  );
  const [error, setError] = useState<string | null>(null);

  const [activeChip, setActiveChip] = useState<FilterChip>("all");
  const [showAllAgents, setShowAllAgents] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [bannerHidden, setBannerHidden] = useState(false);
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
  /**
   * Preview waiting for the shared-root confirmation. A shared folder changes
   * every agent that reads it, so the write waits until the coupling is shown.
   */
  const [coupledConfirm, setCoupledConfirm] = useState<{
    row: SkillRowView;
    agent: AgentDescriptor;
    preview: SkillAgentPreviewView;
  } | null>(null);
  /** Below the breakpoint the Status column leaves the model; its actions stay. */
  const compactLayout = useCompactLayout();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [listResult, settingsResult] = await Promise.all([
        invoke<SkillsListView>("skills_list", {
          request: { scope: null, project_id: activeProject },
        }),
        invoke<Record<string, unknown>>("settings_load").catch(() => null),
      ]);
      setView(listResult);
      // The staged second commit: row models derive after the toolbar paints.
      startTransition(() => setTableRows(listResult.rows));
      setError(null);
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

  // One pass over the snapshot replaces the per-cell scans (see SkillLookup).
  const skillLookup = useMemo(() => buildSkillLookup(allRows), [allRows]);

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

  const filteredRows = useMemo(
    () => applySkillFilters(tableRows ?? [], activeChip, searchQuery),
    [tableRows, activeChip, searchQuery],
  );
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
    // The design shows the agents this Mac actually has, so a `not_installed`
    // agent keeps its column only while one of its cells still carries a
    // deployment the user must see (linked, off, conflict or native). Hiding a
    // column would hide a drifted or disabled deployment with it; `none` and
    // `not_installed` cells have nothing to show and wait for the toggle.
    return view.agents
      .filter(
        (agent) =>
          showAllAgents ||
          agent.presence !== "not_installed" ||
          allRows.some((row) =>
            row.agents.some(
              (cell) =>
                cell.agent === agent.id &&
                cell.state !== "none" &&
                cell.state !== "not_installed",
            ),
          ),
      )
      .map((agent) => ({
        id: agent.id,
        label: agent.label,
      }));
  }, [view?.agents, allRows, showAllAgents]);

  /**
   * One folder block for agents that share a discovery root. Native cells always
   * carry their folder; a managed cell only proves the folder once a deployment
   * path is recorded, so an unrecorded target stays a separate column.
   */
  const folderGroupsFor = useCallback(
    (row: SkillRowView): ReadonlyMap<string, AgentFolderGroup> =>
      agentFolderGroups(agents, (agent) => {
        const cell = row.agents.find((entry) => entry.agent === agent.id);
        if (cell === undefined) return null;
        if (row.native) return cell.config_path;
        return cell.state === "linked" || cell.state === "conflict" ? cell.config_path : null;
      }),
    [agents],
  );

  /**
   * Bulk agent choices folded by the folder they write for every selected skill.
   * Agents whose cells point at one folder in all selected rows are one checkbox;
   * the coupling note in the picker says why they cannot be selected apart.
   */
  const bulkAgentGroups = useMemo(() => {
    const rows = selectedRows.filter((row) => !row.native && row.item_id !== "");
    if (rows.length === 0) return agents.map((agent) => [agent]);
    const signatures = new Map<string, string | null>();
    for (const agent of agents) {
      const paths = rows.map(
        (row) => row.agents.find((cell) => cell.agent === agent.id)?.config_path ?? null,
      );
      signatures.set(agent.id, paths.every((path) => path !== null) ? paths.join("|") : null);
    }
    const groups: AgentDescriptor[][] = [];
    const groupIndex = new Map<string, number>();
    for (const agent of agents) {
      const signature = signatures.get(agent.id) ?? null;
      if (signature === null) {
        groups.push([agent]);
        continue;
      }
      const existing = groupIndex.get(signature);
      if (existing === undefined) {
        groupIndex.set(signature, groups.length);
        groups.push([agent]);
      } else {
        groups[existing] = [...(groups[existing] ?? []), agent];
      }
    }
    return groups;
  }, [agents, selectedRows]);

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
        if (preview.no_op) {
          toast.info(
            `Already ${next ? "enabled" : "disabled"} for ${agent.label}.`,
            { title: "No change" },
          );
          return;
        }
        // A shared discovery root changes every agent reading the folder. Show
        // that coupling before any file is written instead of applying silently.
        if (preview.coupled_agents.length > 0) {
          setCoupledConfirm({ row, agent, preview });
          return;
        }

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
          // Adopt is a reviewed write: open the dialog instead of adopting silently.
          showAdoptFirstToast(toast, () => setAdoptModalTarget(row));
        } else {
          toast.warning(failure.message, { title: "Could not update skill" });
        }
        await load();
      }
    },
    [activeProject, load, toast]
  );

  /** Applies the reviewed shared-root change after the coupling was shown. */
  const applyCoupledChange = useCallback(async () => {
    if (coupledConfirm === null) return;
    const { row, agent, preview } = coupledConfirm;
    setCoupledConfirm(null);
    try {
      await invoke<SkillAgentApplyView>("skill_agent_apply", {
        previewId: preview.preview_id,
      });
      toast.success(
        `${preview.enabled ? "Enabled" : "Disabled"} ${row.name} for ${agent.label} and the agents that share its folder.`,
        { title: "Shared folder updated" },
      );
      await load();
    } catch (cause) {
      toast.warning(parseCommandError(cause).message, { title: "Could not update skill" });
      await load();
    }
  }, [coupledConfirm, load, toast]);

  /** Drops the unreviewed change and leaves every agent untouched. */
  const cancelCoupledChange = useCallback(() => {
    if (coupledConfirm === null) return;
    void invoke("skill_agent_discard", {
      previewId: coupledConfirm.preview.preview_id,
    }).catch(() => undefined);
    setCoupledConfirm(null);
  }, [coupledConfirm]);


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
      // Compact mode declares a smaller minimum: the column grows to fill, and
      // 160px keeps the whole row inside a 960px window without side scrolling.
      size: compactLayout ? 160 : 280,
      meta: { grow: true },
      cell: ({ row }) => {
        const item = row.original;

        return (
          <div className="flex min-w-0 flex-col gap-2 py-4">
            <div className="flex min-w-0 flex-wrap items-center gap-6">
              <span className="min-w-0 max-w-full truncate font-medium text-text" title={item.name}>{item.name}</span>
              {!item.native && (
                <SkillUpdateBadge
                  row={item}
                  busy={updateBusy === item.item_id}
                  onUpdate={(row) => void handleReviewUpdate(row)}
                  onIgnore={(row) => void handleIgnoreUpdate(row)}
                />
              )}
              {item.native && <Badge tone="muted">not in AgenTuna</Badge>}
            </div>
            {item.description && (
              <span className="truncate text-12 text-text-muted">{item.description}</span>
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
      getItemLabel: (row) => row.name,
      agents,
      getFolderGroups: folderGroupsFor,
      getState: (row, agent) => {
        const cell = row.agents.find((entry) => entry.agent === agent.id);
        if (row.shadowed_by) {
          // A shadowed copy still loads for agents the winning copy does not cover.
          const winner = skillLookup.firstAtScope(row, row.shadowed_by);
          const winnerCell = winner?.agents.find((entry) => entry.agent === agent.id);
          if (winnerCell === undefined || winnerCell.enabled) {
            return { kind: "shadowed", shadowedBy: row.shadowed_by };
          }
        }
        if (cell === undefined) {
          return { kind: "none" };
        }
        if (cell.state === "linked" || cell.state === "conflict") {
          return { kind: "linked" };
        }
        if (cell.state === "off") {
          return { kind: "off" };
        }
        if (cell.state === "native") {
          return {
            kind: "native",
            path: cell.config_path ?? row.native_path ?? "",
            readOnly: true,
          };
        }
        if (cell.state === "shadowed") {
          // The row above names the winning scope when the whole row is shadowed;
          // a cell that is shadowed on its own only proves a nearer copy wins, so
          // say that instead of rendering the agent as `not present`.
          return { kind: "shadowed", shadowedBy: row.shadowed_by ?? "a nearer scope" };
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
        const status = skillStatus(item, skillLookup, (id) =>
          agents.find((agent) => agent.id === id)?.label ?? id,
        );
        const copies = item.native ? nativeCopyPaths(skillLookup.nativeCopies(item)).length : 0;
        return (
          <span data-slot="skill-status" className="flex min-w-0 flex-col items-start gap-4 py-4">
            <Badge tone={status.tone} dot title={status.title}>
              {status.label}
            </Badge>
            {item.native && item.native_path && (
              <span className="flex flex-wrap items-center gap-6">
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={(event) => {
                    event.stopPropagation();
                    setAdoptModalTarget(item);
                  }}
                >
                  Adopt
                </Button>
                {copies > 1 && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={(event) => {
                      event.stopPropagation();
                      app.navigate("migrate");
                    }}
                  >
                    Resolve
                  </Button>
                )}
              </span>
            )}
          </span>
        );
      },
    };

    // Status is the secondary column: dropping it keeps a row inside a 960px
    // window, where its two acts (Adopt, Resolve) move to the row menu.
    return compactLayout
      ? [nameCol, scopeCol, ...agentCols]
      : [nameCol, scopeCol, ...agentCols, statusCol];
  }, [agents, compactLayout, skillLookup, folderGroupsFor, handleIgnoreUpdate, handleReviewUpdate, handleToggleAgent, app, updateBusy]);

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
        selected.flatMap((row) => {
          // A shared folder is written once per row: fold agents by deployed path.
          const folderTaken = new Set<string>();
          const applicableAgents = selectedAgents.filter((agent) => {
            const path = row.agents.find((cell) => cell.agent === agent.id)?.config_path ?? null;
            if (path === null) return true;
            if (folderTaken.has(path)) return false;
            folderTaken.add(path);
            return true;
          });
          return applicableAgents.map(async (agent) => {
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
          });
        }),
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
            app.setScreenState("detail:itemId", "");
            app.setScreenState("detail:nativePath", row.native_path ?? "");
            app.recordVisitedItem(`native-skill:${row.native_path ?? row.name}`);
            app.navigate("skill-detail");
          } else {
            app.setScreenState("detail:nativePath", null);
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

  const coupledAgentLabels =
    coupledConfirm === null
      ? []
      : coupledConfirm.preview.coupled_agents.map(
          (id) => agents.find((agent) => agent.id === id)?.label ?? id,
        );

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
        <Suspense fallback={<LoadingState title="Loading presets…" minHeight={400} />}>
          <PresetsScreen />
        </Suspense>
      </div>
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
          <Button loading={loading} onClick={() => void load()}>Retry</Button>
        </div>
      </main>
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
      {/* Title and actions; the shell top bar owns the one scope switcher. */}
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
          </div>
          <div className="flex flex-wrap items-center gap-8">
            <Button
              variant="secondary"
              onClick={() => {
                // Both entries skip the source picker: the button names the source.
                app.setScreenState("add-skill:source", "git");
                app.navigate("add-skill");
              }}
            >
              Import from git
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                app.setScreenState("add-skill:source", "new");
                app.navigate("add-skill");
              }}
            >
              + New skill
            </Button>
          </div>
        </div>
        <div className="text-12 text-text-muted">
          {counts.all} skill{counts.all === 1 ? "" : "s"} found: {counts.all - counts.native} managed by AgenTuna, {counts.native} still live only in agent folders.
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
        <Button
          size="sm"
          variant="ghost"
          aria-expanded={showAllAgents}
          onClick={() => setShowAllAgents((value) => !value)}
        >
          {showAllAgents ? "Hide inactive agents" : "Show all agents"}
        </Button>
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
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkTransfer("promote")}>Promote to…</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkAgent("enable")}>Enable for agent…</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkTransfer("move")}>Move to scope…</Button>
            <Button size="sm" disabled={bulkBusy} onClick={() => openBulkAgent("disable")}>Disable</Button>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Clear selection"
              disabled={bulkBusy}
              onClick={() => {
                setSelectedIds(new Set());
                setBulkAgentOpen(false);
                setBulkFailures([]);
              }}
            >
              ✕
            </Button>
            {bulkProgress !== null ? <span role="status" className="text-12 text-text-muted">{bulkProgress}</span> : null}
          </div>
          {bulkAgentOpen && (
            <div role="dialog" aria-label={`${bulkMode === "enable" ? "Enable" : "Disable"} skills for agents`} className="grid gap-8 rounded-6 border border-border bg-surface p-12">
              <fieldset className="m-0 grid gap-6 border-0 p-0">
                <legend className="mb-4 text-12 font-semibold text-text">
                  Choose agents for all {selectedRows.length} selected skills
                </legend>
                {bulkAgentGroups.map((group) => {
                  const ids = group.map((agent) => agent.id);
                  const checked = ids.every((id) => bulkAgentIds.has(id));
                  return (
                    <label key={ids.join(":")} className="flex items-center gap-8 text-12 text-text">
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={bulkBusy}
                        onChange={(event) => {
                          setBulkAgentIds((current) => {
                            const next = new Set(current);
                            for (const id of ids) {
                              if (event.target.checked) next.add(id);
                              else next.delete(id);
                            }
                            return next;
                          });
                        }}
                      />
                      {group.map((agent) => agent.label).join(" + ")}
                      {group.length > 1 && (
                        <span className="text-11 text-scope-user">one folder</span>
                      )}
                    </label>
                  );
                })}
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
        rowHeight={84}
        getColumnSpan={(row, columnId) => folderSpan(columnId, folderGroupsFor(row), agents)}
        getRowId={skillRowId}
        getRowLabel={(r) => r.name}
        loading={loading || tableRows === null}
        rowTone={(r) => (r.shadowed_by ? "dimmed" : "default")}
        rowTooltip={(r) =>
          r.shadowed_by
            ? `Shadowed by ${r.shadowed_by}${r.native_path ? ` (${r.native_path})` : ""}`
            : undefined
        }
        emptyTitle="No skills found"
        emptyBody="Add a skill or adjust your filters."
        emptyAction={{
          label: "+ New skill",
          onClick: () => {
            app.setScreenState("add-skill:source", "new");
            app.navigate("add-skill");
          },
        }}
        rowActions={(row) => (
          <div className="relative flex items-center justify-center" data-row-menu data-row-menu-id={skillRowId(row)}>
            <RowMenuTrigger
              label={`Actions for ${row.name}`}
              open={rowMenu.isOpen(skillRowId(row))}
              disabled={bulkBusy}
              onToggle={(event) => rowMenu.toggle(skillRowId(row), event.currentTarget)}
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
                className={`absolute right-0 z-50 min-w-[140px] rounded-6 border border-border bg-raised p-4 shadow-lg text-12 text-text overflow-y-auto overscroll-contain ${
                  menuPlacement.side === "above" ? "bottom-full mb-4" : "top-full mt-4"
                }`}
                style={{ maxHeight: menuPlacement.maxHeight }}
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
                {row.native && row.native_path && (
                  <>
                    {/* The Status column owns these below the breakpoint; the
                        menu keeps them reachable once that column is dropped. */}
                    <button
                      type="button"
                      role="menuitem"
                      tabIndex={-1}
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                      onClick={() => {
                        rowMenu.close();
                        setAdoptModalTarget(row);
                      }}
                    >
                      Adopt
                    </button>
                    {nativeCopyPaths(skillLookup.nativeCopies(row)).length > 1 && (
                      <button
                        type="button"
                        role="menuitem"
                        tabIndex={-1}
                        className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus"
                        onClick={() => {
                          rowMenu.close();
                          app.navigate("migrate");
                        }}
                      >
                        Resolve copies
                      </button>
                    )}
                  </>
                )}
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
        open={coupledConfirm !== null}
        title="Shared discovery root"
        description={
          coupledConfirm === null
            ? undefined
            : `${coupledConfirm.agent.label}${coupledAgentLabels.length > 0 ? ` and ${coupledAgentLabels.join(" + ")}` : ""} read the same folder on disk, so one change writes the folder both agents load.`
        }
        onOpenChange={(open) => {
          if (!open) cancelCoupledChange();
        }}
        footer={
          <>
            <Button variant="ghost" onClick={cancelCoupledChange}>
              Cancel
            </Button>
            <Button variant="primary" onClick={() => void applyCoupledChange()}>
              {coupledConfirm?.preview.enabled ? "Enable for both agents" : "Disable for both agents"}
            </Button>
          </>
        }
      >
        {coupledConfirm !== null && (
          <div className="grid gap-12">
            <div
              role="note"
              className="rounded-8 border border-warning/40 bg-warning/10 p-12 text-12 text-text"
            >
              <strong className="block text-13">One folder, not two switches</strong>
              <span className="mt-4 block text-text-secondary">
                {coupledConfirm.preview.warnings[0] ??
                  `A shared discovery root means enabling or disabling this skill changes every agent that reads the folder; AgenTuna never offers a separate switch for it.`}
              </span>
            </div>
            <dl className="m-0 grid gap-6 text-12">
              <div>
                <dt className="text-11 text-text-muted">Folder</dt>
                <dd className="m-0 mt-2 break-all font-mono text-11 text-text-secondary">
                  {coupledConfirm.preview.paths.length > 0
                    ? coupledConfirm.preview.paths.join("\n")
                    : coupledConfirm.row.native_path ?? "No path recorded"}
                </dd>
              </div>
            </dl>
          </div>
        )}
      </Modal>
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
                {file.backup_required && <span className="text-11 text-text-muted">Will be backed up</span>}
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
