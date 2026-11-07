import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { JobFinishedEvent } from "../../generated/JobFinishedEvent";
import type { JobId } from "../../generated/JobId";
import type { JobProgressEvent } from "../../generated/JobProgressEvent";
import type { RemoteBackupCommitView } from "../../generated/RemoteBackupCommitView";
import type { RemoteBackupCreateRepoView } from "../../generated/RemoteBackupCreateRepoView";
import type { RemoteBackupConfirmationView } from "../../generated/RemoteBackupConfirmationView";
import type { RemoteBackupConflictGroupView } from "../../generated/RemoteBackupConflictGroupView";
import type { RemoteBackupConflictPreviewView } from "../../generated/RemoteBackupConflictPreviewView";
import type { RemoteBackupConflictApplyView } from "../../generated/RemoteBackupConflictApplyView";
import type { RemoteBackupFailure } from "../../generated/RemoteBackupFailure";
import type { RemoteBackupFailureCode } from "../../generated/RemoteBackupFailureCode";
import type { RemoteBackupRemedy } from "../../generated/RemoteBackupRemedy";
import type { RemoteBackupCreateRepoRequest } from "../../generated/RemoteBackupCreateRepoRequest";
import type { RemoteBackupSettingsView } from "../../generated/RemoteBackupSettingsView";
import type { RemoteBackupStatusView } from "../../generated/RemoteBackupStatusView";
import { Button } from "../../components/button";
import { useToast } from "../../overlays/toasts";
import { showRemoteBackupCompleteToast, showRemoteConflictToast } from "../../overlays/toasts/catalogue";

type RemoteJobCommand = "backup_now" | "sync_now" | "restore";
type RemoteJob = { id: JobId; name: string };
type ConflictResolution = "keep_local" | "keep_remote" | "keep_both";
type CreateRepoStatus = "creating" | "success" | "scope_refused" | "offline" | "auth_failed" | "error" | null;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFailureCode(value: unknown): value is RemoteBackupFailureCode {
  return value === "repository_not_configured" || value === "token_missing" || value === "token_rejected"
    || value === "repo_scope_missing" || value === "repo_name_invalid" || value === "offline"
    || value === "network_denied" || value === "remote_conflict" || value === "confirm_deletions"
    || value === "non_empty_target" || value === "stale_review" || value === "invalid_store"
    || value === "action_failed";
}

function isRemedy(value: unknown): value is RemoteBackupRemedy {
  return value === "configure_repository" || value === "save_token" || value === "replace_token"
    || value === "add_repo_scope" || value === "edit_repo_name" || value === "turn_network_on"
    || value === "review_network_policy" || value === "sync_first" || value === "confirm_deletions"
    || value === "choose_empty_store" || value === "review_again" || value === "retry";
}

function failureFrom(value: unknown): RemoteBackupFailure {
  let candidate = value;
  if (isRecord(candidate) && candidate.kind === "remote_backup") candidate = candidate.failure;
  if (isRecord(candidate) && "failure" in candidate && isRecord(candidate.failure)) candidate = candidate.failure;
  if (isRecord(candidate) && isFailureCode(candidate.code) && typeof candidate.message === "string" && isRemedy(candidate.remedy)) {
    return { code: candidate.code, message: candidate.message, remedy: candidate.remedy };
  }
  const message = isRecord(value) && typeof value.message === "string" ? value.message : typeof value === "string" ? value : "Remote backup action failed.";
  return { code: "action_failed", message, remedy: "retry" };
}

function safeFailure(value: unknown, secret = ""): RemoteBackupFailure {
  const failure = failureFrom(value);
  return secret.length === 0 ? failure : { ...failure, message: failure.message.replaceAll(secret, "[redacted]") };
}

function createRepoFailureState(failure: RemoteBackupFailure): Exclude<CreateRepoStatus, "creating" | "success" | null> {
  if (failure.code === "repo_scope_missing") return "scope_refused";
  if (failure.code === "offline") return "offline";
  if (failure.code === "token_rejected" || failure.code === "token_missing") return "auth_failed";
  return "error";
}

function repoNameProblem(value: string): string | null {
  return value.length === 0 || value.length > 100 || value.startsWith(".") || !/^[A-Za-z0-9._-]+$/.test(value)
    ? "Use 1-100 letters, digits, dots, hyphens or underscores; do not start with a dot."
    : null;
}

function remedyText(remedy: RemoteBackupRemedy): string {
  switch (remedy) {
    case "configure_repository": return "Choose a valid repository in Preferences, then retry.";
    case "save_token": return "Save a GitHub token in Preferences, then retry.";
    case "add_repo_scope": return "Create a GitHub token with the repo scope, save it in Preferences, then retry.";
    case "edit_repo_name": return "Edit the repository name and confirm creation again.";
    case "replace_token": return "Replace the saved GitHub token in Preferences, then retry.";
    case "turn_network_on": return "Turn off offline mode in Network settings, then retry explicitly.";
    case "review_network_policy": return "Review Network settings and allow the repository host before retrying.";
    case "sync_first": return "Synchronize the remote changes before starting another backup.";
    case "confirm_deletions": return "Review the deletion count, then confirm to apply the remote deletions.";
    case "choose_empty_store": return "Choose or clear an empty User store before restoring.";
    case "review_again": return "Reopen the conflict and review its current local and remote copies.";
    case "retry": return "Check the connection and retry this explicit action.";
  }
}

function formatBytes(value: number | bigint | null | undefined): string {
  if (value === null || value === undefined) return "size unknown";
  const bytes = typeof value === "bigint" ? Number(value) : value;
  return `${bytes} B`;
}

const DEVICE_NAME_MAX_CHARS = 64;

/** Mirrors `at_store::remote_backup::validate_device_name` so invalid names never reach git. */
function deviceNameProblem(value: string): string | null {
  if (value.length === 0) return "Enter a device name before saving.";
  if (value !== value.trim()) return "Remove leading or trailing spaces from the device name.";
  if (value.length > DEVICE_NAME_MAX_CHARS) return `Use at most ${DEVICE_NAME_MAX_CHARS} characters.`;
  if (value.includes("/") || value.includes("\\")) return "Device names cannot contain slashes.";
  if (/[\u0000-\u001f\u007f]/.test(value)) return "Device names cannot contain control characters.";
  return null;
}

function formatTime(value: string | null | undefined): string {
  if (!value) return "Never";
  const parsed = new Date(value);
  return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString();
}

function subscribe<T>(event: string, handler: (payload: T) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void listen<T>(event, message => handler(message.payload)).then(
    dispose => { if (disposed) dispose(); else unlisten = dispose; },
    () => undefined,
  );
  return () => { disposed = true; unlisten?.(); };
}

export function RemoteBackupSection({ onShowOfflineMode, reloadKey = 0 }: { onShowOfflineMode?: () => void; reloadKey?: number } = {}) {
  const toast = useToast();
  const [settings, setSettings] = useState<RemoteBackupSettingsView | null>(null);
  const [status, setStatus] = useState<RemoteBackupStatusView | null>(null);
  const [deviceName, setDeviceName] = useState("");
  const [deviceNameErrorText, setDeviceNameErrorText] = useState<string | null>(null);
  const [repoUrl, setRepoUrl] = useState("");
  const [token, setToken] = useState("");
  const [createRepoOpen, setCreateRepoOpen] = useState(false);
  const [createRepoName, setCreateRepoName] = useState("");
  const [createRepoNameError, setCreateRepoNameError] = useState<string | null>(null);
  const [createRepoStatus, setCreateRepoStatus] = useState<CreateRepoStatus>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [actionFailure, setActionFailure] = useState<RemoteBackupFailure | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const [job, setJob] = useState<RemoteJob | null>(null);
  const [startingJob, setStartingJob] = useState<string | null>(null);
  const [jobProgress, setJobProgress] = useState<JobProgressEvent | null>(null);
  const [conflicts, setConflicts] = useState<RemoteBackupConflictGroupView[]>([]);
  const [conflictsError, setConflictsError] = useState<string | null>(null);
  const [showConflicts, setShowConflicts] = useState(false);
  const [conflictLoading, setConflictLoading] = useState<string | null>(null);
  const [preview, setPreview] = useState<RemoteBackupConflictPreviewView | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [applyingPreview, setApplyingPreview] = useState(false);
  const [restoreOpen, setRestoreOpen] = useState(false);
  const [commits, setCommits] = useState<RemoteBackupCommitView[]>([]);
  const [selectedCommit, setSelectedCommit] = useState("");
  const [loadingCommits, setLoadingCommits] = useState(false);
  const [restoreConfirmation, setRestoreConfirmation] = useState<RemoteBackupConfirmationView | null>(null);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const [preparingRestore, setPreparingRestore] = useState(false);
  const [savingRestore, setSavingRestore] = useState(false);
  const [repoUrlFocus, setRepoUrlFocus] = useState(false);
  const repoUrlRef = useRef<HTMLInputElement>(null);
  const tokenRef = useRef<HTMLInputElement>(null);
  const privateRepoNameRef = useRef<HTMLInputElement>(null);
  const activeJob = useRef<JobId | null>(null);
  const activeJobName = useRef<string | null>(null);
  const queuedProgress = useRef(new Map<JobId, JobProgressEvent>());
  const queuedFinished = useRef(new Map<JobId, JobFinishedEvent>());
  const retryAction = useRef<(() => void) | null>(null);
  const [conflictsLoading, setConflictsLoading] = useState(false);

  const refreshConflicts = useCallback(async () => {
    setConflictsLoading(true);
    setConflictsError(null);
    try {
      setConflicts(await invoke<RemoteBackupConflictGroupView[]>("conflicts_list"));
    } catch (reason) {
      setConflictsError(String(reason));
    } finally {
      setConflictsLoading(false);
    }
  }, []);
  const presentFailure = useCallback((reason: unknown) => {
    const failure = safeFailure(reason);
    setActionFailure(failure);
    if (failure.code === "remote_conflict") {
      showRemoteConflictToast(toast, failure.message, () => {
        setShowConflicts(true);
        void refreshConflicts();
      });
    }
  }, [refreshConflicts, toast]);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    setSettingsError(null);
    setStatusError(null);
    const [settingsResult, statusResult, conflictsResult] = await Promise.allSettled([
      invoke<RemoteBackupSettingsView>("remote_backup_settings"),
      invoke<RemoteBackupStatusView>("status"),
      invoke<RemoteBackupConflictGroupView[]>("conflicts_list"),
    ]);
    if (settingsResult.status === "fulfilled") {
      setSettings(settingsResult.value);
      setDeviceName(settingsResult.value.device_name);
      setRepoUrl(settingsResult.value.repo_url ?? "");
    } else {
      setSettingsError(String(settingsResult.reason));
    }
    if (statusResult.status === "fulfilled") setStatus(statusResult.value);
    else setStatusError(String(statusResult.reason));
    if (conflictsResult.status === "fulfilled") {
      setConflicts(conflictsResult.value);
      setConflictsError(null);
    } else {
      setConflictsError(String(conflictsResult.reason));
    }
    setRefreshing(false);
  }, []);

  const completeJob = useCallback((event: JobFinishedEvent) => {
    if (activeJob.current !== event.id) return;
    const completedJobName = activeJobName.current;
    activeJob.current = null;
    activeJobName.current = null;
    setJob(null);
    setJobProgress(null);
    if (event.state === "failed" && event.error) {
      const failure = safeFailure(event.error);
      if (completedJobName === "Create private repository") setCreateRepoStatus(createRepoFailureState(failure));
      presentFailure(failure);
    } else {
      retryAction.current = null;
      if (event.state === "cancelled" || event.state === "interrupted") {
        setActionNotice(completedJobName === "Create private repository" ? "Private repository creation was cancelled." : "Remote backup action was cancelled.");
        if (completedJobName === "Create private repository") setCreateRepoStatus(null);
      } else {
        if (completedJobName === "Create private repository") {
          setCreateRepoStatus("success");
          setCreateRepoOpen(false);
          setActionNotice("Private repository created and connected.");
        } else {
          setActionNotice("Remote backup action completed.");
        }
        if (completedJobName === "Backup remote store") showRemoteBackupCompleteToast(toast);
      }
    }
    queuedProgress.current.delete(event.id);
    queuedFinished.current.delete(event.id);
    void refresh();
  }, [presentFailure, refresh, toast]);

  useEffect(() => {
    const disposeProgress = subscribe<JobProgressEvent>("agentuna:job-progress", event => {
      if (activeJob.current === event.id) setJobProgress(event);
      else {
        queuedProgress.current.set(event.id, event);
        if (queuedProgress.current.size > 32) {
          const oldest = queuedProgress.current.keys().next();
          if (!oldest.done) queuedProgress.current.delete(oldest.value);
        }
      }
    });
    const disposeFinished = subscribe<JobFinishedEvent>("agentuna:job-finished", event => {
      if (activeJob.current === event.id) completeJob(event);
      else {
        queuedFinished.current.set(event.id, event);
        if (queuedFinished.current.size > 32) {
          const oldest = queuedFinished.current.keys().next();
          if (!oldest.done) queuedFinished.current.delete(oldest.value);
        }
      }
    });
    return () => { disposeProgress(); disposeFinished(); };
  }, [completeJob]);

  useEffect(() => { void refresh(); }, [refresh]);
  // Refresh after the user flips the shared network policy so the offline banner is current.
  // Skipping the first run keeps the mount fetch above the only initial load.
  const initialReload = useRef(true);
  useEffect(() => {
    if (initialReload.current) { initialReload.current = false; return; }
    void refresh();
  }, [reloadKey, refresh]);

  const save = async () => {
    const problem = deviceNameProblem(deviceName);
    setDeviceNameErrorText(problem);
    if (problem !== null) return;
    setSettingsError(null);
    setSaving(true);
    const attemptedToken = token;
    try {
      const result = await invoke<RemoteBackupSettingsView>("remote_backup_configure", {
        deviceName, repoUrl, token: token.length === 0 ? null : token,
      });
      setSettings(result);
      setDeviceName(result.device_name);
      setRepoUrl(result.repo_url ?? "");
      setToken("");
      setActionNotice("Remote backup settings saved.");
      await refresh();
    } catch (reason) {
      setSettingsError(safeFailure(reason, attemptedToken).message);
    } finally {
      setSaving(false);
    }
  };

  const startCreateRepo = async () => {
    if (job || startingJob) return;
    const problem = repoNameProblem(createRepoName);
    setCreateRepoNameError(problem);
    if (problem !== null) return;
    setActionFailure(null);
    setActionNotice(null);
    setCreateRepoStatus("creating");
    setStartingJob("Create private repository");
    retryAction.current = null;
    try {
      const request: RemoteBackupCreateRepoRequest = { repo_name: createRepoName };
      const result = await invoke<RemoteBackupCreateRepoView>("remote_backup_create_repo", { request });
      const id = result.job_id;
      activeJob.current = id;
      activeJobName.current = "Create private repository";
      setJob({ id, name: "Create private repository" });
      setJobProgress(queuedProgress.current.get(id) ?? null);
      queuedProgress.current.delete(id);
      const finished = queuedFinished.current.get(id);
      if (finished) completeJob(finished);
    } catch (reason) {
      const failure = safeFailure(reason);
      setCreateRepoStatus(createRepoFailureState(failure));
      presentFailure(failure);
    } finally {
      setStartingJob(null);
    }
  };

  const startJob = async (command: RemoteJobCommand, args: Record<string, unknown> | undefined, name: string, retryable = true) => {
    if (job || startingJob) return;
    setActionFailure(null);
    setActionNotice(null);
    setStartingJob(name);
    if (retryable) retryAction.current = () => { void startJob(command, args, name, false); };
    try {
      const id = await invoke<JobId>(command, args);
      activeJob.current = id;
      activeJobName.current = name;
      setJob({ id, name });
      setJobProgress(queuedProgress.current.get(id) ?? null);
      queuedProgress.current.delete(id);
      const finished = queuedFinished.current.get(id);
      if (finished) completeJob(finished);
    } catch (reason) {
      presentFailure(reason);
      setStartingJob(null);
      return;
    }
    setStartingJob(null);
  };

  const loadCommits = async () => {
    setRestoreOpen(true);
    setRestoreError(null);
    setLoadingCommits(true);
    setCommits([]);
    setSelectedCommit("");
    setRestoreConfirmation(null);
    setActionFailure(null);
    retryAction.current = () => { void loadCommits(); };
    try {
      const rows = await invoke<RemoteBackupCommitView[]>("restore_commits");
      setCommits(rows);
      setSelectedCommit(rows[0]?.commit ?? "");
      if (rows.length === 0) setRestoreError("No commits are available in the configured remote.");
    } catch (reason) {
      presentFailure(reason);
    } finally {
      setLoadingCommits(false);
    }
  };

  const prepareRestore = async () => {
    if (!selectedCommit) return;
    setActionFailure(null);
    setRestoreError(null);
    setPreparingRestore(true);
    retryAction.current = () => { void prepareRestore(); };
    try {
      setRestoreConfirmation(await invoke<RemoteBackupConfirmationView>("restore_confirmation", { commit: selectedCommit }));
    } catch (reason) {
      presentFailure(reason);
    } finally {
      setPreparingRestore(false);
    }
  };

  const confirmRestore = async () => {
    if (!restoreConfirmation) return;
    setSavingRestore(true);
    await startJob("restore", { commit: restoreConfirmation.commit, confirmationToken: restoreConfirmation.confirmation_token }, "Restore remote backup", false);
    setRestoreConfirmation(null);
    setSavingRestore(false);
    setRestoreOpen(false);
  };

  const revealConflict = async (groupId: string, relativePath: string, side: "local" | "remote") => {
    const key = `${groupId}:${relativePath}:${side}`;
    setConflictLoading(key);
    setActionFailure(null);
    retryAction.current = () => { void revealConflict(groupId, relativePath, side); };
    try {
      await invoke("conflicts_reveal", { groupId, relativePath, side });
    } catch (reason) {
      presentFailure(reason);
    } finally {
      setConflictLoading(null);
    }
  };

  const reviewConflict = async (groupId: string, relativePath: string, resolution: ConflictResolution) => {
    setPreview(null);
    setPreviewLoading(true);
    setActionFailure(null);
    retryAction.current = () => { void reviewConflict(groupId, relativePath, resolution); };
    try {
      setPreview(await invoke<RemoteBackupConflictPreviewView>("conflicts_resolve_preview", { groupId, relativePath, resolution }));
    } catch (reason) {
      presentFailure(reason);
    } finally {
      setPreviewLoading(false);
    }
  };

  const applyResolution = async () => {
    if (!preview) return;
    setApplyingPreview(true);
    setActionFailure(null);
    retryAction.current = () => { void applyResolution(); };
    try {
      const result = await invoke<RemoteBackupConflictApplyView>("conflicts_resolve_apply", { previewId: preview.preview_id });
      setActionNotice(`Conflict resolution applied (${result.files_changed} file changes).`);
      setPreview(null);
      await refresh();
    } catch (reason) {
      presentFailure(reason);
    } finally {
      setApplyingPreview(false);
    }
  };

  const busy = job !== null || startingJob !== null;
  const actionButton = (failure: RemoteBackupFailure) => {
    if (failure.remedy === "configure_repository") return <Button size="sm" onClick={() => { repoUrlRef.current?.focus(); setRepoUrlFocus(true); }}>Focus repository field</Button>;
    if (failure.remedy === "save_token" || failure.remedy === "replace_token" || failure.remedy === "add_repo_scope") return <Button size="sm" onClick={() => tokenRef.current?.focus()}>Focus token field</Button>;
    if (failure.remedy === "edit_repo_name") return <Button size="sm" onClick={() => privateRepoNameRef.current?.focus()}>Edit repository name</Button>;
    if (failure.remedy === "turn_network_on" && onShowOfflineMode) return <Button size="sm" onClick={onShowOfflineMode}>Show offline mode</Button>;
    if (failure.remedy === "confirm_deletions") return <Button size="sm" loading={busy} onClick={() => void startJob("sync_now", { confirmDeletions: true }, "Sync remote backup", false)}>Confirm remote deletions</Button>;
    if (failure.remedy === "sync_first") return <Button size="sm" loading={busy} onClick={() => void startJob("sync_now", { confirmDeletions: false }, "Sync remote backup")}>Sync first</Button>;
    if (failure.remedy === "review_again") return <Button size="sm" onClick={() => { setShowConflicts(true); void refreshConflicts(); }}>Review conflicts</Button>;
    if (failure.remedy === "retry" && retryAction.current) return <Button size="sm" loading={busy} onClick={() => retryAction.current?.()}>Retry action</Button>;
    return null;
  };

  return <section className="rounded-10 border border-border bg-surface p-16" aria-label="Remote backup">
    <div className="flex flex-wrap items-start justify-between gap-10">
      <div><h2 className="m-0 text-15 font-semibold text-text">Remote backup</h2><p className="mt-6 text-12 text-muted">Credentials are stored in the system credential store. The token is never shown after saving.</p></div>
      <Button size="sm" variant="secondary" loading={refreshing} onClick={() => void refresh()}>Refresh status</Button>
    </div>
    {settingsError && <div role="alert" className="my-8 rounded-6 border border-danger/40 bg-danger/10 p-10 text-13 text-danger">{settingsError}<Button size="sm" className="ml-8" onClick={() => void refresh()}>Retry</Button></div>}
    {statusError && <div role="alert" className="my-8 rounded-6 border border-danger/40 bg-danger/10 p-10 text-13 text-danger">Status unavailable: {statusError}<Button size="sm" className="ml-8" onClick={() => void refresh()}>Retry</Button></div>}
    {status?.offline && <div role="status" className="my-10 rounded-6 border border-warning/40 bg-warning/10 p-10 text-13 text-text"><strong>Offline mode is on.</strong> Remote backup actions are paused until Network settings allow requests.</div>}
    {!settings && !settingsError && <p role="status" className="text-12 text-muted">Loading remote backup settings…</p>}
    {settings && <div className="mt-12 grid gap-12">
      <label className="grid gap-4 text-13 text-text">Device name<input aria-label="Remote backup device name" aria-invalid={deviceNameErrorText !== null} aria-describedby={deviceNameErrorText ? "remote-backup-device-error" : undefined} value={deviceName} onChange={event => { setDeviceName(event.target.value); setDeviceNameErrorText(null); }} className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      <p className="m-0 text-11 text-muted">Recorded in backup commit messages so other machines can tell copies apart.</p>
      {deviceNameErrorText && <p id="remote-backup-device-error" role="alert" className="m-0 text-12 text-danger">{deviceNameErrorText}</p>}
      <label className="grid gap-4 text-13 text-text">GitHub repository URL<input ref={repoUrlRef} aria-label="Remote backup repository URL" value={repoUrl} onChange={event => { setRepoUrl(event.target.value); setRepoUrlFocus(false); }} placeholder="https://github.com/owner/repository.git" className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      {repoUrlFocus && <p className="m-0 text-12 text-muted">Repository URLs must not contain credentials, query parameters or fragments.</p>}
      <label className="grid gap-4 text-13 text-text">GitHub token{settings.token_saved && <span className="text-12 text-success">Connected token saved</span>}<input ref={tokenRef} aria-label="GitHub token" type="password" autoComplete="new-password" value={token} onChange={event => setToken(event.target.value)} placeholder={settings.token_saved ? "Token saved; enter to replace" : "Paste a GitHub token"} className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      <div className="flex flex-wrap gap-8">
        <Button loading={saving} onClick={() => void save()}>Save remote backup settings</Button>
        <Button variant="secondary" disabled={busy} onClick={() => { setCreateRepoOpen(true); setCreateRepoStatus(null); setCreateRepoNameError(null); setActionFailure(null); setActionNotice(null); }}>Create private repo</Button>
      </div>
      {createRepoOpen && <section className="grid gap-8 rounded-6 border border-border bg-raised p-12" aria-label="Private repository creation">
        <div><h3 className="m-0 text-13 font-semibold text-text">Create a private GitHub repository</h3><p className="mt-4 text-12 text-secondary">AgenTuna checks the saved token’s repo scope first. The new repository becomes the remote backup destination only after creation succeeds.</p></div>
        <label className="grid gap-4 text-12 text-text">GitHub repository name<input ref={privateRepoNameRef} aria-label="Private repository name" aria-invalid={createRepoNameError !== null} value={createRepoName} onChange={event => { setCreateRepoName(event.target.value); setCreateRepoNameError(null); }} className="rounded-6 border border-border bg-surface px-10 py-8 text-text" /></label>
        {createRepoNameError && <p role="alert" className="m-0 text-12 text-danger">{createRepoNameError}</p>}
        {createRepoStatus === "creating" && <div role="status" aria-live="polite" className="flex flex-wrap items-center justify-between gap-8 rounded-6 border border-border p-10 text-12 text-secondary"><span>{startingJob ? "Starting private repository creation…" : "Creating private repository…"}</span>{job?.name === "Create private repository" && <Button size="sm" variant="destructive" onClick={() => { void invoke("jobs_cancel", { jobId: job.id }).catch(presentFailure); }}>Cancel creation</Button>}</div>}
        {createRepoStatus === "scope_refused" && <p role="status" className="m-0 text-12 text-warning">Scope refused: the saved token needs GitHub’s repo scope.</p>}
        {createRepoStatus === "offline" && <p role="status" className="m-0 text-12 text-warning"><span>Offline: turn off offline mode before contacting GitHub.</span>{onShowOfflineMode && <Button size="sm" variant="secondary" className="ml-8" onClick={onShowOfflineMode}>Open Network settings</Button>}</p>}
        {createRepoStatus === "auth_failed" && <p role="status" className="m-0 text-12 text-danger">Authentication failed: replace or save a valid GitHub token.</p>}
        {createRepoStatus === "error" && <p role="status" className="m-0 text-12 text-danger">Repository creation failed. Review the alert and retry.</p>}
        {createRepoStatus !== "creating" && <div className="flex flex-wrap gap-8"><Button disabled={busy} onClick={() => void startCreateRepo()}>Create repository</Button><Button variant="secondary" onClick={() => { setCreateRepoOpen(false); setCreateRepoStatus(null); }}>Cancel</Button></div>}
      </section>}
      <div role="group" className="rounded-6 border border-border bg-raised p-10 text-12 text-secondary" aria-label="Remote backup status">
        <div><strong>Status:</strong> {status?.status ?? settings.status}{(status?.error_kind ?? settings.error_kind) ? ` (${status?.error_kind ?? settings.error_kind})` : ""}</div>
        <div><strong>Last backup:</strong> {formatTime(status?.last_push_at ?? settings.last_push_at)}</div>
        <div><strong>Last sync:</strong> {formatTime(status?.last_sync_at ?? settings.last_pull_at)}</div>
        <div><strong>Last commit:</strong> {status?.last_commit ?? settings.last_commit ?? "None"}</div>
        <div><strong>Unresolved conflicts:</strong> {status?.conflict_count ?? conflicts.reduce((total, group) => total + group.files.length, 0)}</div>
      </div>
      <div role="group" className="flex flex-wrap gap-8" aria-label="Remote backup actions">
        <Button loading={startingJob === "Backup remote store"} disabled={busy} onClick={() => void startJob("backup_now", undefined, "Backup remote store")}>Backup now</Button>
        <Button variant="secondary" loading={startingJob === "Sync remote backup"} disabled={busy} onClick={() => void startJob("sync_now", { confirmDeletions: false }, "Sync remote backup")}>Sync</Button>
        <Button variant="secondary" disabled={busy || loadingCommits} loading={loadingCommits} onClick={() => void loadCommits()}>Restore</Button>
        <Button variant="secondary" disabled={busy} onClick={() => { const next = !showConflicts; setShowConflicts(next); if (next) void refreshConflicts(); }}>Resolve conflicts{conflicts.length ? ` (${conflicts.reduce((total, group) => total + group.files.length, 0)})` : ""}</Button>
        {job && job.name !== "Create private repository" && <Button variant="destructive" onClick={() => { void invoke("jobs_cancel", { jobId: job.id }).catch(presentFailure); }}>Cancel {job.name.toLowerCase()}</Button>}
      </div>
      {busy && <div role="status" aria-live="polite" className="grid gap-6 rounded-6 border border-border p-10 text-12 text-secondary"><span>{startingJob ? `${startingJob} starting…` : jobProgress?.label ?? `${job?.name ?? "Remote backup"} running…`}</span>{jobProgress?.fraction !== null && jobProgress?.fraction !== undefined && <progress aria-label="Remote backup progress" max={1} value={jobProgress.fraction} className="w-full" />}</div>}
      {actionNotice && <p role="status" className="m-0 text-12 text-success">{actionNotice}</p>}
      {actionFailure && <div role="alert" className="grid gap-6 rounded-6 border border-danger/40 bg-danger/10 p-10 text-13 text-danger"><strong>{actionFailure.message}</strong><span>{remedyText(actionFailure.remedy)}</span>{actionButton(actionFailure)}</div>}
      {restoreError && <p role="alert" className="m-0 text-12 text-danger">{restoreError}</p>}
      {restoreOpen && <section className="grid gap-8 rounded-6 border border-border bg-raised p-12" aria-label="Restore from remote"><div className="flex items-center justify-between gap-8"><h3 className="m-0 text-13 font-semibold text-text">Restore from remote</h3><Button size="sm" variant="ghost" onClick={() => { setRestoreOpen(false); setRestoreConfirmation(null); }}>Close</Button></div><p className="m-0 text-12 text-secondary">Restore requires an empty User store and a separate confirmation for the selected commit.</p>{commits.length > 0 && <><label className="grid gap-4 text-12 text-text">Remote commit<select aria-label="Restore commit" value={selectedCommit} onChange={event => setSelectedCommit(event.target.value)} className="rounded-6 border border-border bg-surface px-8 py-6">{commits.map(commit => <option key={commit.commit} value={commit.commit}>{commit.message} · {formatTime(commit.committed_at)} · {commit.commit.slice(0, 8)}</option>)}</select></label><Button disabled={!selectedCommit || preparingRestore} loading={preparingRestore} onClick={() => void prepareRestore()}>Review restore</Button></>}{restoreConfirmation && <div role="dialog" aria-labelledby="restore-confirm-title" className="grid gap-8 rounded-6 border border-warning/40 bg-warning/10 p-10"><h4 id="restore-confirm-title" className="m-0 text-13 font-semibold text-text">Confirm restore</h4><p className="m-0 text-12 text-secondary">Restore commit {restoreConfirmation.commit.slice(0, 12)} from {restoreConfirmation.repo_url} to {restoreConfirmation.device_name}? The target must still be empty.</p><div className="flex gap-8"><Button variant="destructive" loading={savingRestore} onClick={() => void confirmRestore()}>Confirm restore</Button><Button variant="secondary" onClick={() => setRestoreConfirmation(null)}>Cancel restore</Button></div></div>}</section>}
      {showConflicts && <div className="grid gap-8 rounded-6 border border-border bg-raised p-12"><div className="flex items-center justify-between gap-8"><h3 className="m-0 text-13 font-semibold text-text">Unresolved remote conflicts</h3><Button size="sm" variant="ghost" onClick={() => { setShowConflicts(false); setPreview(null); }}>Close</Button></div>{conflictsLoading && <p role="status" className="m-0 text-12 text-secondary">Loading remote conflicts…</p>}{conflictsError && <p role="alert" className="m-0 text-12 text-danger">{conflictsError}<Button size="sm" className="ml-8" onClick={() => void refreshConflicts()}>Retry</Button></p>}{conflicts.length === 0 && !conflictsError && <p className="m-0 text-12 text-secondary">No unresolved remote conflicts.</p>}{conflicts.map(group => <article key={group.id} className="grid gap-8 border-t border-border pt-8"><h4 className="m-0 text-12 font-semibold text-text">{group.device_name} · {group.timestamp}</h4>{group.files.map(file => <div key={`${group.id}:${file.path}`} className="grid gap-6 rounded-6 border border-border p-8"><code className="break-all text-11 text-secondary">{file.path}</code><dl className="m-0 grid gap-2 text-11 text-secondary"><div className="flex flex-wrap gap-4"><dt className="font-semibold text-text">Local copy</dt><dd className="m-0 break-all">{file.local_exists ? `${file.local_path} · ${formatBytes(file.local_size)}` : "deleted locally"}</dd></div><div className="flex flex-wrap gap-4"><dt className="font-semibold text-text">Remote copy</dt><dd className="m-0 break-all">{file.remote_path} · {formatBytes(file.remote_size)}</dd></div></dl><div className="flex flex-wrap gap-6"><Button size="sm" variant="secondary" disabled={!file.local_exists || conflictLoading === `${group.id}:${file.path}:local`} onClick={() => void revealConflict(group.id, file.path, "local")}>Open local</Button><Button size="sm" variant="secondary" disabled={conflictLoading === `${group.id}:${file.path}:remote`} onClick={() => void revealConflict(group.id, file.path, "remote")}>Open remote</Button><Button size="sm" onClick={() => void reviewConflict(group.id, file.path, "keep_local")}>Keep local</Button><Button size="sm" onClick={() => void reviewConflict(group.id, file.path, "keep_remote")}>Keep remote</Button><Button size="sm" onClick={() => void reviewConflict(group.id, file.path, "keep_both")}>Keep both</Button></div></div>)}</article>)}{previewLoading && <p role="status" className="m-0 text-12 text-muted">Building reviewed change preview…</p>}{preview && <div className="grid gap-8 rounded-6 border border-accent/40 bg-surface p-10"><h4 className="m-0 text-13 font-semibold text-text">Review before applying</h4><p className="m-0 text-12 text-secondary">{preview.summary}</p>{preview.archived_remote_path && <p className="m-0 break-all text-12 text-secondary">Remote copy archived at <code>{preview.archived_remote_path}</code></p>}<ul className="m-0 grid list-none gap-8 p-0">{preview.files.map(file => <li key={`${file.path}:${file.action}`} className="grid gap-4"><code className="break-all text-11 text-secondary">{file.action}: {file.path}</code><pre className="m-0 max-h-180 overflow-auto whitespace-pre-wrap break-words rounded-6 bg-raised p-8 text-11 text-secondary">{file.diff}</pre></li>)}</ul><Button loading={applyingPreview} onClick={() => void applyResolution()}>Apply reviewed resolution</Button></div>}</div>}
    </div>}
  </section>;
}
