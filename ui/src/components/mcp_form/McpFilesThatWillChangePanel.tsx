import { useState } from "react";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import { Button } from "../button";
import { Checkbox } from "../checkbox";
import { DiffViewer } from "../diff/diff-viewer";

export interface McpFilesThatWillChangePanelProps {
  readonly preview: McpChangeSetPreviewView;
  readonly onAgentSelectedChange: (agent: string, selected: boolean) => void;
  readonly onSave?: () => void;
  readonly onCancel?: () => void;
  readonly applyLabel?: string;
  readonly acknowledged?: boolean;
  readonly onAcknowledgedChange?: (acknowledged: boolean) => void;
}

/** Renders an immutable planner preview; selection changes are delegated to its owner. */
export function McpFilesThatWillChangePanel({
  preview,
  onAgentSelectedChange,
  onSave,
  onCancel,
  applyLabel = "Save",
  acknowledged = false,
  onAcknowledgedChange,
}: McpFilesThatWillChangePanelProps) {
  const [expandedDiffs, setExpandedDiffs] = useState<ReadonlySet<string>>(() => new Set());
  const droppedFields = preview.unsupported_fields ?? [];
  const needsAcknowledgement = droppedFields.length > 0;
  const canSave = preview.files.length > 0
    && preview.files.every((file) => !file.selected || file.cannot_write_reason === null)
    && (!needsAcknowledgement || acknowledged);

  return (
    <section aria-labelledby="mcp-files-heading" className="grid gap-12 rounded-12 border border-border bg-raised p-24" data-slot="mcp-files-panel">
      <div className="flex items-center justify-between gap-8">
        <h2 id="mcp-files-heading" className="m-0 text-16 font-semibold text-text">Files that will change</h2>
        <span className="text-11 text-text-muted">{preview.files.length} files</span>
      </div>
      <p className="text-11 text-text-secondary">Backup created before files are replaced.</p>
      {(preview.warnings ?? []).length > 0 && (
        <ul aria-label="Planner warnings" className="grid gap-4 rounded-6 border border-warning/40 bg-warning/10 p-8 text-11 text-text">
          {(preview.warnings ?? []).map((warning) => <li key={warning}>{warning}</li>)}
        </ul>
      )}
      {preview.store_init && (
        <p aria-label="Store creation" className="m-0 flex flex-wrap items-baseline gap-6 rounded-6 border border-border bg-surface p-8 text-11 text-text" data-slot="mcp-store-init">
          <span className="font-semibold">Create store at</span>
          <code className="break-all">{preview.store_init.path}</code>
          <span className="text-text-muted">({preview.store_init.files_created} new entries, reviewed with this plan)</span>
        </p>
      )}
      <ul className="grid gap-8">
        {preview.files.map((file) => {
          const diffId = `mcp-preview-${preview.change_set_id}-${file.agent.replaceAll(/[^a-zA-Z0-9_-]/g, "-")}`;
          return (
            <li key={`${file.agent}:${file.scope}:${file.path}`} className="grid gap-6 rounded-6 border border-border p-8">
              <div className="flex items-start gap-8">
                <Checkbox
                  checked={file.selected}
                  aria-label={`Select ${file.agent}`}
                  onCheckedChange={(checked) => onAgentSelectedChange(file.agent, checked === true)}
                />
                <div className="min-w-0 flex-1">
                  <p className="text-12 font-medium text-text">{file.agent} <span className="font-normal text-text-muted">· {file.scope}</span></p>
                  <p className="break-all text-11 text-text-secondary">{file.path}</p>
                  {file.cannot_write_reason ? <p className="mt-4 text-11 text-danger">{file.cannot_write_reason}</p> : <p className="mt-4 text-11 text-text-muted">+{file.additions} −{file.deletions}</p>}
                </div>
                {file.diff && <Button
                  size="sm"
                  variant="ghost"
                  aria-controls={diffId}
                  aria-expanded={expandedDiffs.has(diffId)}
                  aria-label={`${expandedDiffs.has(diffId) ? "Hide" : "Show"} diff for ${file.agent}`}
                  onClick={() => setExpandedDiffs((current) => {
                    const next = new Set(current);
                    if (next.has(diffId)) next.delete(diffId);
                    else next.add(diffId);
                    return next;
                  })}
                >{expandedDiffs.has(diffId) ? "Hide diff" : "Diff"}</Button>}
              </div>
              {file.diff && expandedDiffs.has(diffId) && <div id={diffId}><DiffViewer diff={file.diff} displayPath={file.path} /></div>}
            </li>
          );
        })}
      </ul>
      {needsAcknowledgement && (
        <label className="flex items-start gap-8 rounded-6 border border-warning/40 bg-warning/10 p-8 text-11 text-text">
          <Checkbox
            checked={acknowledged}
            aria-label="Acknowledge dropped fields"
            onCheckedChange={(checked) => onAcknowledgedChange?.(checked === true)}
          />
          <span>
            These fields cannot be represented and will be dropped:{" "}
            {droppedFields.map((entry) => `${entry.fields.join(", ")} (${entry.agent})`).join("; ")}.
          </span>
        </label>
      )}
      <div className="flex justify-end gap-8">
        {onCancel && <Button onClick={onCancel}>Cancel</Button>}
        {onSave && <Button variant="primary" onClick={onSave} disabled={!canSave}>{applyLabel}</Button>}
      </div>
    </section>
  );
}
