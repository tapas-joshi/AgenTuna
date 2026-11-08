import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { lazy, Suspense, useCallback, useEffect, useState, useSyncExternalStore } from "react";
import type { JSX } from "react";
import type { AppInfo } from "./generated/AppInfo";
import type { AgentCardView } from "./generated/AgentCardView";
import type { InstructionCreateOptions } from "./generated/InstructionCreateOptions";
import type { ProjectView } from "./generated/ProjectView";
import type { SourceKind } from "./generated/SourceKind";
import { ScopeSwitcher } from "./components/scope_switcher/ScopeSwitcher";
import { LoadingState } from "./components/states";
import type { StateRouteId } from "./components/states";
import { Modal, ModalHost, OverlayStackProvider } from "./overlays";
import { ToastProvider } from "./overlays/toasts";
import { createAppState } from "./state/AppState";
import { AppStateProvider } from "./state/context";
import { Shell } from "./shell";
import { RouteErrorBoundary } from "./shell/RouteErrorBoundary";
import { shellAgentStatus } from "./shell/agents";
import { commandErrorMessage } from "./lib/commandError";
import { parseItemDeepLink, SHELL_SECTION_LABEL, sectionForRoute } from "./shell/routes";

import { AppOverlays } from "./overlays/AppOverlays";
import { OverviewScreen } from "./screens/overview/OverviewScreen";
import { DriftBanner } from "./screens/drift/DriftBanner";
import { ToastEventBridge } from "./overlays/toasts/ToastEventBridge";
import type { ScopeToken } from "./theme";

const loadSkillsScreen = () => import("./screens/skills/SkillsScreen");
const loadMcpListScreen = () => import("./screens/mcp_list/McpListScreen");

/**
 * Routed screens load on demand (route-level code splitting, TF-035). The
 * frequently used list routes are fetched after the shell paints so the first
 * click can use the browser's module cache without inflating the entry chunk.
 */
const EffectiveScreen = lazy(() => import("./screens/effective/EffectiveScreen").then((m) => ({ default: m.EffectiveScreen })));
const MigrateScanScreen = lazy(() => import("./screens/migrate/MigrateScanScreen").then((m) => ({ default: m.MigrateScanScreen })));
const AddSkillWizard = lazy(() => import("./screens/add_skill/AddSkillWizard").then((m) => ({ default: m.AddSkillWizard })));
const SecretInventoryScreen = lazy(() => import("./screens/secrets/SecretInventoryScreen").then((m) => ({ default: m.SecretInventoryScreen })));
const SkillDetailScreen = lazy(() => import("./screens/skill_detail/SkillDetailScreen").then((m) => ({ default: m.SkillDetailScreen })));
const PreferencesScreen = lazy(() => import("./screens/preferences/PreferencesScreen").then((m) => ({ default: m.PreferencesScreen })));
const InstructionsScreen = lazy(() => import("./screens/instructions/InstructionsScreen").then((m) => ({ default: m.InstructionsScreen })));
const AgentsScreen = lazy(() => import("./screens/agents/AgentsScreen").then((m) => ({ default: m.AgentsScreen })));
const PresetsScreen = lazy(() => import("./screens/presets/PresetsScreen").then((m) => ({ default: m.PresetsScreen })));
const McpFormRoute = lazy(() => import("./screens/mcp_form/McpFormRoute").then((m) => ({ default: m.McpFormRoute })));
const McpListScreen = lazy(() => loadMcpListScreen().then((m) => ({ default: m.McpListScreen })));
const SkillsScreen = lazy(() => loadSkillsScreen().then((m) => ({ default: m.SkillsScreen })));
const McpDetailScreen = lazy(() => import("./screens/mcp_detail/McpDetailScreen").then((m) => ({ default: m.McpDetailScreen })));
const DriftScreen = lazy(() => import("./screens/drift/DriftScreen").then((m) => ({ default: m.DriftScreen })));

/** Suspense fallback for a route chunk still loading; the StateView loading state. */
function RouteSuspenseFallback(): JSX.Element {
  return (
    <div className="mx-auto flex w-full max-w-[1200px] flex-col p-24" role="presentation">
      <LoadingState title="Loading screen…" skeletonRows={8} minHeight={600} />
    </div>
  );
}

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
/** Routed body; each screen is a lazy chunk loaded on first visit (TF-035). */
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
  if (route === "add-skill") return <AddSkillWizard onClose={onClose} projectId={projectId} initialScope={projectId !== null ? "project" : scope} initialSource={initialSkillSource} />;
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
    // Route-level code splitting means the Skills chunk otherwise arrives inside the first
    // click. `requestIdleCallback` is not a fix: it only fires once the main thread idles,
    // which happens at or after the earliest realistic click (TF-035). Fetch right after the
    // shell's first paint instead, and let the sidebar's pointerdown/focus prefetch cover an
    // even earlier interaction.
    if (!bootstrapped) return;
    let cancelled = false;
    const frame = globalThis.requestAnimationFrame(() => {
      // A microtask after the frame callback keeps the fetch out of that frame's paint.
      void Promise.resolve().then(() => {
        if (cancelled) return;
        void loadSkillsScreen().catch(() => undefined);
        void loadMcpListScreen().catch(() => undefined);
      });
    });
    return () => {
      cancelled = true;
      globalThis.cancelAnimationFrame(frame);
    };
  }, [bootstrapped]);
  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(commandErrorMessage(e)));
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
      setError(commandErrorMessage(cause));
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
                <RouteErrorBoundary>
                  <Suspense fallback={<RouteSuspenseFallback />}>
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
                  </Suspense>
                </RouteErrorBoundary>
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
