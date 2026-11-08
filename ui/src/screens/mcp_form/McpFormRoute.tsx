import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AddMcpServerForm } from "./AddMcpServerForm";
import type { AddMcpServerDraft, McpServerFormInitialState } from "./AddMcpServerForm";
import { useAppSnapshot } from "../../state/context";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import type { McpServerDetailView } from "../../generated/McpServerDetailView";
import type { McpLaunchApproval } from "../../generated/McpLaunchApproval";
import type { McpLaunchApprovalView } from "../../generated/McpLaunchApprovalView";
import type { McpPlanPreviewRequest } from "../../generated/McpPlanPreviewRequest";
import type { McpSaveRequest } from "../../generated/McpSaveRequest";
import type { McpSaveResultView } from "../../generated/McpSaveResultView";
import type { McpTestResultView } from "../../generated/McpTestResultView";

/** Maps the form draft onto the typed planner request; preview and draft test share this shape. */
function draftRequest(draft: AddMcpServerDraft, projectId: string | null, itemId: string | null): McpPlanPreviewRequest {
  return {
    name: draft.name,
    transport: draft.transport,
    command: draft.command,
    args: [...draft.args],
    url: draft.url,
    headers: draft.headers.map(({ name, value }) => ({ name, value })),
    env: draft.env.map(({ name, value }) => ({ name, value })),
    scope: draft.scope,
    agents: [...draft.agents],
    project_id: projectId,
    item_id: itemId,
  };
}

/** Add/edit MCP form route; a selected item id loads the canonical server for editing. */
export function McpFormRoute({ onClose, projectId }: { readonly onClose: () => void; readonly projectId: string | null }) {
  const snapshot = useAppSnapshot();
  const selectedId = snapshot.selections["mcp-form"]?.[0] ?? null;
  const [initialState, setInitialState] = useState<McpServerFormInitialState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(selectedId === null);
  const [globalAllowed, setGlobalAllowed] = useState(false);
  useEffect(() => {
    let live = true;
    void invoke<{ readonly global_store?: { readonly status?: string } }>("overview_load", { activeScope: "global" }).then((overview) => {
      const status = overview?.global_store?.status;
      if (live && typeof status === "string") setGlobalAllowed(status === "available");
    }, () => {
      if (live) setGlobalAllowed(false);
    });
    return () => { live = false; };
  }, []);

  useEffect(() => {
    if (selectedId === null) { setLoaded(true); return; }
    let live = true;
    invoke<McpServerDetailView>("mcp_server_detail", {
      request: { item_id: selectedId, scope: snapshot.scope, project_id: snapshot.activeProject },
    }).then((detail) => {
      if (!live) return;
      const transport: "stdio" | "http" = detail.transport === "http" ? "http" : "stdio";
      const rows = detail.env.map((row) => ({ name: row.key, value: row.is_secret ? (row.keychain_account ?? "") : row.value }));
      setInitialState({
        id: detail.item_id,
        name: detail.name,
        transport,
        command: detail.command ?? "",
        args: [...detail.args],
        url: detail.url ?? "",
        headers: transport === "http" ? rows : [],
        env: transport === "stdio" ? rows.map((row) => ({ ...row, kind: detail.env.find((entry) => entry.key === row.name)?.is_secret === true ? "secret" as const : "literal" as const })) : [],
        scope: detail.scope as AddMcpServerDraft["scope"],
        agents: detail.agents.filter((agent) => agent.enabled).map((agent) => agent.id as AddMcpServerDraft["agents"][number]),
      });
    }).catch((cause: unknown) => {
      if (!live) return;
      const message = typeof cause === "object" && cause !== null && "message" in cause && typeof cause.message === "string"
        ? cause.message
        : "The MCP server could not be loaded.";
      setLoadError(message);
    }).finally(() => { if (live) setLoaded(true); });
    return () => { live = false; };
  }, [selectedId, snapshot.scope, snapshot.activeProject]);

  if (loadError !== null) return <p role="alert" className="mx-auto max-w-[960px] p-24 text-13 text-danger">{loadError}</p>;
  if (!loaded) return <p role="status" className="mx-auto max-w-[960px] p-24 text-13 text-muted">Loading MCP server…</p>;
  return <AddMcpServerForm
    mode={initialState === null ? "add" : "edit"}
    initialState={initialState ?? undefined}
    initialScope={snapshot.scope}
    globalAllowed={globalAllowed}
    onCancel={onClose}
    onPlan={(draft: AddMcpServerDraft, itemId?: string) => invoke<McpChangeSetPreviewView>("mcp_plan_preview", { request: draftRequest(draft, projectId, itemId ?? null) })}
    onSave={async (previewId, selectedAgents, acknowledgeUnsupported) => {
      const request: McpSaveRequest = { preview_id: previewId, selected_agents: [...selectedAgents], acknowledge_unsupported: acknowledgeUnsupported };
      await invoke<McpSaveResultView>("mcp_save", { request });
    }}
    onPrepareLaunch={(draft: AddMcpServerDraft) => invoke<McpLaunchApprovalView>("mcp_launch_prepare", { request: draftRequest(draft, projectId, null) })}
    onTest={(draft: AddMcpServerDraft, approval: McpLaunchApproval) => invoke<McpTestResultView>("mcp_test_draft", { request: { draft: draftRequest(draft, projectId, null), approval } })}
  />;
}
