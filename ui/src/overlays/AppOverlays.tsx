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
import type { ScopeToken } from "../theme";

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

  const reloadProjects = async () => {
    try {
      const rows = await invoke<ProjectView[]>("projects_list");
      setProjects(rows.map(projectRow));
      setProjectError(null);
    } catch (error) {
      setProjectError(String(error));
    }
  };
  const openProjects = () => { setProjectOpen(true); void reloadProjects(); };
  const runAdd = (action: AddMenuAction) => {
    const routeByAction: Readonly<Record<AddMenuAction["id"], StateRouteId>> = {
      "add.skill": "add-skill",
      "add.server": "mcp-form",
      "add.rule": "instructions",
    };
    state.receiveScope(action.scope);
    state.navigate(routeByAction[action.id]);
  };
  const paletteIndex = useMemo(() => buildPaletteIndex({
    skills: [], servers: [], rules: [], agents: [],
    actions: [
      { id: "action:skills", title: "Skills", subtitle: "Open skills", keywords: ["browse", "manage"], run: () => state.navigate("skills") },
      { id: "action:servers", title: "MCP Servers", subtitle: "Open MCP servers", keywords: ["mcp", "server", "manage"], run: () => state.navigate("mcp-list") },
      { id: "action:instructions", title: "Rule files", subtitle: "Open instructions", keywords: ["rules", "files"], run: () => state.navigate("instructions") },
      { id: "action:add-skill", title: "New skill", subtitle: `Add to ${scope} scope`, keywords: ["create", "add"], run: () => runAdd({ id: "add.skill", scope }) },
      { id: "action:add-server", title: "Add MCP server", subtitle: `Add to ${scope} scope`, keywords: ["mcp", "server", "create"], run: () => runAdd({ id: "add.server", scope }) },
      { id: "action:add-rule", title: "New rule file", subtitle: `Add to ${scope} scope`, keywords: ["rule", "file", "create"], run: () => runAdd({ id: "add.rule", scope }) },
    ],
  }), [scope, state]);

  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() === "n") { event.preventDefault(); setAddOpen(true); }
      if (event.key.toLowerCase() === "p") { event.preventDefault(); openProjects(); }
      if (event.key.toLowerCase() === "k") { event.preventDefault(); setPaletteOpen(true); }
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, []);

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

  const overlays = <>
    <ProjectPicker open={projectOpen} projects={projects} error={projectError} scanning={scanJob !== null} onClose={() => setProjectOpen(false)} onSelect={(project) => { void state.selectProject(project.id); }} onAddFolder={() => {
      void invoke<ProjectView | null>("projects_choose_folder").then((project) => {
        if (project) { setProjects((current) => [...current.filter((item) => item.id !== project.id), projectRow(project)]); void state.selectProject(project.id); }
      }, (error: unknown) => setProjectError(String(error)));
    }} onScan={() => {
      void invoke<string>("projects_scan").then(setScanJob, (error: unknown) => setProjectError(String(error)));
    }} onCancelScan={() => {
      if (scanJob) void invoke("jobs_cancel", { jobId: scanJob }).finally(() => setScanJob(null));
    }} />
    <Palette open={paletteOpen} index={paletteIndex} onClose={() => setPaletteOpen(false)} onRun={(entry) => entry.run?.()} />
  </>;
  const disabledReason = (scope === "project" || scope === "local") && !hasProject ? "Choose a project first" : undefined;
  const addControl = <AddMenu scope={scope} routeKey={route} disabledReason={disabledReason} open={addOpen} onOpenChange={setAddOpen} onAction={runAdd} />;
  return children({ addControl, onProjectPicker: openProjects, onPalette: () => setPaletteOpen(true), overlays });
}
