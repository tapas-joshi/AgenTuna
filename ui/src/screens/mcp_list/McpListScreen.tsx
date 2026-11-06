/**
 * MCP servers list with reviewed row actions (TM-025) and Global lock controls (TA-097).
 *
 * The screen is presentation plus orchestration: it reads the index projection through one
 * typed command, opens the row menu, reviews a plan before any write, and reports every result
 * through the toast service. It never touches the filesystem or SQL.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, Input } from "../../components";
import type { McpActionPreviewView } from "../../generated/McpActionPreviewView";
import type { McpServerActionOutcome } from "../../generated/McpServerActionOutcome";
import type { McpServerActionRequest } from "../../generated/McpServerActionRequest";
import type { McpServerListView } from "../../generated/McpServerListView";
import type { McpServerRowView } from "../../generated/McpServerRowView";
import type { McpServerTestView } from "../../generated/McpServerTestView";
import { Modal, WhyTraceDialog } from "../../overlays";
import { McpServerActions } from "../../overlays/menus";
import type { McpServerAction } from "../../overlays/menus";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";

/** A command rejection as the toast service needs it. */
function commandError(cause: unknown): { code: string; message: string } {
  if (typeof cause === "object" && cause !== null && "code" in cause && "message" in cause) {
    const value = cause as { code: unknown; message: unknown };
    return { code: String(value.code), message: String(value.message) };
  }
  return { code: "error", message: String(cause) };
}

/** Which dialog the screen is showing. */
type DialogState =
  | { readonly kind: "disable-repo"; readonly row: McpServerRowView }
  | { readonly kind: "disable-agent"; readonly row: McpServerRowView; readonly agent: string }
  | { readonly kind: "delete"; readonly row: McpServerRowView }
  | { readonly kind: "lock"; readonly row: McpServerRowView }
  | { readonly kind: "unlock"; readonly row: McpServerRowView }
  | null;

/** MCP server list with row actions, confirmations and toasts. */
export function McpListScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const [list, setList] = useState<McpServerListView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [menuRow, setMenuRow] = useState<string | null>(null);
  const [dialog, setDialog] = useState<DialogState>(null);
  const [preview, setPreview] = useState<McpActionPreviewView | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [typedName, setTypedName] = useState("");
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [why, setWhy] = useState<{ itemId: string; agent: string } | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const view = await invoke<McpServerListView>("mcp_server_list", {
        request: { scope, project_id: activeProject },
      });
      setList(view);
    } catch (cause) {
      setError(commandError(cause).message);
    } finally {
      setLoading(false);
    }
  }, [scope, activeProject]);

  useEffect(() => {
    void load();
  }, [load]);

  const closeDialog = useCallback(() => {
    setDialog(null);
    setPreview(null);
    setPreviewError(null);
    setTypedName("");
    setReason("");
  }, []);

  const requestPreview = useCallback(
    async (
      command: "mcp_server_disable" | "mcp_server_delete",
      request: McpServerActionRequest,
    ) => {
      setPreview(null);
      setPreviewError(null);
      try {
        const outcome = await invoke<McpServerActionOutcome>(command, {
          request: { ...request, apply: false, preview_id: null },
        });
        if (outcome.stage === "preview") setPreview(outcome.preview);
      } catch (cause) {
        setPreviewError(commandError(cause).message);
      }
    },
    [],
  );

  const applyAction = useCallback(
    async (
      command: "mcp_server_disable" | "mcp_server_delete",
      request: McpServerActionRequest,
      name: string,
    ) => {
      if (preview === null) return;
      setBusy(true);
      try {
        await invoke<McpServerActionOutcome>(command, {
          request: { ...request, apply: true, preview_id: preview.preview_id },
        });
        toast.success(
          command === "mcp_server_delete"
            ? `Deleted ${name}`
            : request.in_repo
              ? `Disabled ${name} in this repo`
              : `Disabled ${name} for ${request.agent ?? "this agent"}`,
        );
        closeDialog();
        await load();
      } catch (cause) {
        const failure = commandError(cause);
        toast.warning(failure.message, { title: failure.code });
      } finally {
        setBusy(false);
      }
    },
    [preview, toast, closeDialog, load],
  );

  const openAction = useCallback(
    (row: McpServerRowView, action: McpServerAction) => {
      setMenuRow(null);
      const base: McpServerActionRequest = {
        item_id: row.id,
        scope: row.scope,
        project_id: activeProject,
        agent: null,
        in_repo: false,
        apply: false,
        preview_id: null,
      };
      switch (action) {
        case "open":
          if (row.native) {
            // Adoption is the documented destination for a native-only row.
            app.navigate("migrate");
            return;
          }
          app.setSelection("mcp-detail", [row.id]);
          app.navigate("mcp-detail");
          return;
        case "edit":
          app.setSelection("mcp-form", [row.id]);
          app.navigate("mcp-form");
          return;
        case "why":
          setWhy({ itemId: row.id, agent: row.agents[0]?.agent ?? "claude_code" });
          return;
        case "test":
          void invoke<McpServerTestView>("mcp_server_test", {
            request: { item_id: row.id, scope: row.scope, project_id: activeProject },
          }).then(
            (result) =>
              toast.success(
                `${row.name}: ${result.tools_count} tool(s) in ${result.latency_ms} ms`,
                { title: "Connection OK" },
              ),
            (cause) => {
              const failure = commandError(cause);
              toast.warning(failure.message, { title: "Test failed" });
            },
          );
          return;
        case "disable-repo":
          setDialog({ kind: "disable-repo", row });
          void requestPreview("mcp_server_disable", { ...base, in_repo: true });
          return;
        case "disable-agent": {
          const agent = row.agents.find((cell) => cell.enabled)?.agent;
          if (agent === undefined) {
            toast.info(`${row.name} is not enabled for any agent`);
            return;
          }
          setDialog({ kind: "disable-agent", row, agent });
          void requestPreview("mcp_server_disable", { ...base, agent });
          return;
        }
        case "delete":
          setDialog({ kind: "delete", row });
          void requestPreview("mcp_server_delete", base);
          return;
        case "lock":
          setDialog({ kind: "lock", row });
          return;
        case "unlock":
          setDialog({ kind: "unlock", row });
          return;
        default:
          return;
      }
    },
    [activeProject, app, requestPreview, toast],
  );

  const applyLock = useCallback(async () => {
    if (dialog === null || (dialog.kind !== "lock" && dialog.kind !== "unlock")) return;
    const { row } = dialog;
    setBusy(true);
    try {
      if (dialog.kind === "lock") {
        await invoke("effective_lock", {
          request: { item_id: row.id, scope: "global", reason: reason.trim() },
        });
        toast.success(`Locked ${row.name}`);
      } else {
        await invoke("effective_unlock", {
          request: { item_id: row.id, scope: "global" },
        });
        toast.success(`Unlocked ${row.name}`);
      }
      closeDialog();
      await load();
    } catch (cause) {
      const failure = commandError(cause);
      toast.warning(failure.message, { title: "Lock change failed" });
    } finally {
      setBusy(false);
    }
  }, [dialog, reason, toast, closeDialog, load]);

  const projectAgents = list?.project_agents ?? [];
  const repoAgents = useMemo(
    () => (dialog?.kind === "disable-repo" ? dialog.row.agents.filter((cell) => projectAgents.includes(cell.agent)) : []),
    [dialog, projectAgents],
  );
  const deleteNeedsName = (preview?.files.length ?? 0) >= 3;
  const deleteConfirmed = !deleteNeedsName || typedName === (dialog?.kind === "delete" ? dialog.row.name : "");

  if (loading) {
    return (
      <main aria-label="MCP Servers" className="mx-auto max-w-[1100px] p-24">
        <p role="status">Loading MCP servers…</p>
      </main>
    );
  }
  if (error !== null) {
    return (
      <main aria-label="MCP Servers" className="mx-auto max-w-[1100px] p-24">
        <div role="alert">
          <p>{error}</p>
          <Button onClick={() => void load()}>Retry</Button>
        </div>
      </main>
    );
  }
  const servers = list?.servers ?? [];
  return (
    <main aria-label="MCP Servers" className="mx-auto flex max-w-[1100px] flex-col gap-16 p-24">
      <header className="flex items-center justify-between">
        <h1 className="m-0 text-22 font-semibold text-text">MCP Servers</h1>
        <span className="text-12 text-text-muted">
          {servers.length} server(s) in {scope} scope
        </span>
      </header>
      {servers.length === 0 ? (
        <p>No MCP servers in this scope yet.</p>
      ) : (
        <ul className="m-0 list-none divide-y divide-border p-0" aria-label="MCP server rows">
          {servers.map((row) => (
            <li key={row.id === "" ? `native:${row.name}` : row.id} className="flex flex-col gap-8 py-12">
              <div className="flex flex-wrap items-center gap-8">
                <strong>{row.name}</strong>
                {row.locked ? (
                  <span role="status" aria-label={`Locked: ${row.lock_reason ?? "locked"}`}>
                    🔒 {row.lock_reason ?? "Locked"}
                  </span>
                ) : null}
                <span className="text-12 text-text-secondary">{row.transport}</span>
                <span className="text-12 text-text-secondary">health: {row.health}</span>
                {row.native ? <span className="text-12 text-text-secondary">native · not in Studio</span> : null}
                <span className="ml-auto">
                  <McpServerActions
                    name={row.name}
                    native={row.native}
                    locked={row.locked}
                    canLock={scope === "global"}
                    readOnlyReason={list?.global_writable === false ? (list.global_reason ?? "Global store is read-only") : null}
                    open={menuRow === (row.id === "" ? `native:${row.name}` : row.id)}
                    onOpenChange={(open) => setMenuRow(open ? (row.id === "" ? `native:${row.name}` : row.id) : null)}
                    onSelect={(action) => openAction(row, action)}
                  />
                </span>
              </div>
              <ul className="m-0 flex list-none flex-wrap gap-8 p-0" aria-label={`${row.name} agents`}>
                {row.agents.map((cell) => (
                  <li key={cell.agent}>
                    <button
                      type="button"
                      disabled={row.locked || !cell.enabled}
                      title={row.locked ? (row.lock_reason ?? "Locked") : undefined}
                      onClick={() => {
                        if (row.locked) {
                          toast.warning(`${row.name} is locked: ${row.lock_reason ?? "locked"}`, { title: "Locked" });
                          return;
                        }
                        setDialog({ kind: "disable-agent", row, agent: cell.agent });
                        void requestPreview("mcp_server_disable", {
                          item_id: row.id,
                          scope: row.scope,
                          project_id: activeProject,
                          agent: cell.agent,
                          in_repo: false,
                          apply: false,
                          preview_id: null,
                        });
                      }}
                      className="rounded-6 border border-border bg-surface px-8 py-4 text-12"
                    >
                      {cell.agent}: {cell.state}
                    </button>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}

      <Modal
        open={dialog?.kind === "disable-repo" || dialog?.kind === "disable-agent"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={
          dialog?.kind === "disable-repo"
            ? `Disable ${dialog.row.name} in ${list?.project_name ?? "this repo"}?`
            : `Disable ${dialog?.kind === "disable-agent" ? dialog.row.name : ""} for ${dialog?.kind === "disable-agent" ? dialog.agent : ""}?`
        }
        description={
          dialog?.kind === "disable-repo"
            ? repoAgents.length === 0
              ? `${dialog.row.name} stays available for every agent because none has a project-level MCP scope. Disable it for one agent instead.`
              : `It stops loading in ${list?.project_name ?? "this repo"} for ${repoAgents.map((cell) => cell.agent).join(", ")}. Other agents keep loading it.`
            : "The server keeps its definition and stays available for other agents."
        }
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button
              disabled={busy || preview === null || (dialog?.kind === "disable-repo" && repoAgents.length === 0)}
              onClick={() => {
                if (dialog === null) return;
                if (dialog.kind === "disable-repo") {
                  void applyAction(
                    "mcp_server_disable",
                    {
                      item_id: dialog.row.id,
                      scope: dialog.row.scope,
                      project_id: activeProject,
                      agent: null,
                      in_repo: true,
                      apply: true,
                      preview_id: preview?.preview_id ?? null,
                    },
                    dialog.row.name,
                  );
                } else if (dialog.kind === "disable-agent") {
                  void applyAction(
                    "mcp_server_disable",
                    {
                      item_id: dialog.row.id,
                      scope: dialog.row.scope,
                      project_id: activeProject,
                      agent: dialog.agent,
                      in_repo: false,
                      apply: true,
                      preview_id: preview?.preview_id ?? null,
                    },
                    dialog.row.name,
                  );
                }
              }}
            >
              {busy ? "Applying…" : "Disable"}
            </Button>
            {dialog?.kind === "disable-repo" && repoAgents.length === 0 ? (
              <Button
                disabled={busy}
                onClick={() => {
                  const agent = dialog.row.agents.find((cell) => cell.enabled)?.agent;
                  if (agent === undefined) {
                    toast.info(`${dialog.row.name} is not enabled for any agent`);
                    return;
                  }
                  closeDialog();
                  setDialog({ kind: "disable-agent", row: dialog.row, agent });
                  void requestPreview("mcp_server_disable", {
                    item_id: dialog.row.id,
                    scope: dialog.row.scope,
                    project_id: activeProject,
                    agent,
                    in_repo: false,
                    apply: false,
                    preview_id: null,
                  });
                }}
              >
                Disable for one agent instead
              </Button>
            ) : null}
          </>
        }
      >
        <PreviewSummary preview={preview} error={previewError} />
      </Modal>

      <Modal
        open={dialog?.kind === "delete"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={`Delete ${dialog?.kind === "delete" ? dialog.row.name : ""}?`}
        description="The definition and its agent entries are removed. Native unmanaged entries are never deleted."
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button
              disabled={busy || preview === null || !deleteConfirmed}
              onClick={() => {
                if (dialog?.kind !== "delete") return;
                void applyAction(
                  "mcp_server_delete",
                  {
                    item_id: dialog.row.id,
                    scope: dialog.row.scope,
                    project_id: activeProject,
                    agent: null,
                    in_repo: false,
                    apply: true,
                    preview_id: preview?.preview_id ?? null,
                  },
                  dialog.row.name,
                );
              }}
            >
              {busy ? "Deleting…" : "Delete server"}
            </Button>
          </>
        }
      >
        <PreviewSummary preview={preview} error={previewError} />
        {deleteNeedsName ? (
          <label className="mt-12 grid gap-6 text-13">
            Type the server name to confirm
            <Input
              aria-label="Server name"
              value={typedName}
              onChange={(event) => setTypedName(event.target.value)}
            />
          </label>
        ) : null}
      </Modal>

      <Modal
        open={dialog?.kind === "lock"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={`Lock ${dialog?.kind === "lock" ? dialog.row.name : ""} at Global scope?`}
        description="Lower-scope copies stop taking effect until the lock is removed."
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button disabled={busy || reason.trim().length === 0 || reason.trim().length > 200} onClick={() => void applyLock()}>
              {busy ? "Saving…" : "Lock item"}
            </Button>
          </>
        }
      >
        <label className="grid gap-6 text-13">
          Reason
          <Input
            aria-label="Lock reason"
            maxLength={200}
            value={reason}
            onChange={(event) => setReason(event.target.value)}
          />
        </label>
      </Modal>

      <Modal
        open={dialog?.kind === "unlock"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
        title={`Unlock ${dialog?.kind === "unlock" ? dialog.row.name : ""}?`}
        description="Lower-scope changes are allowed again."
        footer={
          <>
            <Button onClick={closeDialog}>Cancel</Button>
            <Button disabled={busy} onClick={() => void applyLock()}>
              {busy ? "Saving…" : "Unlock"}
            </Button>
          </>
        }
      />

      {why ? (
        <WhyTraceDialog itemId={why.itemId} scope={scope} agent={why.agent} onClose={() => setWhy(null)} />
      ) : null}
    </main>
  );
}

/** Reviewed file list plus the planner's skipped-agent notes. */
function PreviewSummary({
  preview,
  error,
}: {
  readonly preview: McpActionPreviewView | null;
  readonly error: string | null;
}) {
  if (error !== null) return <p role="alert">{error}</p>;
  if (preview === null) return <p role="status">Reviewing the change…</p>;
  return (
    <div>
      <p className="m-0 text-13 text-text-secondary">
        {preview.files.length} file(s) change
        {preview.skipped_agents.length > 0 ? `; skipped ${preview.skipped_agents.join(", ")}` : ""}
      </p>
      <ul className="m-0 max-h-[240px] list-none overflow-auto p-0">
        {preview.files.map((file) => (
          <li key={`${file.agent}:${file.path}`} className="border-t border-border py-8 first:border-0">
            <strong className="block font-mono text-11 text-text">{file.path}</strong>
            <span className="text-11 text-text-muted">
              {file.agent} · +{file.additions} −{file.deletions}
            </span>
            <pre className="m-0 overflow-x-auto whitespace-pre-wrap text-11 text-text-secondary">{file.diff}</pre>
          </li>
        ))}
      </ul>
      {preview.warnings.length > 0 ? (
        <ul className="mt-8 list-disc pl-16 text-12 text-text-secondary">
          {preview.warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
