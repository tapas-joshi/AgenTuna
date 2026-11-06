import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { AppInfo } from "./generated/AppInfo";
import { ToastProvider } from "./overlays/toasts";

export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo, (e: unknown) => setError(String(e)));
  }, []);

  return (
    <ToastProvider>
      <main className="p-8">
        <h1 className="m-0 font-mono text-26 font-semibold">{info?.name ?? "AgenTuna"}</h1>
        <p className="text-text-muted">
          {error ? `Backend error: ${error}` : info ? `v${info.version} · local` : "Loading…"}
        </p>
      </main>
    </ToastProvider>
  );
}
