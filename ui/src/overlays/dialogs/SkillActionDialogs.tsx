import { useEffect, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import type { SkillDeleteApplyRequest } from "../../generated/SkillDeleteApplyRequest";
import type { SkillDeletePreviewRequest } from "../../generated/SkillDeletePreviewRequest";
import type { SkillDeletePreviewView } from "../../generated/SkillDeletePreviewView";
import type { SkillDeleteResultView } from "../../generated/SkillDeleteResultView";
import type { SkillDisableInRepoApplyRequest } from "../../generated/SkillDisableInRepoApplyRequest";
import type { SkillDisableInRepoApplyView } from "../../generated/SkillDisableInRepoApplyView";
import type { SkillDisableInRepoPreviewRequest } from "../../generated/SkillDisableInRepoPreviewRequest";
import type { SkillDisableInRepoPreviewView } from "../../generated/SkillDisableInRepoPreviewView";
import type { SkillTransferApplyRequest } from "../../generated/SkillTransferApplyRequest";
import type { SkillTransferApplyView } from "../../generated/SkillTransferApplyView";
import type { SkillTransferOptionsRequest } from "../../generated/SkillTransferOptionsRequest";
import type { SkillTransferOptionsView } from "../../generated/SkillTransferOptionsView";
import type { SkillTransferPreviewRequest } from "../../generated/SkillTransferPreviewRequest";
import type { SkillTransferPreviewView } from "../../generated/SkillTransferPreviewView";
import type { SkillTransferBulkApplyRequest } from "../../generated/SkillTransferBulkApplyRequest";
import type { SkillTransferBulkApplyView } from "../../generated/SkillTransferBulkApplyView";
import { commandErrorMessage } from "../../lib/commandError";
import type { SkillTransferBulkPreviewRequest } from "../../generated/SkillTransferBulkPreviewRequest";
import type { SkillTransferBulkPreviewView } from "../../generated/SkillTransferBulkPreviewView";
import { Modal } from "./Modal";

export type SkillActionDialogRequest =
  | { readonly kind: "transfer"; readonly itemId: string; readonly transfer: "promote" | "move" | "duplicate"; readonly targetScope?: string }
  | { readonly kind: "delete"; readonly itemId: string }
  | { readonly kind: "disable-in-repo"; readonly itemId: string };

export interface SkillActionDialogsProps {
  readonly action: SkillActionDialogRequest | null;
  readonly projectId: string | null;
  readonly onClose: () => void;
  readonly onTransferSuccess: (result: SkillTransferApplyView) => void;
  readonly onDeleteSuccess: (result: SkillDeleteResultView) => void;
  readonly onDisableSuccess: (result: SkillDisableInRepoApplyView) => void;
}

/** Routes one explicit skill action to its typed preview/apply dialog. */
export function SkillActionDialogs({
  action,
  projectId,
  onClose,
  onTransferSuccess,
  onDeleteSuccess,
  onDisableSuccess,
}: SkillActionDialogsProps): JSX.Element | null {
  if (action === null) return null;
  if (action.kind === "transfer") {
    return (
      <SkillTransferDialog
        itemId={action.itemId}
        transfer={action.transfer}
        projectId={projectId}
        initialTargetScope={action.targetScope}
        onClose={onClose}
        onSuccess={onTransferSuccess}
      />
    );
  }
  if (action.kind === "delete") {
    return (
      <SkillDeleteDialog
        itemId={action.itemId}
        projectId={projectId}
        onClose={onClose}
        onSuccess={onDeleteSuccess}
      />
    );
  }
  return (
    <SkillDisableInRepoDialog
      itemId={action.itemId}
      projectId={projectId}
      onClose={onClose}
      onSuccess={onDisableSuccess}
    />
  );
}

/** Properties for a single atomic Promote or Move across selected skills. */
export interface BulkSkillTransferDialogProps {
  /** Selected managed skill ids. */
  readonly itemIds: readonly string[];
  /** All selected display names, including locally blocked native rows. */
  readonly names: readonly string[];
  /** Items the UI can identify as unmanageable before requesting a backend preview. */
  readonly blockedItems: readonly {
    readonly item_id: string;
    readonly name: string;
    readonly reason: string;
  }[];
  /** Requested scope transfer. */
  readonly transfer: "promote" | "move";
  /** Project context for Project and Local scope resolution. */
  readonly projectId: string | null;
  /** Dismiss the dialog without applying its preview. */
  readonly onClose: () => void;
  /** Called after the one atomic transfer ChangeSet succeeds. */
  readonly onSuccess: (result: SkillTransferBulkApplyView) => void;
}

/** Selects one destination scope and applies a reviewed all-or-nothing skill transfer. */
export function BulkSkillTransferDialog({
  itemIds,
  names,
  blockedItems,
  transfer,
  projectId,
  onClose,
  onSuccess,
}: BulkSkillTransferDialogProps): JSX.Element {
  const [options, setOptions] = useState<SkillTransferOptionsView | null>(null);
  const [targetScope, setTargetScope] = useState("");
  const [preview, setPreview] = useState<SkillTransferBulkPreviewView | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const firstItem = itemIds[0];
    if (firstItem === undefined) {
      if (blockedItems.length === 0) setError("Select at least one managed skill.");
      setLoading(false);
      return () => { active = false; };
    }
    const request: SkillTransferOptionsRequest = {
      item_id: firstItem,
      transfer,
      project_id: projectId,
    };
    setLoading(true);
    setError(null);
    void invoke<SkillTransferOptionsView>("skill_transfer_options", { request })
      .then((view) => {
        if (!active) return;
        setOptions(view);
        setTargetScope(view.targets.find((target) => target.available)?.scope ?? "");
      })
      .catch((cause: unknown) => { if (active) setError(errorMessage(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [blockedItems.length, itemIds, projectId, transfer]);

  async function reviewBatch(): Promise<void> {
    if (targetScope === "" || itemIds.length === 0 || blockedItems.length > 0) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillTransferBulkPreviewRequest = {
        item_ids: [...itemIds],
        target_scope: targetScope,
        transfer,
        project_id: projectId,
      };
      setPreview(await invoke<SkillTransferBulkPreviewView>("skill_transfer_bulk_preview", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  async function applyBatch(): Promise<void> {
    if (preview?.preview_id == null || preview.items.some((item) => item.reason !== null)) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillTransferBulkApplyRequest = { preview_id: preview.preview_id };
      onSuccess(await invoke<SkillTransferBulkApplyView>("skill_transfer_bulk_apply", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  const blocked = [
    ...blockedItems,
    ...(preview?.items.filter((item) => item.reason !== null) ?? []),
  ];
  const ready = preview?.preview_id !== null && preview !== null && blocked.length === 0;
  return (
    <Modal
      open
      title={transfer === "promote" ? "Promote selected skills" : "Move selected skills to scope"}
      description={`Choose one destination for all ${names.length} selected skills. The operation is atomic.`}
      onOpenChange={(open) => { if (!open && !busy) onClose(); }}
      size="lg"
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          <Button variant="secondary" disabled={busy || loading || targetScope === "" || blockedItems.length > 0} onClick={() => void reviewBatch()}>
            {busy ? "Reviewing…" : "Review batch"}
          </Button>
          <Button variant="primary" disabled={busy || !ready} onClick={() => void applyBatch()}>
            {busy ? "Applying…" : `Apply ${names.length} skills`}
          </Button>
        </>
      }
    >
      {loading ? <p role="status" className="m-0 text-12 text-text-muted">Loading destination scopes…</p> : null}
      {error ? <p role="alert" className="m-0 rounded-6 bg-danger/10 p-8 text-12 text-danger">{error}</p> : null}
      <div className="grid gap-8" role="radiogroup" aria-label="Destination scope">
        {options?.targets.map((target) => (
          <button
            key={target.scope}
            type="button"
            role="radio"
            aria-checked={targetScope === target.scope}
            disabled={!target.available || busy}
            onClick={() => { setTargetScope(target.scope); setPreview(null); }}
            className={`grid gap-4 rounded-6 border p-8 text-left ${targetScope === target.scope ? "border-accent bg-accent/10" : "border-border bg-surface"}`}
          >
            <strong className="text-12 text-text">{target.scope}</strong>
            <span className="break-all font-mono text-11 text-text-muted">{target.path}</span>
            {!target.available && target.reason ? <span className="text-11 text-warning">{target.reason}</span> : null}
          </button>
        ))}
      </div>
      <p className="mb-0 mt-12 text-12 text-text-secondary">
        Selected: {names.slice(0, 5).join(", ")}{names.length > 5 ? `, and ${names.length - 5} more` : ""}
      </p>
      {(preview !== null || blockedItems.length > 0) && (
        <section aria-label="Bulk transfer preflight" className="mt-12 rounded-6 border border-border bg-surface p-8">
          {blocked.length === 0 && preview !== null ? (
            <>
              <p className="m-0 text-12 text-text">All {preview.items.length} skills passed preflight.</p>
              <p className="mb-0 mt-4 break-all text-11 text-text-muted">Backups: {preview.backup_location}</p>
              {transfer === "move" ? (
                <p className="mb-0 mt-8 text-12 text-warning">Each source folder is removed only after its backup is created.</p>
              ) : null}
            </>
          ) : (
            <>
              <p className="m-0 text-12 font-semibold text-danger">No files were changed. Resolve the blocked skills before applying.</p>
              <ul className="m-0 mt-8 max-h-[180px] overflow-auto p-0 text-11">
                {blocked.map((item) => (
                  <li key={item.item_id} className="mb-4 rounded-4 bg-danger/5 p-6">
                    <strong>{item.name}</strong>: {item.reason}
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      )}
    </Modal>
  );
}

function SkillTransferDialog({
  itemId,
  transfer,
  projectId,
  initialTargetScope,
  onClose,
  onSuccess,
}: {
  readonly itemId: string;
  readonly transfer: "promote" | "move" | "duplicate";
  readonly projectId: string | null;
  /** Destination preselected when the caller named one, for example Project. */
  readonly initialTargetScope?: string;
  readonly onClose: () => void;
  readonly onSuccess: (result: SkillTransferApplyView) => void;
}): JSX.Element {
  const [options, setOptions] = useState<SkillTransferOptionsView | null>(null);
  const [targetScope, setTargetScope] = useState("");
  const [preview, setPreview] = useState<SkillTransferPreviewView | null>(null);
  const [renameSlug, setRenameSlug] = useState("");
  const [duplicateSlug, setDuplicateSlug] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const request: SkillTransferOptionsRequest = {
      item_id: itemId,
      transfer,
      project_id: projectId,
    };
    setLoading(true);
    setError(null);
    void invoke<SkillTransferOptionsView>("skill_transfer_options", { request })
      .then((view) => {
        if (!active) return;
        setOptions(view);
        const preferred = initialTargetScope !== undefined && view.targets.some((target) => target.scope === initialTargetScope && target.available)
          ? initialTargetScope
          : transfer === "duplicate"
            ? view.targets.find((target) => target.scope === view.source_scope && target.available)?.scope
            : view.targets.find((target) => target.available)?.scope;
        setTargetScope(preferred ?? "");
      })
      .catch((cause: unknown) => {
        if (active) setError(errorMessage(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [initialTargetScope, itemId, transfer, projectId]);

  async function previewTransfer(resolution: string | null = null): Promise<void> {
    if (targetScope === "" || (transfer === "duplicate" && duplicateSlug.trim() === "")) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillTransferPreviewRequest = {
        item_id: itemId,
        target_scope: targetScope,
        transfer,
        resolution: transfer === "duplicate" ? null : resolution,
        new_slug: transfer === "duplicate" ? duplicateSlug.trim() : resolution === "rename" ? renameSlug.trim() : null,
        project_id: projectId,
      };
      const result = await invoke<SkillTransferPreviewView>("skill_transfer_preview", { request });
      setPreview(result);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  async function applyTransfer(): Promise<void> {
    const previewId = preview?.preview_id;
    if (previewId === null || previewId === undefined) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillTransferApplyRequest = { preview_id: previewId };
      const result = await invoke<SkillTransferApplyView>("skill_transfer_apply", { request });
      onSuccess(result);
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  const title = transfer === "promote" ? "Promote skill" : transfer === "move" ? "Move skill to scope" : "Duplicate skill";
  const collision = preview?.overwrite ?? null;
  const canRename = renameSlug.trim() !== "";
  const canDuplicate = duplicateSlug.trim() !== "";
  const targetOptions = options?.targets.filter((target) =>
    transfer === "duplicate"
      ? target.scope === options.source_scope
      : target.scope !== options.source_scope,
  ) ?? [];
  return (
    <Modal
      open
      title={title}
      description={`${options?.name ?? "Skill"} · ${options?.source_scope ?? "source scope"} → destination scope`}
      onOpenChange={(open) => { if (!open && !busy) onClose(); }}
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          {preview?.preview_id ? (
            <Button variant="primary" disabled={busy} onClick={() => void applyTransfer()}>
              {busy ? "Applying…" : transfer === "promote" ? "Promote skill" : transfer === "move" ? "Move skill" : "Duplicate skill"}
            </Button>
          ) : collision && transfer !== "duplicate" ? (
            <>
              <Button variant="secondary" disabled={busy} onClick={() => void previewTransfer("replace")}>
                Replace with backup
              </Button>
              <Button variant="primary" disabled={busy || !canRename} onClick={() => void previewTransfer("rename")}>
                Rename destination
              </Button>
            </>
          ) : (
            <Button
              variant="primary"
              disabled={busy || loading || targetScope === "" || (transfer === "duplicate" && !canDuplicate)}
              onClick={() => void previewTransfer()}
            >
              {busy ? "Preparing review…" : transfer === "duplicate" ? "Review duplicate" : "Review transfer"}
            </Button>
          )}
        </>
      }
    >
      {loading ? <p role="status" className="m-0 text-12 text-text-muted">Loading available scopes…</p> : null}
      {error ? <p role="alert" className="m-0 rounded-6 bg-danger/10 p-8 text-12 text-danger">{error}</p> : null}
      {options && !preview?.preview_id && !collision ? (
        <label className="grid gap-4 text-12 text-text">
          Destination scope
          <select
            aria-label="Destination scope"
            value={targetScope}
            onChange={(event) => { setTargetScope(event.target.value); setPreview(null); }}
            className="rounded-6 border border-border bg-surface px-8 py-6 text-text"
          >
            {targetOptions.map((target) => (
              <option key={target.scope} value={target.scope} disabled={!target.available}>
                {target.scope} — {target.path || target.reason || "unavailable"}
              </option>
            ))}
          </select>
        </label>
      ) : null}
      {transfer === "duplicate" && (
        <label className="mt-8 grid gap-4 text-12 text-text">
          New lowercase skill slug
          <input
            aria-label="New skill slug"
            value={duplicateSlug}
            onChange={(event) => { setDuplicateSlug(event.target.value); setPreview(null); }}
            className="rounded-6 border border-border bg-surface px-8 py-6 font-mono text-12 text-text"
          />
        </label>
      )}
      {options && (
        <div className="mt-12 rounded-6 border border-border bg-surface p-8 text-11">
          <p className="m-0 break-all"><strong>Source:</strong> {options.source_path}</p>
          {preview && <p className="mb-0 mt-4 break-all"><strong>Destination:</strong> {preview.target_path}</p>}
        </div>
      )}
      {collision && (
        <section aria-label="Destination overwrite warning" className="mt-12 rounded-6 border border-warning/40 bg-warning/5 p-12">
          <h2 className="m-0 text-13 font-semibold text-text">
            {transfer === "duplicate" ? "That skill slug is already in use" : "A managed skill already exists there"}
          </h2>
          {transfer === "duplicate" ? (
            <p className="mb-8 mt-4 text-12 text-text-secondary">Choose a different lowercase slug for the duplicate.</p>
          ) : (
            <p className="mb-8 mt-4 break-all text-12 text-text-secondary">Replace backs up the current folder before writing. Backup location: {preview?.backup_location}</p>
          )}
          <div className="grid gap-4 rounded-6 bg-surface p-8 text-11">
            <p className="m-0 break-all"><strong>Source SHA-256:</strong> {collision.source_hash}</p>
            <p className="m-0 break-all"><strong>Target SHA-256:</strong> {collision.target_hash}</p>
          </div>
          <p className="mb-4 mt-8 text-12 text-text-secondary">File comparison ({collision.files.length} changed):</p>
          <ul className="m-0 max-h-[180px] overflow-auto rounded-6 border border-border bg-surface p-8 text-11">
            {collision.files.length === 0 ? <li>No package file differences.</li> : collision.files.map((file) => (
              <li key={file.path} className="break-all"><span className="font-semibold capitalize">{file.change}</span> · {file.path}</li>
            ))}
          </ul>
          {transfer !== "duplicate" && (
            <label className="mt-8 grid gap-4 text-12 text-text">
              New lowercase slug for Rename
              <input
                aria-label="New destination slug"
                value={renameSlug}
                onChange={(event) => setRenameSlug(event.target.value)}
                className="rounded-6 border border-border bg-surface px-8 py-6 font-mono text-12 text-text"
              />
            </label>
          )}
        </section>
      )}
      {preview?.preview_id && (
        <section aria-label="Reviewed transfer" className="mt-12 rounded-6 border border-border bg-surface p-12">
          <h2 className="m-0 text-13 font-semibold text-text">Review the file changes</h2>
          <p className="mb-8 mt-4 break-all text-12 text-text-secondary">Backup location: {preview.backup_location}</p>
          {preview.removes_source_after_backup && (
            <p className="mb-8 rounded-6 bg-warning/10 p-8 text-12 text-warning">
              Move removes the source folder only after it has been backed up.
            </p>
          )}
          <ul className="m-0 max-h-[180px] overflow-auto rounded-6 border border-border p-8 text-11">
            {preview.files.map((path) => <li key={path} className="break-all">{path}</li>)}
          </ul>
        </section>
      )}
    </Modal>
  );
}

function SkillDeleteDialog({
  itemId,
  projectId,
  onClose,
  onSuccess,
}: {
  readonly itemId: string;
  readonly projectId: string | null;
  readonly onClose: () => void;
  readonly onSuccess: (result: SkillDeleteResultView) => void;
}): JSX.Element {
  const [preview, setPreview] = useState<SkillDeletePreviewView | null>(null);
  const [confirmedName, setConfirmedName] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const request: SkillDeletePreviewRequest = { item_id: itemId, project_id: projectId };
    setLoading(true);
    setError(null);
    void invoke<SkillDeletePreviewView>("skill_delete_preview", { request })
      .then((view) => { if (active) setPreview(view); })
      .catch((cause: unknown) => { if (active) setError(errorMessage(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [itemId, projectId]);

  async function deleteSkill(): Promise<void> {
    if (!preview || confirmedName.trim() !== preview.name) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillDeleteApplyRequest = {
        preview_id: preview.preview_id,
        confirmed_name: confirmedName.trim(),
      };
      onSuccess(await invoke<SkillDeleteResultView>("skill_delete_apply", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  const canDelete = preview !== null && confirmedName.trim() === preview.name;
  return (
    <Modal
      open
      title="Delete skill"
      description="The managed skill package and its recorded deployments will be removed after backup."
      onOpenChange={(open) => { if (!open && !busy) onClose(); }}
      dismissible
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy || loading || !canDelete} onClick={() => void deleteSkill()}>
            {busy ? "Deleting…" : "Delete skill"}
          </Button>
        </>
      }
    >
      {loading ? <p role="status" className="m-0 text-12 text-text-muted">Preparing deletion preview…</p> : null}
      {error ? <p role="alert" className="m-0 rounded-6 bg-danger/10 p-8 text-12 text-danger">{error}</p> : null}
      {preview && (
        <>
          <div className="rounded-6 border border-border bg-surface p-8 text-12">
            <p className="m-0"><strong>{preview.name}</strong> · {preview.scope}</p>
            <p className="mb-0 mt-4 break-all font-mono text-11 text-text-muted">{preview.path}</p>
            <p className="mb-0 mt-8 text-text-secondary">{preview.file_count} package file(s); {preview.deployments.length} deployment path(s) will be removed.</p>
          </div>
          {preview.next_effective && <p className="mb-0 mt-8 text-12 text-text-secondary">After deletion, the {preview.next_effective.scope} copy will become effective.</p>}
          <label className="mt-12 grid gap-4 text-12 text-text">
            Type <strong>{preview.name}</strong> to confirm
            <input
              aria-label="Type skill name to confirm deletion"
              value={confirmedName}
              onChange={(event) => setConfirmedName(event.target.value)}
              className="rounded-6 border border-border bg-surface px-8 py-6 text-12 text-text"
            />
          </label>
        </>
      )}
    </Modal>
  );
}

function SkillDisableInRepoDialog({
  itemId,
  projectId,
  onClose,
  onSuccess,
}: {
  readonly itemId: string;
  readonly projectId: string | null;
  readonly onClose: () => void;
  readonly onSuccess: (result: SkillDisableInRepoApplyView) => void;
}): JSX.Element {
  const [preview, setPreview] = useState<SkillDisableInRepoPreviewView | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    if (projectId === null) {
      setError("Select a registered project before disabling a skill in its repository.");
      setLoading(false);
      return () => { active = false; };
    }
    const request: SkillDisableInRepoPreviewRequest = { item_id: itemId, project_id: projectId };
    setLoading(true);
    setError(null);
    void invoke<SkillDisableInRepoPreviewView>("skill_disable_in_repo_preview", { request })
      .then((view) => { if (active) setPreview(view); })
      .catch((cause: unknown) => { if (active) setError(errorMessage(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [itemId, projectId]);

  async function disableInRepo(): Promise<void> {
    if (!preview) return;
    setBusy(true);
    setError(null);
    try {
      const request: SkillDisableInRepoApplyRequest = { preview_id: preview.preview_id, confirmed: true };
      onSuccess(await invoke<SkillDisableInRepoApplyView>("skill_disable_in_repo_apply", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open
      title="Disable in this repo"
      description="A Project-scope override will disable this inherited skill for every agent in the selected repository."
      onOpenChange={(open) => { if (!open && !busy) onClose(); }}
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy || loading || preview === null} onClick={() => void disableInRepo()}>
            {busy ? "Disabling…" : "Disable in repo"}
          </Button>
        </>
      }
    >
      {loading ? <p role="status" className="m-0 text-12 text-text-muted">Preparing repository override…</p> : null}
      {error ? <p role="alert" className="m-0 rounded-6 bg-danger/10 p-8 text-12 text-danger">{error}</p> : null}
      {preview && (
        <div className="grid gap-12">
          <p className="m-0 text-13 text-text">Disable <strong>{preview.name}</strong> from {preview.scope} in this repository?</p>
          <div className="rounded-6 border border-border bg-surface p-8 text-11">
            <p className="m-0 text-12 font-semibold text-text">Project override written to</p>
            <p className="mb-0 mt-4 break-all font-mono text-text-secondary">{preview.override_path}</p>
          </div>
          <div className="rounded-6 border border-border bg-surface p-8 text-11">
            <p className="m-0 text-12 font-semibold text-text">Agent links removed after backup</p>
            {preview.agents.length === 0 ? <p className="mb-0 mt-4 text-text-muted">No linked agent deployments.</p> : <p className="mb-0 mt-4 text-text-secondary">{preview.agents.join(", ")}</p>}
            <ul className="m-0 mt-4 max-h-[160px] overflow-auto p-0">
              {preview.links.map((path) => <li key={path} className="break-all font-mono">{path}</li>)}
            </ul>
          </div>
          <p className="m-0 break-all text-11 text-text-muted">Backup location: {preview.backup_location}</p>
        </div>
      )}
    </Modal>
  );
}

function errorMessage(cause: unknown): string {
  return commandErrorMessage(cause);
}
