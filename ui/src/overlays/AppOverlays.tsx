import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useState } from "react";
import type { JSX, ReactNode } from "react";
import type { ProjectView } from "../generated/ProjectView";
import type { JobFinishedEvent } from "../generated/JobFinishedEvent";
import { AddMenu } from "./menus/AddMenu";
import type { AddMenuAction } from "./menus/AddMenu";
import { Palette } from "./palette/Palette";
import { buildPaletteIndex } from "./palette";
import { ProjectPicker, relativeLastUsed } from "./project_picker";
import type { ProjectPickerProject } from "./project_picker";
import type { AppState } from "../state/AppState";
import type { StateRouteId } from "../components/states";
import type { CliInstallStatus } from "../generated/CliInstallStatus";
import type { CliInstallReport } from "../generated/CliInstallReport";
import type { CliInstallLocation } from "../generated/CliInstallLocation";
import type { RecoverySheetView } from "../generated/RecoverySheetView";
import { RecoverySheet } from "./recovery/RecoverySheet";
import { Modal } from "./dialogs/Modal";
import type { AccountMenuAction } from "./menus/AccountMenu";
import type { ScopeToken } from "../theme";
import { ACTION_IDS, SHORTCUTS, routeForAction, shortcutHint } from "../shell/actions";
import { OverwriteRefusalDialog, refusedOverwriteDetails } from "./dialogs/OverwriteRefusalDialog";
import type { RefusedOverwriteDetails } from "./dialogs/OverwriteRefusalDialog";
import { applyAdoption, moveRefusedItemAside, previewAdoption } from "./dialogs/overwrite-commands";
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
  readonly overlays: ReactNode;
}

/** App-shell composition point for shared, state-backed overlays. */
export function AppOverlays({ state, scope, route, hasProject, children }: {
  readonly state: AppState;
  readonly scope: ScopeToken;
  readonly route: StateRouteId;
  readonly hasProject: boolean;
  readonly children: (slots: AppOverlaySlots) => JSX.Element;
}): JSX.Element {
  const [projectOpen, setProjectOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [projects, setProjects] = useState<readonly ProjectPickerProject[]>([]);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [scanJob, setScanJob] = useState<string | null>(null);
  const [accountDialog, setAccountDialog] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<RecoverySheetView | null>(null);
  const [cliStatus, setCliStatus] = useState<CliInstallStatus | null>(null);
  const [cliError, setCliError] = useState<string | null>(null);
  const [installingCli, setInstallingCli] = useState(false);
  const [doctorReport, setDoctorReport] = useState<string | null>(null);
  const [overwriteRefusal, setOverwriteRefusal] = useState<RefusedOverwriteDetails | null>(null);

  const reloadProjects = async () => {
    try {
      const rows = await invoke<ProjectView[]>("projects_list");
      setProjects(rows.map(projectRow));
      setProjectError(null);
    } catch (error) {
      setProjectError(String(error));
    }
  };
  const openProjects = () => {
    setProjectOpen(true);
    void reloadProjects();
  };
  const dispatch = (id: string) => {
    if (id.startsWith("scope:")) {
      const nextScope = id.slice("scope:".length);
      if (nextScope === "global" || nextScope === "user" || nextScope === "project" || nextScope === "local") state.receiveScope(nextScope);
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
    if (id === ACTION_IDS.docs) { setAccountDialog("documentation"); return; }
    if (id === ACTION_IDS.rescan) {
      void invoke<string>("projects_scan").then(setScanJob, (error: unknown) => setProjectError(String(error)));
      return;
    }
    if (id === ACTION_IDS.installCli) {
      setCliError(null);
      void invoke<CliInstallStatus>("cli_install_status").then(setCliStatus, (error: unknown) => setCliError(String(error)));
      return;
    }
    if (id === ACTION_IDS.doctor) {
      setDoctorReport(null);
      void invoke<string>("diagnostics_report_copy").then(setDoctorReport, (error: unknown) => state.toasts.show({ kind: "error", title: "Diagnostics unavailable", message: String(error) }));
      return;
    }
    const target = routeForAction(id);
    if (target) {
      if (id === "add-skill" || id === "add-mcp-server" || id === "add-rule-file") state.receiveScope(scope);
      state.navigate(target);
      setAddOpen(false);
      setPaletteOpen(false);
      setProjectOpen(false);
    }
  };
  const onAccountAction = (action: AccountMenuAction) => {
    if (action === "preferences") { dispatch(ACTION_IDS.preferences); return; }
    if (action === "agents") { dispatch(ACTION_IDS.agents); return; }
    if (action === "secrets") { state.navigate("secrets"); return; }
    setAccountDialog(action);
  };
  const runAdd = (action: AddMenuAction) => {
    const actionId: Readonly<Record<AddMenuAction["id"], string>> = {
      "add.skill": "add-skill",
      "add.server": "add-mcp-server",
      "add.rule": "add-rule-file",
    };
    state.receiveScope(action.scope);
    dispatch(actionId[action.id]);
  };
  const paletteIndex = useMemo(() => buildPaletteIndex({
    skills: [], servers: [], rules: [], agents: [],
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
    ],
  }), [scope, state]);

  useEffect(() => {
    const dispatchMenu = (event: Event) => dispatch((event as CustomEvent<string>).detail);
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
      (error: unknown) => state.toasts.show({ kind: "error", title: "Recovery status unavailable", message: String(error) }),
    );
  }, [state]);

  const overlays = <>
    <ProjectPicker open={projectOpen} projects={projects} currentProjectId={state.getSnapshot().activeProject} error={projectError} scanning={scanJob !== null} onClose={() => setProjectOpen(false)} onSelect={(project) => { void state.selectProject(project.id); }} onAddFolder={() => {
      void invoke<ProjectView | null>("projects_choose_folder").then((project) => {
        if (project) { setProjects((current) => [...current.filter((item) => item.id !== project.id), projectRow(project)]); void state.selectProject(project.id); }
      }, (error: unknown) => setProjectError(String(error)));
    }} onScan={() => {
      void invoke<string>("projects_scan").then(setScanJob, (error: unknown) => setProjectError(String(error)));
    }} onCancelScan={() => {
      if (scanJob) void invoke("jobs_cancel", { jobId: scanJob }).finally(() => setScanJob(null));
    }} />
    {overwriteRefusal && <OverwriteRefusalDialog refusal={overwriteRefusal} onClose={() => setOverwriteRefusal(null)} onMoveAside={() => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return moveRefusedItemAside(path);
    }} onAdoptPreview={(handling) => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return previewAdoption(path, overwriteRefusal.existingHash, handling);
    }} onAdoptApply={(handling) => {
      const path = overwriteRefusal.report.path;
      if (!path) return Promise.reject(new Error("The refused path is unavailable."));
      return applyAdoption(path, overwriteRefusal.existingHash, handling);
    }} onUndo={async (operationId) => {
      await invoke("operations_undo", { operationId });
      state.toasts.show({ kind: "success", title: "Adoption undone", message: "The original tree was restored from its backup." });
    }} />}
    <Palette open={paletteOpen} index={paletteIndex} onClose={() => setPaletteOpen(false)} onRun={(entry) => entry.run?.()} />
    {recovery ? <RecoverySheet recovery={recovery} onContinue={() => setRecovery(null)} onDiscard={(operationId) => {
      void invoke("recovery_discard", { operationId }).then(() => setRecovery((current) => current ? { ...current, operations: current.operations.filter((operation) => operation.id !== operationId), files: current.files.filter((file) => file.operation_id !== operationId) } : null), (error: unknown) => state.toasts.show({ kind: "error", title: "Could not discard pending changes", message: String(error) }));
    }} onRestore={(file) => {
      void invoke("recovery_restore", { operationId: file.operation_id, path: file.path }).then(
        () => invoke<RecoverySheetView | null>("recovery_sheet").then(setRecovery),
        (error: unknown) => state.toasts.show({ kind: "error", title: "Could not restore file", message: String(error) }),
      );
    }} /> : null}
    <Modal open={state.getSnapshot().overlay === "keyboard-shortcuts"} onOpenChange={(open) => { if (!open) state.setOverlay(null); }} title="Keyboard shortcuts" description="Shortcuts are generated from the same registry used by the command dispatcher." size="md">
      <div className="flex flex-col gap-8">{SHORTCUTS.map((item) => (
        <div key={item.action} className="flex items-center justify-between gap-16 text-13">
          <span>{item.label}</span><kbd className="rounded-4 border border-border bg-raised px-6 py-2 font-mono text-11">{shortcutHint(item.action) ?? item.key}</kbd>
        </div>
      ))}</div>
    </Modal>
    <Modal open={accountDialog !== null} onOpenChange={(open) => { if (!open) setAccountDialog(null); }} title={accountDialog === "about" ? "About AgenTuna" : accountDialog === "documentation" ? "Documentation" : "Backups & restore"} description="AgenTuna is local-first. No account or telemetry is used." size="sm">
      <div className="text-13 text-text-secondary">
        {accountDialog === "about" ? "AgenTuna manages agent configuration on this device." : accountDialog === "documentation" ? "Help and keyboard shortcuts are available from the Help menu." : "Backups created by safe file operations are available from the operation history and recovery flow."}
      </div>
    </Modal>
    <Modal open={doctorReport !== null} onOpenChange={(open) => { if (!open) setDoctorReport(null); }} title="Doctor report" description="Copy this redacted local diagnostics report for support." size="lg">
      <pre className="max-h-[50vh] overflow-auto whitespace-pre-wrap rounded-6 bg-surface p-12 font-mono text-11 text-text">{doctorReport}</pre>
      <div className="mt-12 flex justify-end"><button type="button" className="rounded-6 border border-border bg-raised px-12 py-8 text-13" onClick={() => { if (doctorReport) void navigator.clipboard.writeText(doctorReport); }}>Copy report</button></div>
    </Modal>
    <Modal open={cliStatus !== null || cliError !== null} onOpenChange={(open) => { if (!open && !installingCli) { setCliStatus(null); setCliError(null); } }} title="Install command-line tool" description="Choose where to install the local agentuna command." size="md" dismissible={!installingCli}>
      {cliError && <p role="alert" className="mb-12 text-13 text-danger">{cliError}</p>}
      {cliStatus && <div className="flex flex-col gap-12">
        {[["primary", cliStatus.primary], ["fallback", cliStatus.fallback]].map(([key, location]) => {
          const installLocation = key as CliInstallLocation;
          const target = location as CliInstallStatus["primary"];
          return <div key={installLocation} className="flex items-center justify-between gap-12 rounded-6 border border-border p-10">
            <div className="min-w-0"><div className="text-12 font-medium">{target.directory}</div><code className="text-11 text-muted">{target.link_path}</code><div className="text-11 text-muted">{target.on_path ? "Already on PATH" : target.path_export ?? "Not on PATH"}</div></div>
            <button type="button" disabled={!target.writable || installingCli} className="rounded-6 border border-border bg-raised px-10 py-6 text-12 disabled:opacity-50" onClick={() => {
              setInstallingCli(true);
              void invoke<CliInstallReport>("cli_install", { location: installLocation }).then(
                (report) => { state.toasts.show({ kind: "success", title: "CLI installed", message: `Installed at ${report.link_path}` }); setCliStatus(null); },
                (error: unknown) => setCliError(String(error)),
              ).finally(() => setInstallingCli(false));
            }}>{installingCli ? "Installing…" : "Install"}</button>
          </div>;
        })}
      </div>}
    </Modal>
  </>;
  const disabledReason = (scope === "project" || scope === "local") && !hasProject ? "Choose a project first" : undefined;
  const addControl = <AddMenu scope={scope} routeKey={route} disabledReason={disabledReason} open={addOpen} onOpenChange={setAddOpen} onAction={runAdd} />;
  return children({ addControl, onProjectPicker: openProjects, onPalette: () => setPaletteOpen(true), onAccountAction, overlays });
}
