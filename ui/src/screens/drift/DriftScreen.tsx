import { useCallback, useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button } from "../../components/button";
import { DiffViewer } from "../../components/diff/diff-viewer";
import type { BackupPreviewView } from "../../generated/BackupPreviewView";
import type { BackupRestoreResultView } from "../../generated/BackupRestoreResultView";
import type { BackupRowView } from "../../generated/BackupRowView";
import type { DriftPreviewView } from "../../generated/DriftPreviewView";
import type { DriftRowView } from "../../generated/DriftRowView";
import type { DriftSnapshotView } from "../../generated/DriftSnapshotView";
import type { DriftWatcherStatusView } from "../../generated/DriftWatcherStatusView";
import { Modal } from "../../overlays/dialogs/Modal";
import { useToast } from "../../overlays/toasts/ToastProvider";
import { showBackupRestoredToast } from "../../overlays/toasts/catalogue";
import { useAppState } from "../../state/context";
import { commandErrorMessage } from "../../lib/commandError";

type Tab = "drift" | "backups";
type DriftAction = "adopt" | "restore";

type Confirmation =
  | { readonly kind: "drift"; readonly action: DriftAction; readonly row: DriftRowView }
  | { readonly kind: "backup"; readonly backup: BackupRowView }
  | null;

const KIND_LABEL: Readonly<Record<string, string>> = {
  drifted: "Changed on disk",
  link_replaced_by_file: "Managed link replaced",
  link_removed: "Managed link removed",
  file_deleted: "File deleted",
  store_file_edited_while_linked: "Store file edited while linked",
  unreadable: "Permission or read error",
};

const WATCHER_STATUS_EVENT = "agentuna:drift-watcher-status";

function failureMessage(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  if (typeof cause === "object" && cause !== null && "message" in cause && typeof cause.message === "string") {
    return cause.message;
  }
  return commandErrorMessage(cause);
}

function formattedTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

function formattedSize(value: number | bigint | null): string {
  if (value === null) return "Unknown size";
  const bytes = typeof value === "bigint" ? Number(value) : value;
  return `${bytes.toLocaleString()} B`;
}

function WatcherBadge({ watcher }: { readonly watcher: DriftWatcherStatusView }): JSX.Element {
  const style = watcher.status === "active"
    ? "border-success/30 bg-success/10 text-success"
    : watcher.status === "error"
      ? "border-danger/30 bg-danger/10 text-danger"
      : "border-warning/30 bg-warning/10 text-warning";
  const dot = watcher.status === "active" ? "bg-success" : watcher.status === "error" ? "bg-danger" : "bg-warning";
  const label = watcher.status === "active"
    ? "Watching"
    : watcher.status === "degraded"
      ? "Watching (degraded)"
      : watcher.status === "error"
        ? "Not watching"
        : watcher.status === "starting"
          ? "Starting…"
          : "Paused";
  return <span className={`inline-flex items-center gap-6 rounded-full border px-10 py-5 text-12 max-[1199px]:gap-8 max-[1199px]:px-12 max-[1199px]:py-4 ${style}`}>
    <span aria-hidden="true" className={`h-7 w-7 rounded-full max-[1199px]:size-8 ${dot}`} />
    {label}
  </span>;
}

/** Persisted external drift review and safe restore access (TC-026). */
export function DriftScreen(): JSX.Element {
  const app = useAppState();
  const toast = useToast();
  const [tab, setTab] = useState<Tab>("drift");
  const [snapshot, setSnapshot] = useState<DriftSnapshotView | null>(null);
  const [backups, setBackups] = useState<readonly BackupRowView[]>([]);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [selectedEvent, setSelectedEvent] = useState<number | null>(null);
  const [selectedBackupId, setSelectedBackupId] = useState<string | null>(null);
  const [preview, setPreview] = useState<DriftPreviewView | null>(null);
  const [backupPreview, setBackupPreview] = useState<BackupPreviewView | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const loadSnapshot = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<DriftSnapshotView>("drift_snapshot");
      setSnapshot(result);
      setSelectedEvent((current) => {
        const storedSelection = app.getSnapshot().selections.drift?.[0];
        const selected = current ?? (storedSelection ? Number(storedSelection) : null);
        if (selected !== null && result.rows.some((row) => Number(row.event_id) === selected)) return selected;
        const firstEvent = result.rows[0]?.event_id;
        return firstEvent === undefined ? null : Number(firstEvent);
      });
    } catch (cause) {
      setError(failureMessage(cause));
    } finally {
      setLoading(false);
    }
  }, [app]);

  const loadBackups = useCallback(async () => {
    setBackupsLoading(true);
    try {
      const result = await invoke<BackupRowView[]>("backups_list");
      setBackups(result);
      setSelectedBackupId((current) => result.some((row) => row.id === current) ? current : result[0]?.id ?? null);
    } catch (cause) {
      setError(failureMessage(cause));
    } finally {
      setBackupsLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadSnapshot();
  }, [loadSnapshot]);

  useEffect(() => {
    // The backend retries the watcher and updates the condition in the background; the event
    // keeps the badge and banners current without polling.
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    void listen<DriftWatcherStatusView>(WATCHER_STATUS_EVENT, ({ payload }) => {
      setSnapshot((current) => (current ? { ...current, watcher: payload } : current));
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    }).catch(() => undefined);
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (selectedEvent === null) {
      setPreview(null);
      return;
    }
    let active = true;
    setPreview(null);
    void invoke<DriftPreviewView>("drift_preview", { eventId: selectedEvent }).then(
      (result) => { if (active) setPreview(result); },
      (cause: unknown) => { if (active) setActionError(failureMessage(cause)); },
    );
    return () => { active = false; };
  }, [selectedEvent]);

  useEffect(() => {
    if (tab !== "backups") return;
    void loadBackups();
  }, [loadBackups, tab]);

  useEffect(() => {
    if (selectedBackupId === null || tab !== "backups") {
      setBackupPreview(null);
      return;
    }
    let active = true;
    setBackupPreview(null);
    void invoke<BackupPreviewView>("backup_preview", { backupId: selectedBackupId }).then(
      (result) => { if (active) setBackupPreview(result); },
      (cause: unknown) => { if (active) setActionError(failureMessage(cause)); },
    );
    return () => { active = false; };
  }, [selectedBackupId, tab]);

  const selectedRow = useMemo(
    () => snapshot?.rows.find((row) => Number(row.event_id) === selectedEvent) ?? null,
    [selectedEvent, snapshot?.rows],
  );
  const selectedBackup = useMemo(
    () => backups.find((row) => row.id === selectedBackupId) ?? null,
    [backups, selectedBackupId],
  );

  async function rescan() {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<DriftSnapshotView>("drift_rescan");
      setSnapshot(result);
      setSelectedEvent((current) => {
        if (current !== null && result.rows.some((row) => Number(row.event_id) === current)) return current;
        const firstEvent = result.rows[0]?.event_id;
        return firstEvent === undefined ? null : Number(firstEvent);
      });
    } catch (cause) {
      setError(failureMessage(cause));
    } finally {
      setLoading(false);
    }
  }

  async function toggleWatcher() {
    if (!snapshot) return;
    setActionError(null);
    try {
      const watcher = await invoke<DriftWatcherStatusView>("drift_watcher_set_paused", {
        paused: snapshot.watcher.status === "active",
      });
      setSnapshot({ ...snapshot, watcher });
    } catch (cause) {
      setActionError(failureMessage(cause));
      await loadSnapshot();
    }
  }

  async function retryWatcher() {
    setActionError(null);
    try {
      const watcher = await invoke<DriftWatcherStatusView>("drift_watcher_set_paused", {
        paused: false,
      });
      setSnapshot((current) => (current ? { ...current, watcher } : current));
    } catch (cause) {
      setActionError(failureMessage(cause));
    }
  }

  async function applyDriftAction() {
    if (confirmation?.kind !== "drift") return;
    const { row, action } = confirmation;
    setBusy(true);
    setActionError(null);
    try {
      await invoke(action === "adopt" ? "drift_adopt" : "drift_restore", { eventId: Number(row.event_id) });
      setConfirmation(null);
      await loadSnapshot();
    } catch (cause) {
      setActionError(failureMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  async function restoreBackup() {
    if (confirmation?.kind !== "backup") return;
    const { backup } = confirmation;
    if (backupPreview === null || backupPreview.id !== backup.id) {
      setActionError("Preview the saved version again before restoring it.");
      return;
    }
    setBusy(true);
    setActionError(null);
    try {
      const outcome = await invoke<BackupRestoreResultView>("backup_restore", {
        request: {
          backup_id: backup.id,
          preview_id: backupPreview.preview_id,
          destination_hash: backupPreview.destination_hash,
        },
      });
      showBackupRestoredToast(toast, outcome.path);
      setConfirmation(null);
      await Promise.all([loadSnapshot(), loadBackups()]);
    } catch (cause) {
      setActionError(failureMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  const watcher = snapshot?.watcher ?? null;

  return <main aria-label="Sync and drift" className="flex min-h-0 w-full flex-1 flex-col gap-16 p-20 max-[1199px]:p-24">
    <header className="flex flex-wrap items-start justify-between gap-12">
      <div className="grid gap-5">
        <h1 className="m-0 text-26 font-semibold text-text">Sync &amp; drift</h1>
        <p className="m-0 text-13 text-text-secondary">Files edited outside AgenTuna. Review each change before AgenTuna adopts it.</p>
      </div>
      <div className="flex items-center gap-8">
        {snapshot && <span className="rounded-8 border border-border bg-raised px-10 py-7 text-12 text-text-secondary max-[1199px]:px-12 max-[1199px]:py-8"><strong className="font-mono text-text">{snapshot.in_sync_count}</strong> items in sync</span>}
        {watcher && <WatcherBadge watcher={watcher} />}
        <Button size="sm" variant="secondary" disabled={!watcher || watcher.status === "error"} onClick={() => void toggleWatcher()}>
          {watcher?.status === "active" ? "Pause watching" : "Resume watching"}
        </Button>
      </div>
    </header>

    {watcher?.status === "error" && <div role="alert" className="flex flex-wrap items-center justify-between gap-8 rounded-8 border border-danger/30 bg-danger/10 p-10 text-12 text-text">
      <span>Not watching: {watcher.error ?? "the filesystem watcher did not start."} {watcher.path && <span className="font-mono">{watcher.path}</span>}</span>
      <span className="flex items-center gap-8">
        <Button size="sm" variant="secondary" onClick={() => void retryWatcher()}>Retry</Button>
        <Button size="sm" variant="secondary" loading={loading} onClick={() => void rescan()}>Rescan</Button>
      </span>
    </div>}
    {watcher?.status === "degraded" && <div role="status" className="flex flex-wrap items-center justify-between gap-8 rounded-8 border border-warning/30 bg-warning/10 p-10 text-12 text-text">
      <span>Watching with reduced coverage: {watcher.error ?? "some roots are degraded."} Affected paths are covered by periodic rescans.</span>
      <Button size="sm" variant="secondary" loading={loading} onClick={() => void rescan()}>Rescan now</Button>
    </div>}
    {error && <div role="alert" className="flex items-center justify-between gap-8 rounded-8 border border-danger/30 bg-danger/10 p-10 text-12 text-danger">{error}<Button size="sm" onClick={() => void loadSnapshot()}>Retry</Button></div>}
    {actionError && <div role="alert" className="flex items-center justify-between gap-8 rounded-8 border border-danger/30 bg-danger/10 p-10 text-12 text-danger">{actionError}<Button size="sm" variant="ghost" onClick={() => setActionError(null)}>Dismiss</Button></div>}

    <div role="tablist" aria-label="Sync and drift views" className="flex border-b border-border">
      <button type="button" role="tab" aria-selected={tab === "drift"} onClick={() => setTab("drift")} className={`border-b-2 px-12 py-8 text-13 ${tab === "drift" ? "border-accent font-semibold text-text" : "border-transparent text-text-muted"}`}>Drift{snapshot?.rows.length ? ` (${snapshot.rows.length})` : ""}</button>
      <button type="button" role="tab" aria-selected={tab === "backups"} onClick={() => setTab("backups")} className={`border-b-2 px-12 py-8 text-13 ${tab === "backups" ? "border-accent font-semibold text-text" : "border-transparent text-text-muted"}`}>Backups</button>
    </div>

    {tab === "drift" ? <section role="tabpanel" aria-label="Drift" className="grid min-h-0 flex-1 grid-cols-[minmax(250px,340px)_minmax(0,1fr)] gap-12">
      <div className="flex min-h-0 flex-col rounded-10 border border-border bg-raised">
        <div className="flex items-center justify-between border-b border-border px-12 py-10 max-[1199px]:py-12"><h2 className="m-0 text-13 font-semibold text-text">Changed items</h2><Button size="sm" variant="ghost" loading={loading} onClick={() => void rescan()}>Rescan</Button></div>
        {loading && snapshot === null ? <p role="status" className="m-12 text-12 text-text-muted">Checking watched files…</p> : snapshot?.rows.length ? <ul aria-label="Drifted items" className="m-0 min-h-0 list-none overflow-auto p-0">
          {snapshot.rows.map((row) => <li key={row.event_id} className="border-b border-border last:border-0">
            <button type="button" aria-current={Number(row.event_id) === selectedEvent ? "true" : undefined} onClick={() => { setActionError(null); setSelectedEvent(Number(row.event_id)); app.setSelection("drift", [String(row.event_id)]); }} className={`grid w-full gap-4 px-12 py-10 text-left max-[1199px]:py-12 ${Number(row.event_id) === selectedEvent ? "bg-accent/10" : "hover:bg-overlay"}`}>
              <span className="flex items-center justify-between gap-6 max-[1199px]:gap-8"><strong className="truncate text-13 text-text">{row.item_name}</strong><span className="rounded-full bg-warning/10 px-7 py-3 text-11 text-warning max-[1199px]:px-8 max-[1199px]:py-4">{KIND_LABEL[row.kind] ?? row.kind}</span></span>
              <span className="text-11 text-text-secondary">{row.agent} · {row.scope}</span>
              <span className="truncate font-mono text-11 text-text-muted" title={row.path}>{row.path}</span>
              <time className="text-11 text-text-muted" dateTime={row.detected_at}>{formattedTime(row.detected_at)}</time>
            </button>
          </li>)}
        </ul> : <div className="grid flex-1 content-center justify-items-center gap-8 p-20 text-center"><span aria-hidden="true" className="grid h-36 w-36 place-items-center rounded-full bg-success/10 text-success">✓</span><h2 className="m-0 text-15 font-semibold text-text">Everything is in sync</h2><p className="m-0 text-12 text-text-muted">AgenTuna found no external changes in {snapshot?.watched_count ?? 0} watched paths.</p></div>}
      </div>

      <div className="flex min-h-0 flex-col gap-10 rounded-10 border border-border bg-raised p-12">
        {!selectedRow ? <div className="grid flex-1 content-center justify-items-center text-center"><h2 className="m-0 text-15 font-semibold text-text">Select a changed item</h2><p className="mt-6 text-12 text-text-muted">The last known AgenTuna version and the current on-disk content will appear here.</p></div> : <>
          <div className="flex flex-wrap items-start justify-between gap-10 border-b border-border pb-10">
            <div className="grid min-w-0 gap-4"><h2 className="m-0 text-15 font-semibold text-text">{selectedRow.item_name}</h2><p className="m-0 break-all font-mono text-11 text-text-muted">{selectedRow.path}</p><p className="m-0 text-11 text-text-secondary">{selectedRow.agent} · {selectedRow.scope} · {formattedTime(selectedRow.detected_at)}</p></div>
            <div className="flex flex-wrap gap-6">
              <Button size="sm" variant="secondary" disabled={!selectedRow.can_adopt || busy} onClick={() => setConfirmation({ kind: "drift", action: "adopt", row: selectedRow })}>Adopt on-disk</Button>
              <Button size="sm" variant="secondary" disabled={!selectedRow.can_restore || busy} onClick={() => setConfirmation({ kind: "drift", action: "restore", row: selectedRow })}>Restore AgenTuna version</Button>
              <Button size="sm" variant="ghost" onClick={() => void invoke("drift_open_in_editor", { eventId: Number(selectedRow.event_id) }).catch((cause: unknown) => setActionError(failureMessage(cause)))}>Open in editor</Button>
            </div>
          </div>
          {selectedRow.kind === "unreadable" && <p role="alert" className="m-0 rounded-6 border border-danger/30 bg-danger/10 p-8 text-12 text-danger">AgenTuna cannot read this path. Check its permissions, then rescan.</p>}
          <div className="min-h-0 flex-1 overflow-auto">
            {preview ? preview.previous_version_available
              ? <DiffViewer diff={preview.diff} displayPath={preview.path} mode="side-by-side" />
              : <section aria-label="On-disk content" className="grid min-h-0 flex-1 content-start gap-8 overflow-auto">
                <p role="status" className="m-0 rounded-8 border border-warning/30 bg-warning/10 p-10 text-12 text-warning">Baseline content not retained: {preview.baseline_unavailable_reason ?? "No recovery reason was provided."}</p>
                <h3 className="m-0 text-13 font-semibold text-text">On-disk content</h3>
                {preview.on_disk_content !== null ? <pre aria-label="Current on-disk content" className="m-0 whitespace-pre-wrap rounded-8 border border-border bg-surface p-12 font-mono text-12 text-text">{preview.on_disk_content}</pre> : <p className="m-0 text-12 text-text-muted">No UTF-8 content is available for preview.</p>}
              </section>
              : <p role="status" className="m-8 text-12 text-text-muted">Loading the reviewed file comparison…</p>}
          </div>
        </>}
      </div>
    </section> : <section role="tabpanel" aria-label="Backups" className="grid min-h-0 flex-1 grid-cols-[minmax(350px,1fr)_minmax(0,1fr)] gap-12">
      <div className="min-h-0 overflow-auto rounded-10 border border-border bg-raised">
        <table className="w-full border-collapse text-left text-12"><thead className="sticky top-0 bg-overlay text-text-muted"><tr><th className="p-8 font-medium">Created</th><th className="p-8 font-medium">Original path</th><th className="p-8 font-medium">Size</th><th className="p-8 font-medium">Operation</th><th className="p-8 font-medium">Action</th></tr></thead><tbody>
          {backups.map((backup) => <tr key={backup.id} aria-selected={backup.id === selectedBackupId} className={`border-t border-border ${backup.id === selectedBackupId ? "bg-accent/10" : ""}`}>
            <td className="whitespace-nowrap p-8 text-text-secondary">{formattedTime(backup.created_at)}</td>
            <td className="max-w-[280px] p-8"><button type="button" className="block max-w-full truncate font-mono text-11 text-text hover:text-accent" title={backup.original_path ?? "Destination unavailable"} onClick={() => setSelectedBackupId(backup.id)}>{backup.original_path ?? "Destination unavailable"}</button></td>
            <td className="whitespace-nowrap p-8 text-text-secondary">{formattedSize(backup.size)}</td>
            <td className="max-w-[120px] truncate p-8 font-mono text-11 text-text-muted" title={backup.operation_id ?? "Unknown operation"}>{backup.operation_id ?? "—"}</td>
            <td className="p-8"><Button size="sm" variant="secondary" disabled={!backup.restorable} title={backup.unavailable_reason ?? undefined} onClick={() => { setSelectedBackupId(backup.id); setConfirmation({ kind: "backup", backup }); }}>Restore</Button></td>
          </tr>)}
        </tbody></table>
        {backupsLoading && <p role="status" className="p-12 text-12 text-text-muted">Loading file backups…</p>}
        {!backupsLoading && backups.length === 0 && <p role="status" className="p-16 text-12 text-text-muted">No file backups are available.</p>}
      </div>
      <div className="flex min-h-0 flex-col gap-8 rounded-10 border border-border bg-raised p-12">
        <h2 className="m-0 text-14 font-semibold text-text">Backup preview</h2>
        {selectedBackup ? <><p className="m-0 break-all font-mono text-11 text-text-muted">{selectedBackup.original_path}</p>{backupPreview ? <div className="min-h-0 flex-1 overflow-auto"><DiffViewer diff={backupPreview.diff} displayPath={backupPreview.original_path ?? "Backup"} mode="side-by-side" /></div> : <p role="status" className="m-0 text-12 text-text-muted">Loading the saved version…</p>}{!selectedBackup.restorable && <p role="alert" className="m-0 text-12 text-danger">{selectedBackup.unavailable_reason}</p>}</> : <p className="m-0 text-12 text-text-muted">Select a backup to compare it with its original path.</p>}
      </div>
    </section>}

    <Modal
      open={confirmation !== null}
      onOpenChange={(open) => { if (!open && !busy) setConfirmation(null); }}
      title={confirmation?.kind === "backup" ? "Restore from backup" : confirmation?.action === "adopt" ? "Adopt on-disk changes?" : "Restore AgenTuna version?"}
      description="Review the diff before writing. AgenTuna will create a backup first."
      footer={<div className="flex justify-end gap-8"><Button variant="ghost" disabled={busy} onClick={() => setConfirmation(null)}>Cancel</Button><Button variant={confirmation?.kind === "drift" && confirmation.action === "adopt" ? "primary" : "destructive"} loading={busy} disabled={!confirmation || confirmation.kind === "backup" && (!confirmation.backup.restorable || !backupPreview || backupPreview.id !== confirmation.backup.id) || confirmation.kind === "drift" && (!preview || preview.event_id !== confirmation.row.event_id)} onClick={() => void (confirmation?.kind === "backup" ? restoreBackup() : applyDriftAction())}>{confirmation?.kind === "backup" ? "Restore backup" : confirmation?.action === "adopt" ? "Adopt on-disk" : "Restore AgenTuna version"}</Button></div>}
    >
      <div className="grid gap-10">
        <p className="m-0 text-13 text-text">{confirmation?.kind === "backup" ? confirmation.backup.original_path : confirmation?.row.path}</p>
        <p className="m-0 text-12 text-text-secondary">A backup of this path will be created before the change is applied.</p>
        {confirmation?.kind === "backup" && backupPreview && <DiffViewer diff={backupPreview.diff} displayPath={backupPreview.original_path} mode="side-by-side" />}
        {confirmation?.kind === "drift" && preview && (preview.previous_version_available
          ? <DiffViewer diff={preview.diff} displayPath={preview.path} mode="side-by-side" />
          : <section aria-label="On-disk content" className="grid gap-8">
            <p role="status" className="m-0 rounded-8 border border-warning/30 bg-warning/10 p-10 text-12 text-warning">Baseline content not retained: {preview.baseline_unavailable_reason ?? "No recovery reason was provided."}</p>
            {preview.on_disk_content !== null ? <pre aria-label="Current on-disk content" className="m-0 whitespace-pre-wrap rounded-8 border border-border bg-surface p-12 font-mono text-12 text-text">{preview.on_disk_content}</pre> : <p className="m-0 text-12 text-text-muted">No UTF-8 content is available for preview.</p>}
          </section>)}
      </div>
    </Modal>
  </main>;
}
