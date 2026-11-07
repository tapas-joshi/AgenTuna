import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type { UpdateInstallGate } from "../../generated/UpdateInstallGate";
import type { UpdateInstallStatus } from "../../generated/UpdateInstallStatus";

interface AppInfo {
  readonly name: string;
  readonly version: string;
}

const DEFER_TEXT: Record<string, string> = {
  operation_running: "Install waits for the running operation to finish.",
  job_running: "Install waits for the running job to finish.",
  editor_dirty: "Install waits until unsaved editor changes are resolved.",
  modal_open: "Install waits for the open dialog to close.",
};

/**
 * App updates: one explicit check, one explicit install, and the queued/installed/failed states
 * the backend owns. All policy lives in the Rust commands; this section only renders and invokes.
 */
export function AppUpdatesSection() {
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [gate, setGate] = useState<UpdateInstallGate | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    void invoke<UpdateInstallGate>("app_update_status").then(setGate, (error: unknown) => setFailure(String(error)));
  }, []);

  useEffect(() => {
    void invoke<AppInfo>("app_info").then(info => setAppVersion(info.version), () => setAppVersion(null));
    refresh();
  }, [refresh]);

  useEffect(() => {
    let stop: (() => void) | undefined;
    void listen("agentuna:update-install-finished", () => refresh()).then(unlisten => { stop = unlisten; }, () => undefined);
    return () => stop?.();
  }, [refresh]);

  // While a request is queued, retry it every few seconds. The backend still refuses while a job,
  // operation or tracked activity is running, and Preferences is only reachable when no editor
  // is dirty.
  const statusState = gate?.status?.state ?? null;
  useEffect(() => {
    if (statusState !== "queued") return;
    const timer = window.setInterval(() => {
      void invoke<UpdateInstallGate>("app_update_install_resume", { editorDirty: false, modalOpen: false }).then(setGate, (error: unknown) => setFailure(String(error)));
    }, 2000);
    return () => window.clearInterval(timer);
  }, [statusState]);

  const run = async (action: () => Promise<UpdateInstallGate>) => {
    setBusy(true); setFailure(null);
    try { setGate(await action()); } catch (error) { setFailure(String(error)); }
    finally { setBusy(false); }
  };

  const status = gate?.status ?? null;
  const installing = status?.state === "installing";
  return (
    <section aria-label="App updates" className="rounded-10 border border-border bg-surface p-16">
      <h2 className="m-0 text-15 font-semibold text-text">App updates</h2>
      <p className="mt-4 text-12 text-muted">Current version {appVersion ?? "…"}. Checks are explicit; nothing is downloaded at launch or while idle.</p>
      {failure && <p role="alert" className="mt-8 text-13 text-danger">{failure}</p>}
      <div className="mt-12 space-y-10">
        <StatusLine status={status} />
        <div className="flex flex-wrap gap-8">
          <button
            type="button"
            className="rounded-6 border border-border bg-raised px-12 py-8 text-13 text-text disabled:opacity-50"
            disabled={busy || installing}
            onClick={() => void run(() => invoke<UpdateInstallGate>("app_update_check"))}
          >
            Check for updates
          </button>
          <button
            type="button"
            className="rounded-6 border border-border bg-raised px-12 py-8 text-13 text-text disabled:opacity-50"
            disabled={busy || !gate?.can_install || status?.state !== "ready"}
            onClick={() => {
              if (status?.state !== "ready") return;
              void run(() => invoke<UpdateInstallGate>("app_update_install_request", { version: status.version, editorDirty: false, modalOpen: false }));
            }}
          >
            Install {status?.state === "ready" ? status.version : "update"}
          </button>
          <button
            type="button"
            className="rounded-6 border border-border px-12 py-8 text-13 text-text disabled:opacity-50"
            disabled={busy || !gate?.can_cancel}
            onClick={() => void run(() => invoke<UpdateInstallGate>("app_update_install_cancel"))}
          >
            Cancel queued install
          </button>
        </div>
      </div>
    </section>
  );
}

function StatusLine({ status }: { readonly status: UpdateInstallStatus | null }) {
  if (status === null) return <p role="status" className="m-0 text-13 text-secondary">Loading update status…</p>;
  switch (status.state) {
    case "idle":
      return <p role="status" className="m-0 text-13 text-secondary">No update is staged. Use Check for updates to look for one.</p>;
    case "ready":
      return <p role="status" className="m-0 text-13 text-text">Version {status.version} is verified and ready to install. AgenTuna restarts after installing.</p>;
    case "queued":
      return <p role="status" className="m-0 text-13 text-warning">Version {status.version} is queued since {status.requested_at}. {DEFER_TEXT[status.reason] ?? "Install waits until AgenTuna is idle."}</p>;
    case "installing":
      return <p role="status" className="m-0 text-13 text-text">Installing version {status.version}…</p>;
    case "installed":
      return <p role="status" className="m-0 text-13 text-success">Version {status.version} is installed. AgenTuna is restarting.</p>;
    case "failed":
      return (
        <div role="alert" className="text-13 text-danger">
          <p className="m-0">{status.version ? `Version ${status.version} could not be installed. ` : ""}{status.message}</p>
          <p className="m-0 mt-4 text-secondary">{status.remedy}</p>
        </div>
      );
    case "rolled_back":
      return (
        <div role="alert" className="text-13 text-warning">
          <p className="m-0">Version {status.version} did not start. {status.reason}</p>
          <p className="m-0 mt-4 text-secondary">{status.remedy}</p>
        </div>
      );
    default:
      return <p role="status" className="m-0 text-13 text-secondary">Update status is unavailable.</p>;
  }
}
