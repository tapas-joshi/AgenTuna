import { useEffect, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "../../components/button";
import type { McpCopyApplyRequest } from "../../generated/McpCopyApplyRequest";
import type { McpCopyApplyView } from "../../generated/McpCopyApplyView";
import type { McpCopyPreviewRequest } from "../../generated/McpCopyPreviewRequest";
import type { McpCopyPreviewView } from "../../generated/McpCopyPreviewView";
import { Modal } from "./Modal";

export interface McpCopyDialogProps {
  readonly itemId: string;
  readonly scope: string;
  readonly projectId: string | null;
  /** Destination scope; the Effective screen copies into Project. */
  readonly targetScope: string;
  readonly onClose: () => void;
  readonly onSuccess: (result: McpCopyApplyView) => void;
}

/**
 * Reviews and applies one copy of an MCP server definition into another scope.
 *
 * The dialog shows the exact files the reviewed plan writes, the per-agent effects and every
 * agent the plan refuses, so a partial copy is never mistaken for a working one.
 */
export function McpCopyDialog({
  itemId,
  scope,
  projectId,
  targetScope,
  onClose,
  onSuccess,
}: McpCopyDialogProps): JSX.Element {
  const [preview, setPreview] = useState<McpCopyPreviewView | null>(null);
  const [renameSlug, setRenameSlug] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setBusy(true);
    setError(null);
    const request: McpCopyPreviewRequest = {
      item_id: itemId,
      scope,
      project_id: projectId,
      target_scope: targetScope,
      on_collision: null,
      new_slug: null,
    };
    void invoke<McpCopyPreviewView>("mcp_server_copy_preview", { request })
      .then((view) => {
        if (active) setPreview(view);
      })
      .catch((cause: unknown) => {
        if (active) setError(errorMessage(cause));
      })
      .finally(() => {
        if (active) setBusy(false);
      });
    return () => {
      active = false;
    };
  }, [itemId, projectId, scope, targetScope]);

  async function review(onCollision: "replace" | "rename" | null): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const request: McpCopyPreviewRequest = {
        item_id: itemId,
        scope,
        project_id: projectId,
        target_scope: targetScope,
        on_collision: onCollision,
        new_slug: onCollision === "rename" ? renameSlug.trim() : null,
      };
      setPreview(await invoke<McpCopyPreviewView>("mcp_server_copy_preview", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  async function applyCopy(): Promise<void> {
    const previewId = preview?.preview_id;
    if (previewId === null || previewId === undefined) return;
    setBusy(true);
    setError(null);
    try {
      const request: McpCopyApplyRequest = { preview_id: previewId };
      onSuccess(await invoke<McpCopyApplyView>("mcp_server_copy_apply", { request }));
    } catch (cause) {
      setError(errorMessage(cause));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }

  const collision = preview?.collision ?? null;
  const reviewed = preview?.preview_id ?? null;
  return (
    <Modal
      open
      title={`Copy ${preview?.name ?? "server"} to ${targetScope}?`}
      description={
        preview === null
          ? undefined
          : `${preview.source_scope} → ${preview.target_scope} · destination id ${preview.target_slug}`
      }
      onOpenChange={(open) => { if (!open && !busy) onClose(); }}
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
          {reviewed !== null ? (
            <Button variant="primary" disabled={busy} onClick={() => void applyCopy()}>
              {busy ? "Copying…" : "Copy server"}
            </Button>
          ) : collision !== null ? (
            <>
              <Button variant="secondary" disabled={busy} onClick={() => void review("replace")}>
                Replace with backup
              </Button>
              <Button
                variant="primary"
                disabled={busy || renameSlug.trim() === ""}
                onClick={() => void review("rename")}
              >
                Copy under a new name
              </Button>
            </>
          ) : (
            <Button variant="primary" disabled={busy} onClick={() => void review(null)}>
              {busy ? "Preparing review…" : "Review copy"}
            </Button>
          )}
        </>
      }
    >
      {busy && preview === null ? <p role="status" className="m-0 text-12 text-text-muted">Reviewing the copy…</p> : null}
      {error !== null ? (
        <p role="alert" className="m-0 rounded-6 bg-danger/10 p-8 text-12 text-danger">{error}</p>
      ) : null}
      {collision !== null ? (
        <section aria-label="Destination collision" className="mt-12 rounded-6 border border-warning/40 bg-warning/5 p-12">
          <h2 className="m-0 text-13 font-semibold text-text">
            {collision.scope} scope already defines {collision.slug}
          </h2>
          <p className="mb-8 mt-4 text-12 text-text-secondary">
            Replace backs up the existing definition before writing. Rename keeps both definitions
            by copying under a different lowercase id.
          </p>
          <label className="grid gap-4 text-12 text-text">
            New lowercase server id for Rename
            <input
              aria-label="New server id"
              value={renameSlug}
              onChange={(event) => setRenameSlug(event.target.value)}
              className="rounded-6 border border-border bg-surface px-8 py-6 font-mono text-12 text-text"
            />
          </label>
        </section>
      ) : null}
      {reviewed !== null ? (
        <section aria-label="Reviewed copy" className="mt-12 rounded-6 border border-border bg-surface p-12">
          <h2 className="m-0 text-13 font-semibold text-text">Review the file changes</h2>
          <p className="mb-8 mt-4 break-all text-12 text-text-secondary">
            {preview?.replaces_existing === true
              ? "Replaces the existing destination definition after a backup."
              : "Adds a new item to the destination scope."}{" "}
            Backup location: {preview?.backup_location}
          </p>
          {preview?.store_init && (
            <p aria-label="Store creation" className="mb-8 mt-8 flex flex-wrap items-baseline gap-6 rounded-6 border border-border bg-surface p-8 text-11 text-text" data-slot="mcp-copy-store-init">
              <span className="font-semibold">Create store at</span>
              <code className="break-all">{preview.store_init.path}</code>
              <span className="text-muted">({preview.store_init.files_created} new entries, reviewed with this copy)</span>
            </p>
          )}
          <p className="mb-4 mt-8 text-12 text-text-secondary">Files ({preview?.files.length ?? 0})</p>
          <ul className="m-0 max-h-[160px] overflow-auto rounded-6 border border-border bg-surface p-8 text-11">
            {(preview?.files ?? []).map((file) => (
              <li key={file} className="break-all font-mono">{file}</li>
            ))}
          </ul>
          {preview !== null && preview.agents.length > 0 ? (
            <>
              <p className="mb-4 mt-8 text-12 text-text-secondary">Agents ({preview.agents.length})</p>
              <ul className="m-0 max-h-[140px] overflow-auto rounded-6 border border-border bg-surface p-8 text-11">
                {preview.agents.map((agent) => (
                  <li key={agent.agent} className="break-all">
                    <span className="font-semibold">{agent.agent}</span> · {agent.effect} · {agent.path}
                  </li>
                ))}
              </ul>
            </>
          ) : null}
          {preview !== null && preview.refusals.length > 0 ? (
            <section aria-label="Refused agents" className="mt-8 rounded-6 bg-warning/10 p-8">
              <h3 className="m-0 text-12 font-semibold text-warning">Refused agents</h3>
              <ul className="m-0 list-none p-0 text-11 text-text-secondary">
                {preview.refusals.map((refusal) => (
                  <li key={refusal.agent} className="break-all">
                    <span className="font-semibold">{refusal.agent}</span>: {refusal.reason}
                  </li>
                ))}
              </ul>
            </section>
          ) : null}
          {preview !== null && preview.warnings.length > 0 ? (
            <ul className="m-0 mt-8 list-none rounded-6 bg-warning/10 p-8 text-11 text-text-secondary">
              {preview.warnings.map((warning) => (
                <li key={warning} className="break-all">{warning}</li>
              ))}
            </ul>
          ) : null}
        </section>
      ) : null}
    </Modal>
  );
}

function errorMessage(cause: unknown): string {
  if (typeof cause === "string") return cause;
  if (cause !== null && typeof cause === "object" && "message" in cause) {
    return String(cause.message);
  }
  return String(cause);
}
