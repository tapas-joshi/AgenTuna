import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { AppInfo } from "./generated/AppInfo";
import { ModalHost, OverlayStackProvider } from "./overlays";
import { ToastProvider } from "./overlays/toasts";
import { StateView, SURFACE_STATE_FIXTURES } from "./components/states";
import type { StateRouteId } from "./components/states";
import { Shell } from "./shell";
import { SHELL_SECTION_LABEL, sectionForRoute } from "./shell/routes";

/**
 * Placeholder routed body: shows the real empty state for the active route
 * until TU-009/TU-010 land the state store and router. No fake populated
 * data — the fixture's own empty copy is what renders.
 */
function RouteBody({ route }: { readonly route: StateRouteId }) {
  const section = sectionForRoute(route);
  const fixture = SURFACE_STATE_FIXTURES[route];
  const empty = fixture.empty;
  return (
    <div className="mx-auto flex max-w-[720px] flex-col gap-16 p-24">
      <h1 className="m-0 text-22 font-semibold text-text">
        {section === null ? "Settings" : SHELL_SECTION_LABEL[section]}
      </h1>
      <StateView
        variant={empty.variant}
        title={empty.title}
        body={empty.body}
        actions={empty.action === undefined ? [] : [{ label: empty.action, onClick: () => {} }]}
        minHeight={320}
      />
    </div>
  );
}

export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [route, setRoute] = useState<StateRouteId>("overview");

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(String(e)));
  }, []);

  const versionLabel = info === null ? "v… · local" : `v${info.version} · local`;
  const section = sectionForRoute(route);
  const title = section === null ? "Settings" : SHELL_SECTION_LABEL[section];

  return (
    <ToastProvider>
      <OverlayStackProvider>
        <ModalHost>
          <Shell
            route={route}
            title={title}
            projectLabel={null}
            versionLabel={versionLabel}
            onNavigate={setRoute}
            onProjectPicker={() => {}}
            onPalette={() => {}}
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
  );
}
