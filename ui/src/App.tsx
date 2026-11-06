import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState, useSyncExternalStore } from "react";
import type { AppInfo } from "./generated/AppInfo";
import { StateView, SURFACE_STATE_FIXTURES } from "./components/states";
import type { StateRouteId } from "./components/states";
import { ScopeSwitcher } from "./components/scope_switcher/ScopeSwitcher";
import { ModalHost, OverlayStackProvider } from "./overlays";
import { ToastProvider } from "./overlays/toasts";
import { createAppState } from "./state/AppState";
import { AppStateProvider } from "./state/context";
import { Shell } from "./shell";
import { SHELL_SECTION_LABEL, sectionForRoute } from "./shell/routes";

/** Placeholder routed body; real screen contents are supplied by their stories. */
function RouteBody({ route }: { readonly route: StateRouteId }) {
  const section = sectionForRoute(route);
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
  const [appState] = useState(() => createAppState());
  const appSnapshot = useSyncExternalStore(appState.subscribe, appState.getSnapshot, appState.getSnapshot);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const route = appSnapshot.route;

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(String(e)));
  }, []);

  const versionLabel = info === null ? "v… · local" : `v${info.version} · local`;
  const section = sectionForRoute(route);
  const title = section === null ? "Settings" : SHELL_SECTION_LABEL[section];

  return (
    <AppStateProvider state={appState}>
      <ToastProvider store={appState.toasts}>
        <OverlayStackProvider>
          <ModalHost>
            <Shell
              route={route}
              title={title}
              projectLabel={appSnapshot.activeProject}
              scope={appSnapshot.scope}
              scopeSwitcher={
                <ScopeSwitcher
                  scope={appSnapshot.scope}
                  localEnabled={false}
                  hasProject={appSnapshot.activeProject !== null}
                  onScopeSelected={appState.receiveScope}
                />
              }
              versionLabel={versionLabel}
              onNavigate={(next) => appState.navigate(next, "discard")}
              onProjectPicker={() => undefined}
              onPalette={() => undefined}
            >
              {error === null ? (
                <RouteBody route={route} />
              ) : (
                <div className="p-24 text-danger">Backend error: {error}</div>
              )}
            </Shell>
          </ModalHost>
        </OverlayStackProvider>
      </ToastProvider>
    </AppStateProvider>
  );
}
