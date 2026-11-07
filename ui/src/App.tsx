import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import type { AppInfo } from "./generated/AppInfo";
import type { AgentCardView } from "./generated/AgentCardView";
import type { InstructionCreateOptions } from "./generated/InstructionCreateOptions";
import type { ProjectView } from "./generated/ProjectView";
import type { SourceKind } from "./generated/SourceKind";
import { ScopeSwitcher } from "./components/scope_switcher/ScopeSwitcher";
import type { StateRouteId } from "./components/states";
import { Modal, ModalHost, OverlayStackProvider } from "./overlays";
import { ToastProvider } from "./overlays/toasts";
import { createAppState } from "./state/AppState";
import { AppStateProvider, useAppSnapshot } from "./state/context";
import { Shell } from "./shell";
import { shellAgentStatus } from "./shell/agents";
import { parseItemDeepLink, SHELL_SECTION_LABEL, sectionForRoute } from "./shell/routes";

import { EffectiveScreen } from "./screens/effective/EffectiveScreen";
import { MigrateScanScreen } from "./screens/migrate/MigrateScanScreen";
import { AppOverlays } from "./overlays/AppOverlays";
import { OverviewScreen } from "./screens/overview/OverviewScreen";
import { AddSkillWizard } from "./screens/add_skill/AddSkillWizard";
import { SecretInventoryScreen } from "./screens/secrets/SecretInventoryScreen";
import { SkillDetailScreen } from "./screens/skill_detail/SkillDetailScreen";
import { PreferencesScreen } from "./screens/preferences/PreferencesScreen";
import { InstructionsScreen } from "./screens/instructions/InstructionsScreen";
import { AgentsScreen } from "./screens/agents/AgentsScreen";
import { PresetsScreen } from "./screens/presets/PresetsScreen";
import { AddMcpServerForm } from "./screens/mcp_form";
import { McpListScreen } from "./screens/mcp_list/McpListScreen";
import { SkillsScreen } from "./screens/skills/SkillsScreen";
import { McpDetailScreen } from "./screens/mcp_detail/McpDetailScreen";
import { DriftScreen } from "./screens/drift/DriftScreen";
import { DriftBanner } from "./screens/drift/DriftBanner";
import { ToastEventBridge } from "./overlays/toasts/ToastEventBridge";
import type { AddMcpServerDraft, McpServerFormInitialState } from "./screens/mcp_form";
import type { McpServerDetailView } from "./generated/McpServerDetailView";
import type { McpChangeSetPreviewView } from "./generated/McpChangeSetPreviewView";
import type { McpLaunchApproval } from "./generated/McpLaunchApproval";
import type { McpLaunchApprovalView } from "./generated/McpLaunchApprovalView";
import type { McpPlanPreviewRequest } from "./generated/McpPlanPreviewRequest";
import type { McpSaveRequest } from "./generated/McpSaveRequest";
import type { McpSaveResultView } from "./generated/McpSaveResultView";
import type { McpTestResultView } from "./generated/McpTestResultView";
import type { ScopeToken } from "./theme";

/**
 * Subscribes to a Tauri event and returns the cleanup for `useEffect`.
 *
 * Safe under React StrictMode (a listener registered after cleanup is disposed at once) and
 * outside the Tauri runtime (browser/test), where `listen` rejects and the app simply gets
 * no native events.
 */
function subscribe<T>(event: string, handler: (payload: T) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  listen<T>(event, (message) => handler(message.payload)).then(
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
}
/** Maps the form draft onto the typed planner request; preview and draft test share this shape. */
function draftRequest(draft: AddMcpServerDraft, projectId: string | null, itemId: string | null): McpPlanPreviewRequest {
  return {
    name: draft.name,
    transport: draft.transport,
    command: draft.command,
    args: [...draft.args],
    url: draft.url,
    headers: draft.headers.map(({ name, value }) => ({ name, value })),
    env: draft.env.map(({ name, value }) => ({ name, value })),
    scope: draft.scope,
    agents: [...draft.agents],
    project_id: projectId,
    item_id: itemId,
  };
}

/** Add/edit MCP form route; a selected item id loads the canonical server for editing. */
function McpFormRoute({ onClose, projectId }: { readonly onClose: () => void; readonly projectId: string | null }) {
  const snapshot = useAppSnapshot();
  const selectedId = snapshot.selections["mcp-form"]?.[0] ?? null;
  const [initialState, setInitialState] = useState<McpServerFormInitialState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(selectedId === null);
  const [globalAllowed, setGlobalAllowed] = useState(false);
  useEffect(() => {
    let live = true;
    void invoke<{ readonly global_store?: { readonly status?: string } }>("overview_load", { activeScope: "global" }).then((overview) => {
      const status = overview?.global_store?.status;
      if (live && typeof status === "string") setGlobalAllowed(status === "available");
    }, () => {
      if (live) setGlobalAllowed(false);
    });
    return () => { live = false; };
  }, []);

  useEffect(() => {
    if (selectedId === null) { setLoaded(true); return; }
    let live = true;
    invoke<McpServerDetailView>("mcp_server_detail", {
      request: { item_id: selectedId, scope: snapshot.scope, project_id: snapshot.activeProject },
    }).then((detail) => {
      if (!live) return;
      const transport: "stdio" | "http" = detail.transport === "http" ? "http" : "stdio";
      const rows = detail.env.map((row) => ({ name: row.key, value: row.is_secret ? (row.keychain_account ?? "") : row.value }));
      setInitialState({
        id: detail.item_id,
        name: detail.name,
        transport,
        command: detail.command ?? "",
        args: [...detail.args],
        url: detail.url ?? "",
        headers: transport === "http" ? rows : [],
        env: transport === "stdio" ? rows.map((row) => ({ ...row, kind: detail.env.find((entry) => entry.key === row.name)?.is_secret === true ? "secret" as const : "literal" as const })) : [],
        scope: detail.scope as AddMcpServerDraft["scope"],
        agents: detail.agents.filter((agent) => agent.enabled).map((agent) => agent.id as AddMcpServerDraft["agents"][number]),
      });
    }).catch((cause: unknown) => {
      if (!live) return;
      const message = typeof cause === "object" && cause !== null && "message" in cause && typeof cause.message === "string"
        ? cause.message
        : "The MCP server could not be loaded.";
      setLoadError(message);
    }).finally(() => { if (live) setLoaded(true); });
    return () => { live = false; };
  }, [selectedId, snapshot.scope, snapshot.activeProject]);

  if (loadError !== null) return <p role="alert" className="mx-auto max-w-[960px] p-24 text-13 text-danger">{loadError}</p>;
  if (!loaded) return <p role="status" className="mx-auto max-w-[960px] p-24 text-13 text-muted">Loading MCP server…</p>;
  return <AddMcpServerForm
    mode={initialState === null ? "add" : "edit"}
    initialState={initialState ?? undefined}
    initialScope={snapshot.scope}
    globalAllowed={globalAllowed}
    onCancel={onClose}
    onPlan={(draft: AddMcpServerDraft, itemId?: string) => invoke<McpChangeSetPreviewView>("mcp_plan_preview", { request: draftRequest(draft, projectId, itemId ?? null) })}
    onSave={async (previewId, selectedAgents, acknowledgeUnsupported) => {
      const request: McpSaveRequest = { preview_id: previewId, selected_agents: [...selectedAgents], acknowledge_unsupported: acknowledgeUnsupported };
      await invoke<McpSaveResultView>("mcp_save", { request });
    }}
    onPrepareLaunch={(draft: AddMcpServerDraft) => invoke<McpLaunchApprovalView>("mcp_launch_prepare", { request: draftRequest(draft, projectId, null) })}
    onTest={(draft: AddMcpServerDraft, approval: McpLaunchApproval) => invoke<McpTestResultView>("mcp_test_draft", { request: { draft: draftRequest(draft, projectId, null), approval } })}
  />;
}

/** Placeholder routed body; real screen contents are supplied by their stories. */
function RouteBody({
  route,
  onClose,
  projectId,
  projectName,
  scope,
  localEnabled,
  globalLocked,
  initialSkillSource,
}: {
  readonly route: StateRouteId;
  readonly onClose: () => void;
  readonly projectId: string | null;
  readonly projectName: string | null;
  readonly scope: ScopeToken;
  readonly localEnabled: boolean;
  readonly globalLocked: boolean;
  /** Source selected by the button that opened the add-skill wizard. */
  readonly initialSkillSource: SourceKind | null;
}) {
  if (route === "overview") return <OverviewScreen projectName={projectName} />;
  if (route === "skill-detail") return <SkillDetailScreen />;
  if (route === "add-skill") return <AddSkillWizard onClose={onClose} projectId={projectId} initialScope={scope} initialSource={initialSkillSource} />;
  if (route === "secrets") return <SecretInventoryScreen />;
  if (route === "instructions") return <InstructionsScreen />;
  if (route === "agents") return <AgentsScreen />;
  if (route === "skills") return <><DriftBanner /><SkillsScreen localEnabled={localEnabled} globalLocked={globalLocked} /></>;
  if (route === "drift") return <DriftScreen />;
  if ((route as string) === "presets") return <PresetsScreen />;
  if (route === "effective") return <EffectiveScreen />;
  if (route === "migrate") return <MigrateScanScreen />;
  if (route === "settings") return <PreferencesScreen />;
  if (route === "mcp-list") return <><DriftBanner /><McpListScreen localEnabled={localEnabled} globalLocked={globalLocked} /></>;
  if (route === "mcp-detail") return <McpDetailScreen />;
  if (route === "mcp-form") return <McpFormRoute onClose={onClose} projectId={projectId} />;
  return null;
}

/** Root Tauri webview application. */
export function App() {
  const [pendingNavigation, setPendingNavigation] = useState<StateRouteId | null>(null);
  const [pendingScope, setPendingScope] = useState<{ readonly projectId: string; readonly scope: "global" | "user" | "project" | "local" } | null>(null);
  const [appState] = useState(() => createAppState({
    effects: (effect) => {
      if (effect.type === "persist-last-project") {
        void invoke("settings_set", { key: "last_project_id", value: effect.projectId });
      }
      if (effect.type === "persist-last-scope") {
        void invoke("settings_set", { key: `last_scope.${effect.projectId}`, value: effect.scope });
      }
      if (effect.type === "restore-project-scope") {
        void invoke<{ readonly values?: Record<string, unknown> }>("settings_load").then((document) => {
          const value = document.values?.[`last_scope.${effect.projectId}`];
          const scope = value === "global" || value === "user" || value === "project" || value === "local" ? value : "user";
          setPendingScope({ projectId: effect.projectId, scope });
        }, () => setPendingScope({ projectId: effect.projectId, scope: "user" }));
      }
      if (effect.type === "dirty-navigation") setPendingNavigation(effect.to);
      if (effect.type === "project-picker-required") {
        window.dispatchEvent(new CustomEvent("agentuna:project-picker-required", { detail: { route: effect.route, itemId: effect.itemId ?? null } }));
      }
    },
  }));
  const appSnapshot = useSyncExternalStore(appState.subscribe, appState.getSnapshot, appState.getSnapshot);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [projects, setProjects] = useState<readonly ProjectView[]>([]);
  const [scopeAvailability, setScopeAvailability] = useState<{
    readonly projectId: string;
    readonly options: InstructionCreateOptions;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [bootstrapped, setBootstrapped] = useState(false);
  const route = appSnapshot.route;
  const currentScopeAvailability = scopeAvailability?.projectId === appSnapshot.activeProject
    ? scopeAvailability.options
    : null;

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(String(e)));
  }, []);
  useEffect(() => {
    let active = true;
    void invoke<AgentCardView[]>("agents_list").then(
      (cards) => {
        if (active && Array.isArray(cards)) appState.receiveAgents(shellAgentStatus(cards));
      },
      () => undefined,
    );
    return () => { active = false; };
  }, [appState]);
  useEffect(() => {
    let active = true;
    void Promise.all([
      invoke<{ readonly values?: Record<string, unknown> }>("settings_load"),
      invoke<ProjectView[]>("projects_list"),
    ]).then(([document, projects]) => {
      if (!active) return;
      setProjects(projects);
      const values = document.values ?? {};
      const savedProject = typeof values.last_project_id === "string" ? values.last_project_id : null;
      const projectId = savedProject !== null && projects.some((project) => project.id === savedProject) ? savedProject : null;
      const storedScope = projectId === null ? undefined : values[`last_scope.${projectId}`];
      const scope = storedScope === "global" || storedScope === "project" || storedScope === "local" || storedScope === "user"
        ? storedScope : "user";
      appState.restore(projectId, scope);
      setBootstrapped(true);
    }, (cause: unknown) => {
      if (!active) return;
      setError(String(cause));
      appState.restore(null, "user");
      setBootstrapped(true);
    });
    return () => { active = false; };
  }, [appState]);
  useEffect(() => {
    const projectId = appSnapshot.activeProject;
    if (projectId === null) {
      setScopeAvailability(null);
      return;
    }
    let active = true;
    void invoke<InstructionCreateOptions>("instruction_create_options", { projectId }).then(
      (options) => { if (active) setScopeAvailability({ projectId, options }); },
      () => { if (active) setScopeAvailability(null); },
    );
    return () => { active = false; };
  }, [appSnapshot.activeProject]);
  useEffect(() => {
    if (currentScopeAvailability === null) return;
    if (appSnapshot.scope === "local" && !currentScopeAvailability.local_available) {
      appState.receiveScope("project");
    } else if (appSnapshot.scope === "global" && !currentScopeAvailability.global_writable) {
      appState.receiveScope("user");
    }
  }, [appSnapshot.scope, appState, currentScopeAvailability]);
  useEffect(() => {
    if (pendingScope === null) return;
    if (appSnapshot.activeProject === pendingScope.projectId) appState.receiveScope(pendingScope.scope);
    setPendingScope(null);
  }, [appSnapshot.activeProject, appState, pendingScope]);
  const openDeepLink = useCallback((payload: string) => {
    const itemId = parseItemDeepLink(payload);
    if (itemId === null) return;
    void invoke<{ readonly kind: string; readonly scope: string; readonly project_id: string | null } | null>("item_lookup", { itemId }).then(
      (item) => appState.openItem(itemId, item),
      () => appState.openItem(itemId, null),
    );
  }, [appState]);
  useEffect(() => subscribe<string>("deep-link", openDeepLink), [openDeepLink]);
  useEffect(() => {
    // Cold start: the backend buffers OS links until this first take marks the webview listening.
    void invoke<string[]>("deep_link_take").then(
      (payloads) => { if (Array.isArray(payloads)) for (const payload of payloads) openDeepLink(payload); },
      () => undefined,
    );
  }, [openDeepLink]);
  useEffect(
    () =>
      subscribe<string>("menu-action", (payload) => {
        window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: payload }));
      }),
    [],
  );
  useEffect(() => {
    // Scope and screen shortcuts live in the one ShortcutMap dispatcher in AppOverlays. This
    // effect only keeps the back shortcut, which is not part of the documented shortcut map.
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === "[") {
        event.preventDefault();
        appState.back();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [appState]);

  const versionLabel = info === null ? "v… · local" : `v${info.version} · local`;
  const projectLabel = projects.find((project) => project.id === appSnapshot.activeProject)?.name ?? appSnapshot.activeProject;
  const section = sectionForRoute(route);
  const title = section === null ? "Settings" : SHELL_SECTION_LABEL[section];

  if (!bootstrapped) return <main role="status" aria-live="polite" className="grid h-full place-items-center text-secondary">Loading AgenTuna…</main>;
  return (
    <AppStateProvider state={appState}>
      <ToastProvider store={appState.toasts}>
        <ToastEventBridge />
        <OverlayStackProvider>
          <ModalHost>
            <AppOverlays state={appState} scope={appSnapshot.scope} route={route} hasProject={appSnapshot.activeProject !== null} projectId={appSnapshot.activeProject} appVersion={info?.version ?? "unknown"} scopeAvailability={currentScopeAvailability === null ? undefined : { localAvailable: currentScopeAvailability.local_available, globalAvailable: currentScopeAvailability.global_writable }}>
              {(overlaySlots) => (
                <>
            <Shell
              route={route}
              title={title}
              projectLabel={projectLabel}
              scope={appSnapshot.scope}
              scopeSwitcher={
                <ScopeSwitcher
                  scope={appSnapshot.scope}
                  localEnabled={currentScopeAvailability?.local_available ?? false}
                  hasProject={appSnapshot.activeProject !== null}
                  globalLocked={currentScopeAvailability !== null && !currentScopeAvailability.global_writable}
                  onScopeSelected={appState.receiveScope}
                />
              }
              versionLabel={versionLabel}
              agents={appSnapshot.agents}
              canGoBack={appSnapshot.routeStack.length > 1}
              onBack={() => { appState.back(); }}
              onNavigate={(next) => appState.navigate(next)}
              onProjectPicker={overlaySlots.onProjectPicker}
              onPalette={overlaySlots.onPalette}
              addControl={overlaySlots.addControl}
              onAccountAction={overlaySlots.onAccountAction}
              accountDisabledReasons={overlaySlots.accountDisabledReasons}
              overlays={overlaySlots.overlays}
            >
              {error === null ? (
                <RouteBody
                  route={route}
                  onClose={() => {
                    if (appSnapshot.routeStack.length > 1) {
                      appState.back("discard");
                    } else {
                      appState.navigate("mcp-list", "discard");
                    }
                  }}
                  projectId={appSnapshot.activeProject}
                  projectName={projects.find((project) => project.id === appSnapshot.activeProject)?.name ?? null}
                  scope={appSnapshot.scope}
                  localEnabled={currentScopeAvailability?.local_available ?? false}
                  globalLocked={currentScopeAvailability !== null && !currentScopeAvailability.global_writable}
                  initialSkillSource={
                    appSnapshot.screenState["add-skill:source"] === "git" ||
                    appSnapshot.screenState["add-skill:source"] === "new" ||
                    appSnapshot.screenState["add-skill:source"] === "library" ||
                    appSnapshot.screenState["add-skill:source"] === "marketplace" ||
                    appSnapshot.screenState["add-skill:source"] === "folder" ||
                    appSnapshot.screenState["add-skill:source"] === "zip"
                      ? (appSnapshot.screenState["add-skill:source"] as SourceKind)
                      : null
                  }
                />
              ) : (
                <div className="p-24 text-danger">Backend error: {error}</div>
              )}
            </Shell>
                </>
              )}
            </AppOverlays>
            <Modal
              open={pendingNavigation !== null}
              title="Unsaved changes"
              description="Save your changes before leaving this screen?"
              onOpenChange={(open) => { if (!open) setPendingNavigation(null); }}
              footer={
                <div className="flex justify-end gap-8">
                  <button type="button" onClick={() => setPendingNavigation(null)}>Cancel</button>
                  <button type="button" onClick={() => {
                    if (pendingNavigation !== null) appState.navigate(pendingNavigation, "discard");
                    setPendingNavigation(null);
                  }}>Discard</button>
                  <button type="button" onClick={() => {
                    if (pendingNavigation !== null) appState.navigate(pendingNavigation, "save");
                    setPendingNavigation(null);
                  }}>Save</button>
                </div>
              }
            />
          </ModalHost>
        </OverlayStackProvider>
      </ToastProvider>
    </AppStateProvider>
  );
}
