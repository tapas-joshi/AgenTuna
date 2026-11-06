import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState, useSyncExternalStore } from "react";
import type { AppInfo } from "./generated/AppInfo";
import type { ProjectView } from "./generated/ProjectView";
import { StateView, SURFACE_STATE_FIXTURES } from "./components/states";
import type { StateRouteId } from "./components/states";
import { ScopeSwitcher } from "./components/scope_switcher/ScopeSwitcher";
import { Modal, ModalHost, OverlayStackProvider } from "./overlays";
import { ToastProvider } from "./overlays/toasts";
import { createAppState } from "./state/AppState";
import { AppStateProvider } from "./state/context";
import { Shell } from "./shell";
import { SHELL_SECTION_LABEL, parseRouteId, sectionForRoute } from "./shell/routes";

import { PreferencesScreen } from "./screens/preferences/PreferencesScreen";
import { AppOverlays } from "./overlays/AppOverlays";

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
/** Placeholder routed body; real screen contents are supplied by their stories. */
function RouteBody({ route }: { readonly route: StateRouteId }) {
  const section = sectionForRoute(route);
  if (route === "settings") return <PreferencesScreen />;
  const empty = SURFACE_STATE_FIXTURES[route].empty;
  return (
    <div className="mx-auto flex max-w-[720px] flex-col gap-16 p-24">
      <h1 className="m-0 text-22 font-semibold text-text">
        {section === null ? "Settings" : SHELL_SECTION_LABEL[section]}
      </h1>
      <StateView
        variant={empty.variant}
        title={empty.title}
        body={empty.body}
        actions={empty.action === undefined ? [] : [{ label: empty.action, onClick: () => undefined }]}
        minHeight={320}
      />
    </div>
  );
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
      if (effect.type === "project-picker-required") setPendingNavigation(null);
    },
  }));
  const appSnapshot = useSyncExternalStore(appState.subscribe, appState.getSnapshot, appState.getSnapshot);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [bootstrapped, setBootstrapped] = useState(false);
  const route = appSnapshot.route;

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(String(e)));
  }, []);
  useEffect(() => {
    let active = true;
    void Promise.all([
      invoke<{ readonly values?: Record<string, unknown> }>("settings_load"),
      invoke<ProjectView[]>("projects_list"),
    ]).then(([document, projects]) => {
      if (!active) return;
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
    if (pendingScope === null) return;
    if (appSnapshot.activeProject === pendingScope.projectId) appState.receiveScope(pendingScope.scope);
    setPendingScope(null);
  }, [appSnapshot.activeProject, appState, pendingScope]);
  useEffect(
    () =>
      subscribe<string>("deep-link", (payload) => {
        const match = /^agentuna:\/\/item\/([^/?#]+)$/.exec(payload);
        const itemId = match?.[1];
        if (itemId === undefined) return;
        void invoke<{ readonly kind: string; readonly scope: string; readonly project_id: string | null } | null>("item_lookup", { itemId }).then(
          (item) => appState.openItem(itemId, item),
          () => appState.openItem(itemId, null),
        );
      }),
    [appState],
  );
  useEffect(
    () =>
      subscribe<string>("menu-action", (payload) => {
        if (!payload.startsWith("navigate:")) return;
        const target = parseRouteId(payload.slice("navigate:".length));
        if (target !== null) appState.navigate(target);
      }),
    [appState],
  );
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === "[") {
        event.preventDefault();
        appState.back();
        return;
      }
      if (!(event.metaKey || event.ctrlKey) || !event.shiftKey || event.altKey) return;
      if (event.target instanceof HTMLElement && /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return;
      const scope = ({ "1": "global", "2": "user", "3": "project", "4": "local" } as const)[event.key as "1" | "2" | "3" | "4"];
      if (scope === undefined) return;
      event.preventDefault();
      appState.receiveScope(scope);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [appState]);

  const versionLabel = info === null ? "v… · local" : `v${info.version} · local`;
  const section = sectionForRoute(route);
  const title = section === null ? "Settings" : SHELL_SECTION_LABEL[section];

  if (!bootstrapped) return <main role="status" aria-live="polite" className="grid h-full place-items-center text-secondary">Loading AgenTuna…</main>;
  return (
    <AppStateProvider state={appState}>
      <ToastProvider store={appState.toasts}>
        <OverlayStackProvider>
          <ModalHost>
            <AppOverlays state={appState} scope={appSnapshot.scope} route={route} hasProject={appSnapshot.activeProject !== null}>
              {(overlaySlots) => (
                <>
            <Shell
              route={route}
              title={title}
              projectLabel={appSnapshot.activeProject}
              scope={appSnapshot.scope}
              scopeSwitcher={
                <ScopeSwitcher
                  scope={appSnapshot.scope}
                  localEnabled={appSnapshot.activeProject !== null}
                  hasProject={appSnapshot.activeProject !== null}
                  onScopeSelected={appState.receiveScope}
                />
              }
              versionLabel={versionLabel}
              onNavigate={(next) => appState.navigate(next)}
              onProjectPicker={overlaySlots.onProjectPicker}
              onPalette={overlaySlots.onPalette}
              addControl={overlaySlots.addControl}
              overlays={overlaySlots.overlays}
            >
              {error === null ? (
                <RouteBody route={route} />
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
