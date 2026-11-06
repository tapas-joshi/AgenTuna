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
import { Chip } from "../../components/chip";
import { PresetsScreen } from "../presets/PresetsScreen";
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
import { WhyTraceDialog } from "../../overlays/dialogs/WhyTraceDialog";
import type { SkillAgentPreviewRequest } from "../../generated/SkillAgentPreviewRequest";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import type { AdoptPreviewView } from "../../generated/AdoptPreviewView";
import type { AdoptApplyView } from "../../generated/AdoptApplyView";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";

export type FilterChip = "all" | "global" | "user" | "project" | "native" | "shadowed";

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
  local: "bg-scope-project/15 text-scope-project",
};

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

export function SkillsScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const rowMenu = useRowMenu();

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
  const [whyTrace, setWhyTrace] = useState<{ itemId: string; scope: string; agent: string } | null>(null);

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
  }, [activeProject]);

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

  const agents: readonly AgentDescriptor[] = useMemo(() => {
    if (!view?.agents) return [];
    return view.agents.map((a) => ({
      id: a.id,
      label: a.label,
    }));
  }, [view?.agents]);

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
          preview_id: preview.preview_id,
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
          failure.code.toLowerCase().includes("conflict") ||
          failure.message.toLowerCase().includes("unmanaged skill folder")
        ) {
          toast.warning(
            `${row.name} has an unmanaged copy in ${agent.label}; adopt or move it aside first.`,
            {
              title: "Native conflict",
            }
          );
        } else {
          toast.warning(failure.message, { title: "Could not update skill" });
        }
        await load();
      }
    },
    [activeProject, load, toast]
  );

  const handleAdopt = useCallback(
    async (row: SkillRowView) => {
      if (!row.native_path) return;
      setAdoptingPath(row.native_path);
      try {
        const preview = await invoke<AdoptPreviewView>("adopt_preview", {
          path: row.native_path,
          expected_hash: null,
          handling: "link",
        });
        await invoke<AdoptApplyView>("adopt_apply", {
          path: preview.source,
          expected_hash: null,
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

  const handleHideBanner = useCallback(async () => {
    try {
      await invoke("settings_set", { key: "native_banner_hidden.skill", value: true });
      setBannerHidden(true);
    } catch {
      setBannerHidden(true);
    }
  }, []);

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
              {item.update_available && (
                <Badge tone="info">
                  update
                </Badge>
              )}
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
              <div className="flex flex-wrap items-center gap-6 pt-2">
                <span className="font-mono text-11 text-text-muted truncate max-w-[220px]" title={item.native_path}>
                  {item.native_path}
                </span>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={isAdopted || isAdopting}
                  onClick={(e) => {
                    e.stopPropagation();
                    void handleAdopt(item);
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

    return [nameCol, scopeCol, ...agentCols];
  }, [agents, adoptingPath, handleAdopt, handleToggleAgent, app]);

  const handleRowAction = useCallback(
    (row: SkillRowView, action: string) => {
      rowMenu.close();
      switch (action) {
        case "open":
          if (row.native) {
            app.navigate("migrate");
          } else {
            app.setSelection("skill-detail", [row.item_id]);
            app.navigate("skill-detail");
          }
          break;
        case "promote":
          toast.info(`Promoting ${row.name} to higher scope`);
          break;
        case "move":
          toast.info(`Move ${row.name} to scope`);
          break;
        case "duplicate":
          toast.info(`Duplicate ${row.name}`);
          break;
        case "delete":
          toast.info(`Delete ${row.name}`);
          break;
        case "why":
          setWhyTrace({ itemId: row.item_id, scope: row.scope, agent: "claude_code" });
          break;
        default:
          break;
      }
    },
    [app, rowMenu, toast]
  );

  if (activeTab === "presets") {
    return (
      <div className="flex flex-col">
        <div className="mx-auto flex w-full max-w-[1200px] items-center justify-between px-24 pt-16">
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
        </div>
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
    <main aria-label="Skills" className="mx-auto flex max-w-[1200px] flex-col gap-16 p-24">
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
              localEnabled={false}
              hasProject={Boolean(activeProject)}
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

      {/* Virtualized DataTable */}
      <DataTable<SkillRowView>
        ariaLabel="Skills"
        data={filteredRows}
        columns={columns}
        viewportHeight={600}
        selectionMode="none"
        getRowId={(r) => r.item_id || r.native_path || r.name}
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
          <div className="relative flex items-center justify-center" data-row-menu>
            <RowMenuTrigger
              label={`Actions for ${row.name}`}
              open={rowMenu.isOpen(row.item_id || row.name)}
              onToggle={() => rowMenu.toggle(row.item_id || row.name)}
            />
            {rowMenu.isOpen(row.item_id || row.name) && (
              <div
                role="menu"
                data-row-menu
                className="absolute right-0 top-full z-50 mt-4 min-w-[140px] rounded-6 border border-border bg-raised p-4 shadow-lg text-12 text-text"
              >
                <button
                  type="button"
                  role="menuitem"
                  className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface"
                  onClick={() => handleRowAction(row, "open")}
                >
                  Open
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface"
                  onClick={() => handleRowAction(row, "why")}
                >
                  Why is this on?
                </button>
                {!row.native && (
                  <>
                    <button
                      type="button"
                      role="menuitem"
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface"
                      onClick={() => handleRowAction(row, "promote")}
                    >
                      Promote
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface"
                      onClick={() => handleRowAction(row, "move")}
                    >
                      Move to scope
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="w-full text-left rounded-4 px-8 py-4 hover:bg-surface"
                      onClick={() => handleRowAction(row, "duplicate")}
                    >
                      Duplicate
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="w-full text-left rounded-4 px-8 py-4 text-danger hover:bg-surface"
                      onClick={() => handleRowAction(row, "delete")}
                    >
                      Delete
                    </button>
                  </>
                )}
              </div>
            )}
          </div>
        )}
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
    </main>
  );
}
