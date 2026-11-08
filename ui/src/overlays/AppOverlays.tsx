import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useRef, useState } from "react";
import type { JSX, ReactNode } from "react";
import type { ProjectView } from "../generated/ProjectView";
import type { JobFinishedEvent } from "../generated/JobFinishedEvent";
import { AddMenu } from "./menus/AddMenu";
import type { AddMenuAction } from "./menus/AddMenu";
import { Palette } from "./palette/Palette";
import { buildPaletteIndex } from "./palette";
import type { PaletteInventory } from "./palette";
import { ProjectPicker, relativeLastUsed } from "./project_picker";
import type { ProjectPickerProject } from "./project_picker";
import type { AppState } from "../state/AppState";
import type { StateRouteId } from "../components/states";
import type { CliInstallStatus } from "../generated/CliInstallStatus";
import type { CliInstallReport } from "../generated/CliInstallReport";
import type { CliInstallLocation } from "../generated/CliInstallLocation";
import type { CliUninstallReport } from "../generated/CliUninstallReport";
import type { RecoverySheetView } from "../generated/RecoverySheetView";
import type { SkillsListView } from "../generated/SkillsListView";
import type { McpServerListView } from "../generated/McpServerListView";
import type { InstructionFileView } from "../generated/InstructionFileView";
import type { AgentCardView } from "../generated/AgentCardView";
import { shellAgentStatus } from "../shell/agents";
import type { SkillsListRequest } from "../generated/SkillsListRequest";
import type { McpServerListRequest } from "../generated/McpServerListRequest";
import type { ExternalRefreshEvent } from "../generated/ExternalRefreshEvent";
import { RecoveryGate } from "./recovery/RecoverySheet";
import { Modal } from "./dialogs/Modal";
import { WhyTraceDialog } from "./dialogs/WhyTraceDialog";
import type { AccountMenuAction } from "./menus/AccountMenu";
import type { ScopeToken } from "../theme";
import { ACTION_IDS, SHORTCUTS, routeForAction, shortcutHint } from "../shell/actions";
import type { AppActionId } from "../shell/actions";
import { isAppActionId } from "../shell/actions";
import { OverwriteRefusalDialog, refusedOverwriteDetails } from "./dialogs/OverwriteRefusalDialog";
import { parseRouteId } from "../shell/routes";
import type { RefusedOverwriteDetails } from "./dialogs/OverwriteRefusalDialog";
import { commandErrorMessage } from "../lib/commandError";
import { applyAdoption, moveRefusedItemAside, previewAdoption, undoMoveAside } from "./dialogs/overwrite-commands";
function projectRow(project: ProjectView): ProjectPickerProject {
  const pathParts = project.path.split(/[\\/]+/).filter(Boolean);
  const shortPath = pathParts.length > 2 ? `…/${pathParts.slice(-2).join("/")}` : project.path;
  return { id: project.id, name: project.name, path: shortPath, skillCount: Number(project.skill_count), serverCount: Number(project.server_count), lastUsed: relativeLastUsed(project.last_used_at) };
}

/** Mounted top-bar controls and the two modal overlay surfaces. */
export interface AppOverlaySlots {
  readonly addControl: JSX.Element;
  readonly onProjectPicker: () => void;
  readonly onPalette: () => void;
  readonly onAccountAction: (action: AccountMenuAction) => void;
  readonly accountDisabledReasons?: Partial<Record<AccountMenuAction, string>>;
  readonly overlays: ReactNode;
}

/** App-shell composition point for shared, state-backed overlays. */
export function AppOverlays({
  state,
  scope,
  route,
  hasProject,
  projectId,
  appVersion,
  scopeAvailability,
  accountDisabledReasons: customAccountDisabledReasons,
  activeMigration = false,
  children,
}: {
  readonly state: AppState;
  readonly scope: ScopeToken;
  readonly route: StateRouteId;
  readonly hasProject: boolean;
  readonly projectId: string | null;
  readonly appVersion: string;
  /** Live scope availability from the backend; absent means "not checked yet". */
  readonly scopeAvailability?: { readonly localAvailable: boolean; readonly globalAvailable: boolean };
  readonly accountDisabledReasons?: Partial<Record<AccountMenuAction, string>>;
  readonly activeMigration?: boolean;
  readonly children: (slots: AppOverlaySlots) => JSX.Element;
}): JSX.Element {
  const [projectOpen, setProjectOpen] = useState(false);
  const [requiredProjectRoute, setRequiredProjectRoute] = useState<StateRouteId | null>(null);
  const [requiredProjectItemId, setRequiredProjectItemId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [projects, setProjects] = useState<readonly ProjectPickerProject[]>([]);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [scanJob, setScanJob] = useState<string | null>(null);
  const [accountDialog, setAccountDialog] = useState<string | null>(null);
  const [signingStatus, setSigningStatus] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<RecoverySheetView | null>(null);
  const [cliStatus, setCliStatus] = useState<CliInstallStatus | null>(null);
  const [cliError, setCliError] = useState<string | null>(null);
  const [installingCli, setInstallingCli] = useState(false);
  const [cliInstallConfirm, setCliInstallConfirm] = useState<CliInstallLocation | null>(null);
  const [cliUninstallConfirm, setCliUninstallConfirm] = useState<CliInstallLocation | null>(null);
  const [cliInstallReport, setCliInstallReport] = useState<CliInstallReport | null>(null);
  const [cliUninstallReport, setCliUninstallReport] = useState<CliUninstallReport | null>(null);
  const [loadingCliStatus, setLoadingCliStatus] = useState(false);
  const [doctorReport, setDoctorReport] = useState<string | null>(null);
  const [overwriteRefusal, setOverwriteRefusal] = useState<RefusedOverwriteDetails | null>(null);
  const [whyPalette, setWhyPalette] = useState<{ itemId: string; scope: string; agent: string } | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const openModalIds = useRef(new Set<string>());
  const [paletteInventory, setPaletteInventory] = useState<PaletteInventory>({ skills: [], servers: [], rules: [], agents: [], actions: [] });
  const [paletteLoading, setPaletteLoading] = useState(false);
  const [paletteError, setPaletteError] = useState<string | null>(null);
  const [paletteRetry, setPaletteRetry] = useState(0);
  const [addAgentState, setAddAgentState] = useState<"unknown" | "checking" | "available" | "missing">("unknown");
  const [globalAddAccess, setGlobalAddAccess] = useState<{ readonly state: "unknown" | "checking" | "writable" | "readonly"; readonly reason: string | null }>({ state: "unknown", reason: null });
  useEffect(() => {
    if (!addOpen) return;
    let active = true;
    setAddAgentState("checking");
    void invoke<AgentCardView[]>("agents_list").then((agents) => {
      if (!active) return;
      setAddAgentState(Array.isArray(agents) ? (agents.some((agent) => agent.installed) ? "available" : "missing") : "unknown");
    }, () => {
      if (active) setAddAgentState("unknown");
    });
    return () => { active = false; };
  }, [addOpen]);
  useEffect(() => {
    if (!addOpen || scope !== "global") return;
    let active = true;
    setGlobalAddAccess({ state: "checking", reason: null });
    void invoke<{ readonly global_store?: { readonly status?: string; readonly reason?: string | null } }>("overview_load", { activeScope: scope }).then((overview) => {
      if (!active) return;
      if (typeof overview !== "object" || overview === null) {
        setGlobalAddAccess({ state: "unknown", reason: null });
        return;
      }
      const status = overview.global_store?.status;
      if (status === undefined) {
        setGlobalAddAccess({ state: "unknown", reason: null });
        return;
      }
      setGlobalAddAccess({ state: status === "available" ? "writable" : "readonly", reason: overview.global_store?.reason ?? null });
    }, () => {
      if (active) setGlobalAddAccess({ state: "unknown", reason: null });
    });
    return () => { active = false; };
  }, [addOpen, scope]);

  const reloadProjects = async () => {
    try {
      const rows = await invoke<ProjectView[]>("projects_list");
      setProjects(rows.map(projectRow));
      setProjectError(null);
    } catch (error) {
      setProjectError(commandErrorMessage(error));
    }
  };
  const openProjects = () => {
    setProjectOpen(true);
    void reloadProjects();
  };
  const selectProject = (projectId: string) => {
    const target = requiredProjectRoute;
    const itemId = requiredProjectItemId;
    setRequiredProjectRoute(null);
    setRequiredProjectItemId(null);
    void state.selectProject(projectId).then(() => {
      if (target !== null) {
        if (itemId !== null) state.setScreenState("detail:itemId", itemId);
        state.navigate(target);
      }
    });
  };
  const cliErrorMessage = (error: unknown): string => commandErrorMessage(error, "The CLI status could not be loaded.");
  const loadCliStatus = () => {
    setCliError(null);
    setLoadingCliStatus(true);
    void invoke<CliInstallStatus>("cli_install_status").then(setCliStatus, (error: unknown) => {
      setCliStatus(null);
      setCliError(cliErrorMessage(error));
    }).finally(() => setLoadingCliStatus(false));
  };
  const installCli = (location: CliInstallLocation) => {
    setInstallingCli(true);
    setCliError(null);
    void invoke<CliInstallReport>("cli_install", { location }).then((report) => {
      setCliInstallReport(report);
      setCliInstallConfirm(null);
      loadCliStatus();
    }, (error: unknown) => {
      setCliError(cliErrorMessage(error));
      setCliInstallConfirm(null);
    }).finally(() => setInstallingCli(false));
  };
  const uninstallCli = (location: CliInstallLocation) => {
    setInstallingCli(true);
    setCliError(null);
    void invoke<CliUninstallReport>("cli_uninstall", { location }).then((report) => {
      setCliUninstallReport(report);
      setCliUninstallConfirm(null);
      loadCliStatus();
    }, (error: unknown) => {
      setCliError(cliErrorMessage(error));
      setCliUninstallConfirm(null);
    }).finally(() => setInstallingCli(false));
  };
  useEffect(() => {
    const handleProjectRequired = (event: Event) => {
      const detail = (event as CustomEvent<unknown>).detail;
      if (typeof detail !== "object" || detail === null || !("route" in detail) || typeof detail.route !== "string") return;
      const target = parseRouteId(detail.route);
      if (target === null) return;
      setRequiredProjectRoute(target);
      setRequiredProjectItemId("itemId" in detail && typeof detail.itemId === "string" ? detail.itemId : null);
      openProjects();
    };
    window.addEventListener("agentuna:project-picker-required", handleProjectRequired);
    return () => window.removeEventListener("agentuna:project-picker-required", handleProjectRequired);
  }, []);
  const dispatch = (id: AppActionId, actionScope: ScopeToken = scope) => {
    if (id.startsWith("scope:")) {
      const nextScope = id.slice("scope:".length);
      if (nextScope !== "global" && nextScope !== "user" && nextScope !== "project" && nextScope !== "local") return;
      // The switcher disables these options; the keyboard path must refuse them too instead of
      // selecting a scope whose store or project is unavailable.
      if (nextScope === "global" && scopeAvailability?.globalAvailable === false) return;
      if (nextScope === "project" && !hasProject) return;
      if (nextScope === "local" && (!hasProject || scopeAvailability?.localAvailable === false)) return;
      state.receiveScope(nextScope);
      return;
    }
    if (id === ACTION_IDS.addMenu) { setAddOpen(true); return; }
    if (id === ACTION_IDS.palette) { setPaletteOpen(true); return; }
    if (id === ACTION_IDS.projectPicker) { openProjects(); return; }
    if (id === ACTION_IDS.save) {
      const editor = state.getSnapshot().editors[state.getSnapshot().route];
      if (editor?.onSave) void editor.onSave();
      else state.toasts.show({ kind: "info", title: "Nothing to save", message: "No editor is focused." });
      return;
    }
    if (id === ACTION_IDS.shortcuts) { state.setOverlay("keyboard-shortcuts"); return; }
    if (id === ACTION_IDS.about) {
      setAccountDialog("about");
      void invoke<string>("app_signing_status").then(setSigningStatus, () => setSigningStatus(null));
      return;
    }
    if (id === ACTION_IDS.docs) { setAccountDialog("documentation"); return; }
    if (id === ACTION_IDS.rescan) {
      void invoke<string>("projects_scan").then(setScanJob, (error: unknown) => setProjectError(commandErrorMessage(error)));
      // Detection can change between scans; the sidebar shows the same command's answer.
      void invoke<AgentCardView[]>("agents_rescan").then(
        (cards) => {
          if (Array.isArray(cards)) state.receiveAgents(shellAgentStatus(cards));
        },
        () => undefined,
      );
      return;
    }
    if (id === ACTION_IDS.installCli) {
      setCliInstallReport(null);
      setCliUninstallReport(null);
      setCliInstallConfirm(null);
      setCliUninstallConfirm(null);
      loadCliStatus();
      return;
    }
    if (id === ACTION_IDS.doctor) {
      setDoctorReport(null);
      void invoke<string>("diagnostics_report_copy").then(setDoctorReport, (error: unknown) => state.toasts.show({ kind: "error", title: "Diagnostics unavailable", message: commandErrorMessage(error) }));
      return;
    }
    const target = routeForAction(id);
    if (target) {
      if (id === ACTION_IDS.addSkill || id === ACTION_IDS.addServer || id === ACTION_IDS.addRule) state.receiveScope(actionScope);
      if (id === ACTION_IDS.addRule) {
        state.setScreenState("instructions:newRuleOpen", true);
        if (route === "instructions") window.dispatchEvent(new CustomEvent("agentuna:open-new-rule"));
      }
      state.navigate(target);
      setAddOpen(false);
      setPaletteOpen(false);
      setProjectOpen(false);
    }
  };
  const effectiveAccountDisabledReasons: Partial<Record<AccountMenuAction, string>> = {
    ...(scanJob !== null ? { backups: "Project scan in progress" } : {}),
    ...(activeMigration ? { backups: "Migration in progress" } : {}),
    ...customAccountDisabledReasons,
  };
  const onAccountAction = (action: AccountMenuAction) => {
    if (effectiveAccountDisabledReasons[action] !== undefined) {
      return;
    }
    if (action === "preferences") { dispatch(ACTION_IDS.preferences); return; }
    if (action === "agents") { dispatch(ACTION_IDS.agents); return; }
    if (action === "secrets") { state.navigate("secrets"); return; }
    setAccountDialog(action);
  };
  const runAdd = (action: AddMenuAction) => {
    const actionId: Readonly<Record<AddMenuAction["id"], AppActionId>> = {
      "add.skill": ACTION_IDS.addSkill,
      "add.server": ACTION_IDS.addServer,
      "add.rule": ACTION_IDS.addRule,
    };
    dispatch(actionId[action.id], action.scope);
  };
  useEffect(() => {
    if (!paletteOpen) return;
    let active = true;
    setPaletteLoading(true);
    setPaletteError(null);
    setPaletteInventory({ skills: [], servers: [], rules: [], agents: [], actions: [] });
    const skillsRequest: SkillsListRequest = { scope: null, project_id: projectId };
    const serversRequest: McpServerListRequest = { scope: "", project_id: projectId };
    void Promise.all([
      invoke<SkillsListView>("skills_list", { request: skillsRequest }),
      invoke<McpServerListView>("mcp_server_list", { request: serversRequest }),
      invoke<InstructionFileView[]>("instructions_list", { projectId }),
      invoke<AgentCardView[]>("agents_list"),
    ]).then(([skills, servers, rules, agents]) => {
      if (!active) return;
      setPaletteInventory({
        skills: skills.rows.map((row) => {
          const sourceId = row.item_id !== "" ? row.item_id : `native:${row.native_path ?? row.name}`;
          return {
            id: `skill:${sourceId}`,
            title: row.name,
            subtitle: `${row.scope} · Skill${row.description ? ` · ${row.description}` : ""}`,
            keywords: [row.scope, ...(row.description === null ? [] : [row.description]), ...row.agents.map((agent) => agent.agent)],
            run: () => {
              if (row.item_id === "") {
                state.recordVisitedItem(`skill:${sourceId}`);
                dispatch(ACTION_IDS.skills);
              } else {
                state.openItem(row.item_id, { kind: "skill", scope: row.scope, project_id: skills.project_id });
              }
            },
          };
        }),
        servers: servers.servers.map((row) => {
          const sourceId = row.id !== "" ? row.id : `native:${row.source_path ?? row.name}`;
          return {
            id: `server:${sourceId}`,
            title: row.name,
            subtitle: `${row.scope} · MCP server · ${row.transport}`,
            keywords: [row.scope, row.transport, ...row.agents.map((agent) => agent.agent)],
            run: () => {
              if (row.id === "") {
                state.recordVisitedItem(`server:${sourceId}`);
                dispatch(ACTION_IDS.servers);
              } else {
                state.openItem(row.id, { kind: "mcp_server", scope: row.scope, project_id: servers.project_id });
              }
            },
          };
        }),
        rules: rules.map((file) => ({
          id: `rule:${file.item_id}`,
          title: file.path.split(/[\\/]+/).at(-1) ?? file.path,
          subtitle: `${file.scope} · Rule file`,
          keywords: [file.scope, file.path, ...file.agents],
          run: () => state.openItem(file.item_id, { kind: "rule", scope: file.scope, project_id: projectId }),
        })),
        agents: agents.map((agent) => ({
          id: `agent:${agent.id}`,
          title: agent.name,
          subtitle: `${agent.installed ? "Installed" : "Known"} · ${agent.health}`,
          keywords: [agent.id, agent.presence, ...agent.paths],
          run: () => dispatch(ACTION_IDS.agents),
        })),
        actions: [],
      });
    }, (error: unknown) => {
      if (!active) return;
      setPaletteError(commandErrorMessage(error));
    }).finally(() => {
      if (active) setPaletteLoading(false);
    });
    return () => { active = false; };
  }, [paletteOpen, paletteRetry, projectId, scope, state]);

  const paletteIndex = useMemo(() => buildPaletteIndex({
    ...paletteInventory,
    actions: [
      { id: ACTION_IDS.overview, title: "Overview", subtitle: "Open overview", keywords: ["home"], run: () => dispatch(ACTION_IDS.overview), shortcut: shortcutHint("navigate:overview") },
      { id: ACTION_IDS.skills, title: "Skills", subtitle: "Open skills", keywords: ["browse", "manage"], run: () => dispatch(ACTION_IDS.skills), shortcut: shortcutHint(ACTION_IDS.skills) },
      { id: ACTION_IDS.servers, title: "MCP Servers", subtitle: "Open MCP servers", keywords: ["mcp", "server", "manage"], run: () => dispatch(ACTION_IDS.servers), shortcut: shortcutHint(ACTION_IDS.servers) },
      { id: ACTION_IDS.instructions, title: "Rule files", subtitle: "Open instructions", keywords: ["rules", "files"], run: () => dispatch(ACTION_IDS.instructions), shortcut: shortcutHint(ACTION_IDS.instructions) },
      { id: ACTION_IDS.addSkill, title: "New skill", subtitle: `Add to ${scope} scope`, keywords: ["create", "add"], run: () => dispatch(ACTION_IDS.addSkill), shortcut: shortcutHint(ACTION_IDS.addSkill), disabledReason: (scope === "project" || scope === "local") && !hasProject ? "Choose a project first" : undefined },
      { id: ACTION_IDS.addServer, title: "Add MCP server", subtitle: `Add to ${scope} scope`, keywords: ["mcp", "server", "create"], run: () => dispatch(ACTION_IDS.addServer) },
      { id: ACTION_IDS.addRule, title: "New rule file", subtitle: `Add to ${scope} scope`, keywords: ["rule", "file", "create"], run: () => dispatch(ACTION_IDS.addRule) },
      { id: ACTION_IDS.preferences, title: "Preferences", subtitle: "Open preferences", keywords: ["settings"], run: () => dispatch(ACTION_IDS.preferences), shortcut: shortcutHint(ACTION_IDS.preferences) },
      { id: ACTION_IDS.projectPicker, title: "Choose project", subtitle: "Open project picker", keywords: ["switch project"], run: () => dispatch(ACTION_IDS.projectPicker), shortcut: shortcutHint(ACTION_IDS.projectPicker) },
      { id: ACTION_IDS.import, title: "Import native items", subtitle: "Review and import existing configuration", keywords: ["move", "adopt", "migrate"], run: () => dispatch(ACTION_IDS.import) },
      { id: ACTION_IDS.docs, title: "Documentation", subtitle: "Learn about AgenTuna", keywords: ["help"], run: () => dispatch(ACTION_IDS.docs) },
      { id: ACTION_IDS.doctor, title: "Run Doctor", subtitle: "Export a diagnostics report", keywords: ["diagnostics", "health"], run: () => dispatch(ACTION_IDS.doctor) },
      { id: ACTION_IDS.rescan, title: "Rescan projects", subtitle: "Refresh the project registry", keywords: ["scan", "refresh"], run: () => dispatch(ACTION_IDS.rescan) },
      { id: ACTION_IDS.installCli, title: "Install CLI", subtitle: "Install the agentuna command-line tool", keywords: ["command line", "terminal"], run: () => dispatch(ACTION_IDS.installCli) },
      { id: ACTION_IDS.whyTrace, title: "Why is this on?", subtitle: "Trace scope decision for an item", keywords: ["trace", "why", "effective", "winner"], run: () => {
        const selected = state.getSnapshot().selections[state.getSnapshot().route]?.[0] ?? "active";
        setWhyPalette({ itemId: selected, scope, agent: "claude_code" });
      } },
    ],
  }), [hasProject, paletteInventory, scope, state]);

  useEffect(() => {
    const dispatchMenu = (event: Event) => {
      const action = (event as CustomEvent<unknown>).detail;
      if (typeof action === "string" && isAppActionId(action)) dispatch(action);
    };
    window.addEventListener("agentuna:menu-action", dispatchMenu);
    const keydown = (event: KeyboardEvent) => {
      if (event.altKey) return;
      const primary = event.metaKey || event.ctrlKey;
      if (event.key === "Escape") {
        if (addOpen) { event.preventDefault(); setAddOpen(false); }
        else if (paletteOpen) { event.preventDefault(); setPaletteOpen(false); }
        else if (projectOpen) { event.preventDefault(); setProjectOpen(false); }
        else if (state.getSnapshot().overlay !== null) { event.preventDefault(); state.setOverlay(null); }
        return;
      }
      if (!primary) return;
      const modifier = event.shiftKey ? "primary-shift" : "primary";
      const shortcut = SHORTCUTS.find((item) => item.modifier === modifier && item.key.toLowerCase() === event.key.toLowerCase());
      if (!shortcut) return;
      if (!isAppActionId(shortcut.action)) return;
      event.preventDefault();
      dispatch(shortcut.action);
    };
    window.addEventListener("keydown", keydown, true);
    return () => {
      window.removeEventListener("keydown", keydown, true);
      window.removeEventListener("agentuna:menu-action", dispatchMenu);
    };
  }, [addOpen, paletteOpen, projectOpen, scope, state]);
  useEffect(() => {
    const modalState = (event: Event) => {
      const detail = (event as CustomEvent<unknown>).detail;
      if (typeof detail !== "object" || detail === null || !("id" in detail) || typeof detail.id !== "string" || !("open" in detail) || typeof detail.open !== "boolean") return;
      if (detail.open) openModalIds.current.add(detail.id);
      else openModalIds.current.delete(detail.id);
      setModalOpen(openModalIds.current.size > 0);
    };
    window.addEventListener("agentuna:modal-state", modalState);
    return () => window.removeEventListener("agentuna:modal-state", modalState);
  }, []);
  useEffect(() => {
    void invoke("app_menu_set_add_skill_enabled", { enabled: !modalOpen }).catch((error: unknown) => {
      state.toasts.show({ kind: "error", title: "Native menu unavailable", message: commandErrorMessage(error) });
    });
  }, [modalOpen, state]);

  useEffect(() => {
    if (!scanJob) return;
    let unlisten: (() => void) | undefined;
    void listen<JobFinishedEvent>("agentuna:job-finished", ({ payload }) => {
      if (payload.id !== scanJob) return;
      setScanJob(null);
      void reloadProjects();
    }).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, [scanJob]);
  useEffect(() => {
    const handleRefusal = (event: Event) => {
      setOverwriteRefusal(refusedOverwriteDetails((event as CustomEvent<unknown>).detail));
    };
    window.addEventListener("agentuna:refused-overwrite", handleRefusal);
    return () => window.removeEventListener("agentuna:refused-overwrite", handleRefusal);
  }, []);
  useEffect(() => {
    void invoke<RecoverySheetView | null>("recovery_sheet").then(
      setRecovery,
      (error: unknown) => state.toasts.show({ kind: "error", title: "Recovery status unavailable", message: commandErrorMessage(error) }),
    );
  }, [state]);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<ExternalRefreshEvent>("agentuna:external-refresh", ({ payload }) => {
      state.requestExternalReload();
      window.dispatchEvent(new CustomEvent("agentuna:external-refresh", { detail: payload }));
    }).then(
      (dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      },
      () => undefined,
    );
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [state]);

  const cliInstallTarget = cliStatus !== null && cliInstallConfirm !== null ? cliStatus[cliInstallConfirm] : null;
  const cliUninstallTarget = cliStatus !== null && cliUninstallConfirm !== null ? cliStatus[cliUninstallConfirm] : null;
  const overlays = <>
    <ProjectPicker open={projectOpen} projects={projects} currentProjectId={state.getSnapshot().activeProject} error={projectError} scanning={scanJob !== null} onClose={() => { setProjectOpen(false); setRequiredProjectRoute(null); setRequiredProjectItemId(null); }} onSelect={(project) => selectProject(project.id)} onAddFolder={() => {
      void invoke<ProjectView | null>("projects_choose_folder").then((project) => {
        if (project) { setProjects((current) => [...current.filter((item) => item.id !== project.id), projectRow(project)]); selectProject(project.id); }
      }, (error: unknown) => setProjectError(commandErrorMessage(error)));
    }} onScan={() => {
      void invoke<string>("projects_scan").then(setScanJob, (error: unknown) => setProjectError(commandErrorMessage(error)));
    }} onCancelScan={() => {
      if (scanJob) void invoke("jobs_cancel", { jobId: scanJob }).finally(() => setScanJob(null));
    }} />
    {overwriteRefusal && <OverwriteRefusalDialog refusal={overwriteRefusal} onClose={() => setOverwriteRefusal(null)} onMoveAside={() => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return moveRefusedItemAside(path).then((result) => {
        const operationId = result.operationId;
        if (operationId) {
          state.toasts.show({
            kind: "success",
            title: "Moved aside",
            message: "The unmanaged item was preserved outside the agent folder.",
            action: { label: "Undo", run: () => { void undoMoveAside(operationId).then(
              () => state.toasts.show({ kind: "success", title: "Move undone", message: "The original item is back at its path." }),
              (error: unknown) => state.toasts.show({ kind: "error", title: "Undo failed", message: commandErrorMessage(error) }),
            ); } },
          });
        }
        return result;
      });
    }} onAdoptPreview={(handling) => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return previewAdoption(path, overwriteRefusal.existingHash, handling);
    }} onAdoptApply={(handling) => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return applyAdoption(path, overwriteRefusal.existingHash, handling).then((applied) => {
        const operationId = applied.operation_id;
        state.toasts.show({
          kind: "success",
          title: "Item adopted",
          message: applied.summary,
          action: { label: "Undo", run: () => { void invoke("operation_undo", { operationId }).then(
            () => state.toasts.show({ kind: "success", title: "Adoption undone", message: "The original tree was restored from its backup." }),
            (error: unknown) => state.toasts.show({ kind: "error", title: "Undo failed", message: commandErrorMessage(error) }),
          ); } },
        });
        return applied;
      });
    }} onUndo={async (operationId) => {
      await invoke("operation_undo", { operationId });
      state.toasts.show({ kind: "success", title: "Adoption undone", message: "The original tree was restored from its backup." });
    }} onUndoMove={async (operationId) => {
      await undoMoveAside(operationId);
      state.toasts.show({ kind: "success", title: "Move undone", message: "The original item is back at its path." });
    }} />}
    <Palette open={paletteOpen} index={paletteIndex} recentIds={state.getSnapshot().recentItemIds} loading={paletteLoading} error={paletteError} onRetry={() => setPaletteRetry((value) => value + 1)} onClose={() => setPaletteOpen(false)} onRun={(entry) => entry.run?.()} />
    {whyPalette !== null && (
      <WhyTraceDialog
        open={true}
        itemId={whyPalette.itemId}
        scope={whyPalette.scope}
        agent={whyPalette.agent}
        onClose={() => setWhyPalette(null)}
        onOpenEffective={() => state.navigate("effective")}
      />
    )}
    <RecoveryGate view={recovery} onContinue={() => setRecovery(null)} onDiscard={(operationId) => {
      void invoke("recovery_discard", { operationId }).then(() => setRecovery((current) => current ? { ...current, operations: current.operations.filter((operation) => operation.id !== operationId), files: current.files.filter((file) => file.operation_id !== operationId) } : null), (error: unknown) => state.toasts.show({ kind: "error", title: "Could not discard pending changes", message: commandErrorMessage(error) }));
    }} onRestore={(file) => {
      void invoke("recovery_restore", { operationId: file.operation_id, path: file.path }).then(
        () => invoke<RecoverySheetView | null>("recovery_sheet").then(setRecovery),
        (error: unknown) => state.toasts.show({ kind: "error", title: "Could not restore file", message: commandErrorMessage(error) }),
      );
    }} />
    <Modal open={state.getSnapshot().overlay === "keyboard-shortcuts"} onOpenChange={(open) => { if (!open) state.setOverlay(null); }} title="Keyboard shortcuts" description="Shortcuts are generated from the same registry used by the command dispatcher." size="md">
      <div className="flex flex-col gap-8">{SHORTCUTS.map((item) => (
        <div key={item.action} className="flex items-center justify-between gap-16 text-13">
          <span>{item.label}</span><kbd className="rounded-4 border border-border bg-raised px-6 py-2 font-mono text-11">{shortcutHint(item.action) ?? item.key}</kbd>
        </div>
      ))}</div>
    </Modal>
    <Modal open={accountDialog !== null} onOpenChange={(open) => { if (!open) setAccountDialog(null); }} title={accountDialog === "about" ? "About AgenTuna" : accountDialog === "documentation" ? "Documentation" : "Backups & restore"} description={accountDialog === "about" ? (signingStatus === null || signingStatus === "developer_id" ? `AgenTuna ${appVersion} · No account, no telemetry.` : `AgenTuna ${appVersion} · Local build (unsigned) · No account, no telemetry.`) : "AgenTuna is local-first. No account or telemetry is used."} size="sm">
      <div className="text-13 text-text-secondary">
        {accountDialog === "about" ? "AgenTuna manages agent configuration on this device." : accountDialog === "documentation" ? "Help and keyboard shortcuts are available from the Help menu." : "Backups created by safe file operations are available from the operation history and recovery flow."}
      </div>
    </Modal>
    <Modal open={doctorReport !== null} onOpenChange={(open) => { if (!open) setDoctorReport(null); }} title="Doctor report" description="Copy this redacted local diagnostics report for support." size="lg">
      <pre className="max-h-[50vh] overflow-auto whitespace-pre-wrap rounded-6 bg-surface p-12 font-mono text-11 text-text">{doctorReport}</pre>
      <div className="mt-12 flex justify-end"><button type="button" className="rounded-6 border border-border bg-raised px-12 py-8 text-13" onClick={() => { if (doctorReport) void navigator.clipboard.writeText(doctorReport); }}>Copy report</button></div>
    </Modal>
    <Modal
      open={cliStatus !== null || cliError !== null || loadingCliStatus || cliInstallReport !== null || cliUninstallReport !== null}
      onOpenChange={(open) => {
        if (!open && !installingCli) {
          setCliStatus(null);
          setCliError(null);
          setCliInstallConfirm(null);
          setCliUninstallConfirm(null);
          setCliInstallReport(null);
          setCliUninstallReport(null);
        }
      }}
      title={cliInstallTarget !== null ? "Confirm CLI installation" : cliUninstallTarget !== null ? "Confirm CLI uninstall" : "Install command-line tool"}
      description={cliInstallTarget !== null ? "Review the exact symlink before installing." : cliUninstallTarget !== null ? "Review the exact symlink before removing it." : "Manage the local agentuna command-line tool."}
      size="md"
      dismissible={!installingCli}
    >
      {loadingCliStatus && <p role="status" className="text-13 text-text-secondary">Checking CLI install locations…</p>}
      {cliError && <p role="alert" className="mb-12 text-13 text-danger">{cliError}</p>}
      {cliInstallTarget !== null && cliInstallConfirm !== null && cliStatus !== null && (
        <div className="flex flex-col gap-12">
          <p className="text-13">Install AgenTuna {cliStatus.bundled_version} at <code className="break-all">{cliInstallTarget.link_path}</code>, pointing to <code className="break-all">{cliStatus.bundled_binary}</code>?</p>
          {!cliInstallTarget.on_path && cliInstallTarget.path_export && <p className="text-12 text-text-secondary">This directory is not on PATH. Add this exact line to your shell profile: <code className="break-all">{cliInstallTarget.path_export}</code></p>}
          {!cliInstallTarget.writable && <p role="alert" className="text-12 text-danger">This location is not writable. Try the fallback location or grant directory write permission.</p>}
          <div className="flex justify-end gap-8">
            <button type="button" className="rounded-6 border border-border px-10 py-6 text-12" onClick={() => setCliInstallConfirm(null)}>Cancel</button>
            <button type="button" disabled={!cliInstallTarget.writable || installingCli} className="rounded-6 border border-border bg-raised px-10 py-6 text-12 disabled:opacity-50" onClick={() => installCli(cliInstallConfirm)}>{installingCli ? "Installing…" : "Install"}</button>
          </div>
        </div>
      )}
      {cliUninstallTarget !== null && cliUninstallConfirm !== null && (
        <div className="flex flex-col gap-12">
          <p className="text-13">Remove AgenTuna’s CLI link at <code className="break-all">{cliUninstallTarget.link_path}</code>? The bundled executable will not be removed.</p>
          <p className="text-12 text-text-secondary">Only a symlink owned by AgenTuna is removed. A foreign file or symlink is left untouched and reported.</p>
          <div className="flex justify-end gap-8">
            <button type="button" className="rounded-6 border border-border px-10 py-6 text-12" onClick={() => setCliUninstallConfirm(null)}>Cancel</button>
            <button type="button" disabled={installingCli} className="rounded-6 border border-border bg-raised px-10 py-6 text-12 disabled:opacity-50" onClick={() => uninstallCli(cliUninstallConfirm)}>{installingCli ? "Removing…" : "Uninstall"}</button>
          </div>
        </div>
      )}
      {cliInstallTarget === null && cliUninstallTarget === null && (
        <div className="flex flex-col gap-12">
          {cliStatus?.installed && <div className="rounded-6 border border-border bg-raised p-10 text-12"><div className="font-medium">Installed at {cliStatus.installed.link_path}</div><code className="break-all text-muted">{cliStatus.installed.target}</code></div>}
          {cliInstallReport && <div role="status" className="rounded-6 border border-border bg-raised p-10 text-12"><div className="font-medium">{cliInstallReport.created ? "CLI installed" : "CLI already installed"}</div><div>Link: <code className="break-all">{cliInstallReport.link_path}</code></div><div>Target: <code className="break-all">{cliInstallReport.target}</code></div><div>Version: {cliInstallReport.reported_version ?? "Unavailable"}{cliInstallReport.matches_app_version ? " (matches this app)" : " (does not match this app)"}</div></div>}
          {cliUninstallReport && <div role="status" className="rounded-6 border border-border bg-raised p-10 text-12"><div className="font-medium">{cliUninstallReport.removed ? "CLI link removed" : "CLI link not removed"}</div><div>Path: <code className="break-all">{cliUninstallReport.link_path}</code></div>{cliUninstallReport.warning && <p role="alert" className="mt-6 text-danger">{cliUninstallReport.warning}</p>}</div>}
          {cliStatus && <div className="flex flex-col gap-12">
            {(["primary", "fallback"] as const).map((location) => {
              const target = cliStatus[location];
              return <div key={location} className="flex items-center justify-between gap-12 rounded-6 border border-border p-10">
                <div className="min-w-0"><div className="text-12 font-medium">{location === "primary" ? "Preferred location" : "Fallback location"} · {target.directory}</div><code className="break-all text-11 text-muted">{target.link_path}</code><div className="text-11 text-muted">{target.on_path ? "Already on PATH" : target.path_export ?? "Not on PATH"}</div>{!target.writable && <div className="text-11 text-danger">{location === "primary" ? "Not writable; use the fallback location." : "Not writable; grant directory write permission."}</div>}</div>
                <div className="flex shrink-0 gap-6">
                  <button type="button" disabled={!target.writable || installingCli} className="rounded-6 border border-border bg-raised px-10 py-6 text-12 disabled:opacity-50" onClick={() => setCliInstallConfirm(location)}>Install</button>
                  <button type="button" disabled={installingCli} className="rounded-6 border border-border px-10 py-6 text-12 disabled:opacity-50" onClick={() => setCliUninstallConfirm(location)}>Uninstall</button>
                </div>
              </div>;
            })}
          </div>}
          {cliError && <button type="button" className="self-start rounded-6 border border-border px-10 py-6 text-12" onClick={loadCliStatus}>Retry status</button>}
        </div>
      )}
    </Modal>
  </>;
  const projectScopeReason = (scope === "project" || scope === "local") && !hasProject ? "Choose a project first" : undefined;
  const globalScopeReason = scope !== "global" ? undefined
    : globalAddAccess.state === "readonly" ? globalAddAccess.reason ?? "Global scope is read-only"
      : globalAddAccess.state === "checking" ? "Checking Global write access…" : undefined;
  const scopeReason = projectScopeReason ?? globalScopeReason;
  const agentReason = addAgentState === "missing" ? "Install or connect an agent first"
    : addAgentState === "checking" ? "Checking for installed agents…" : undefined;
  const addControl = <AddMenu scope={scope} routeKey={route} disabledReasons={{
    "add.skill": scopeReason ?? agentReason,
    "add.server": scopeReason ?? agentReason,
    "add.rule": scopeReason,
  }} open={addOpen} onOpenChange={setAddOpen} onAction={runAdd} />;
  return children({
    addControl,
    onProjectPicker: openProjects,
    onPalette: () => setPaletteOpen(true),
    onAccountAction,
    accountDisabledReasons: effectiveAccountDisabledReasons,
    overlays,
  });
}
