import { useEffect, useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { CustomAgentPreviewView } from "../../generated/CustomAgentPreviewView";
import type { CustomAgentSpecRequest } from "../../generated/CustomAgentSpecRequest";
import { Modal } from "./Modal";
import { commandErrorMessage } from "../../lib/commandError";

export interface CustomAgentDialogProps {
  /** Whether the dialog is visible. */
  readonly open: boolean;
  /** Closes without writing when the user cancels. */
  readonly onClose: () => void;
  /** Adds the successfully created card to the parent inventory. */
  readonly onCreated: (agent: AgentCardView) => void | Promise<void>;
  /** Navigates to an existing card after a duplicate name is detected. */
  readonly onOpenExisting: (agentId: string) => void;
}

const EMPTY_REQUEST: CustomAgentSpecRequest = {
  display_name: "",
  root: "",
  skills_subfolder: "skills",
  mcp_file: "mcp.json",
  mcp_format: "json",
  rule_files: [],
  supports_symlink: true,
};

/** Adds a custom agent using typed preview and create commands; no files are written by the dialog. */
export function CustomAgentDialog({ open, onClose, onCreated, onOpenExisting }: CustomAgentDialogProps): JSX.Element {
  const [request, setRequest] = useState<CustomAgentSpecRequest>(() => ({ ...EMPTY_REQUEST }));
  const [preview, setPreview] = useState<CustomAgentPreviewView | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [ruleFilesText, setRuleFilesText] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);
  const requestVersion = useRef(0);

  useEffect(() => {
    if (!open) return;
    setRequest({ ...EMPTY_REQUEST });
    setPreview(null);
    setPreviewError(null);
    setSaveError(null);
    setRuleFilesText("");
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const version = ++requestVersion.current;
    setLoading(true);
    const timer = window.setTimeout(() => {
      void invoke<CustomAgentPreviewView>("custom_agent_preview", { request })
        .then((result) => {
          if (requestVersion.current === version) {
            setPreview(result);
            setPreviewError(null);
          }
        })
        .catch((cause: unknown) => {
          if (requestVersion.current === version) {
            setPreview(null);
            setPreviewError(commandErrorMessage(cause));
          }
        })
        .finally(() => {
          if (requestVersion.current === version) setLoading(false);
        });
    }, 120);
    return () => window.clearTimeout(timer);
  }, [open, request]);

  function patch(patch: Partial<CustomAgentSpecRequest>): void {
    setSaveError(null);
    setRequest((current) => ({ ...current, ...patch }));
  }

  async function browse(): Promise<void> {
    setSaveError(null);
    try {
      const selected = await invoke<string | null>("agent_path_choose", {
        title: "Choose the custom agent root folder",
      });
      if (selected !== null) patch({ root: selected });
    } catch (cause) {
      setSaveError(commandErrorMessage(cause));
    }
  }

  async function save(): Promise<void> {
    if (!preview?.valid || saving) return;
    setSaving(true);
    setSaveError(null);
    try {
      const agent = await invoke<AgentCardView>("custom_agent_create", { request });
      await onCreated(agent);
      onClose();
    } catch (cause) {
      setSaveError(commandErrorMessage(cause));
    } finally {
      setSaving(false);
    }
  }

  const duplicateId = preview?.existing_agent_id ?? null;
  return (
    <Modal
      open={open}
      title="Add custom agent"
      description="Register an agent's native paths. AgenTuna previews the resolved locations before saving."
      onOpenChange={(next) => { if (!next) onClose(); }}
      initialFocusRef={inputRef}
      footer={
        <>
          <button type="button" onClick={onClose} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Cancel</button>
          <button type="button" onClick={() => void save()} disabled={!preview?.valid || loading || saving} className="rounded-8 bg-accent-fill px-12 py-8 text-13 font-medium text-on-accent disabled:cursor-not-allowed disabled:opacity-50">
            {saving ? "Adding…" : "Add agent"}
          </button>
        </>
      }
    >
      <div className="grid gap-14">
        <label className="grid gap-5 text-13 text-text">Display name
          <input ref={inputRef} aria-label="Custom agent name" value={request.display_name} onChange={(event) => patch({ display_name: event.target.value })} placeholder="Windsurf" className="rounded-8 border border-border bg-surface px-10 py-8" />
        </label>
        <label className="grid gap-5 text-13 text-text">Root folder
          <span className="flex gap-8"><input aria-label="Custom agent root" value={request.root} onChange={(event) => patch({ root: event.target.value })} placeholder="/Users/you/.windsurf" className="min-w-0 flex-1 rounded-8 border border-border bg-surface px-10 py-8" /><button type="button" onClick={() => void browse()} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Browse…</button></span>
        </label>
        <div className="grid gap-10 sm:grid-cols-2">
          <label className="grid gap-5 text-13 text-text">Skills subfolder
            <input aria-label="Skills subfolder" value={request.skills_subfolder ?? ""} onChange={(event) => patch({ skills_subfolder: event.target.value })} placeholder="skills" className="rounded-8 border border-border bg-surface px-10 py-8" />
          </label>
          <label className="grid gap-5 text-13 text-text">MCP format
            <select aria-label="MCP format" value={request.mcp_format} onChange={(event) => patch({ mcp_format: event.target.value, mcp_file: event.target.value === "none" ? null : request.mcp_file ?? "mcp.json" })} className="rounded-8 border border-border bg-surface px-10 py-8">
              <option value="json">JSON</option><option value="toml">TOML</option><option value="none">Not supported</option>
            </select>
          </label>
        </div>
        {request.mcp_format !== "none" && <label className="grid gap-5 text-13 text-text">MCP configuration file
          <input aria-label="MCP configuration file" value={request.mcp_file ?? ""} onChange={(event) => patch({ mcp_file: event.target.value })} placeholder="mcp.json" className="rounded-8 border border-border bg-surface px-10 py-8" />
        </label>}
        <label className="grid gap-5 text-13 text-text">Rule files
          <textarea aria-label="Rule files" value={ruleFilesText} onChange={(event) => { setRuleFilesText(event.target.value); patch({ rule_files: event.target.value.split(/[\n,]/).map((value) => value.trim()).filter(Boolean) }); }} placeholder="AGENTS.md" className="min-h-64 rounded-8 border border-border bg-surface px-10 py-8" />
          <span className="text-11 text-text-muted">One file name or path per line. Paths are resolved against the root.</span>
        </label>
        <label className="flex items-center gap-8 text-13 text-text"><input type="checkbox" checked={request.supports_symlink} onChange={(event) => patch({ supports_symlink: event.target.checked })} />Supports skill symlinks</label>
        <section aria-label="Resolved path preview" className="grid gap-6 rounded-8 border border-border bg-surface p-12 text-12">
          <h3 className="m-0 text-13 font-semibold text-text">Resolved paths</h3>
          {preview?.skills_path && <p className="m-0 break-all text-text-secondary"><strong>Skills:</strong> {preview.skills_path}</p>}
          <p className="m-0 break-all text-text-secondary"><strong>MCP:</strong> {request.mcp_format === "none" ? "MCP not supported" : preview?.mcp_path ?? "—"}</p>
          {preview?.rule_paths.map((path) => <p key={path} className="m-0 break-all text-text-secondary"><strong>Rules:</strong> {path}</p>)}
          {loading && <p role="status" className="m-0 text-text-muted">Checking this agent specification…</p>}
          {preview?.reason && <p role="alert" className="m-0 text-danger">{preview.reason}</p>}
          {previewError && <p role="alert" className="m-0 text-danger">{previewError}</p>}
          {saveError && <p role="alert" className="m-0 text-danger">{saveError}</p>}
          {duplicateId && <button type="button" onClick={() => onOpenExisting(duplicateId)} className="w-fit border-0 bg-transparent p-0 text-left text-13 text-accent underline">Open existing agent</button>}
        </section>
      </div>
    </Modal>
  );
}
