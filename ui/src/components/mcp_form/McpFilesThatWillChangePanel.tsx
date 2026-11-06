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
}

/** Renders an immutable planner preview; selection changes are delegated to its owner. */
export function McpFilesThatWillChangePanel({
  preview,
  onAgentSelectedChange,
  onSave,
  onCancel,
}: McpFilesThatWillChangePanelProps) {
  const [expandedDiffs, setExpandedDiffs] = useState<ReadonlySet<string>>(() => new Set());
  const canSave = preview.files.length > 0 && preview.files.every((file) => !file.selected || file.cannot_write_reason === null);

  return (
    <section aria-labelledby="mcp-files-heading" className="grid gap-8" data-slot="mcp-files-panel">
      <div className="flex items-center justify-between gap-8">
        <h3 id="mcp-files-heading" className="text-13 font-semibold text-text">Files that will change</h3>
        <span className="text-11 text-text-muted">{preview.files.length} files</span>
      </div>
      <p className="text-11 text-text-secondary">Backup created before files are replaced.</p>
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
      <div className="flex justify-end gap-8">
        {onCancel && <Button onClick={onCancel}>Cancel</Button>}
        {onSave && <Button variant="primary" onClick={onSave} disabled={!canSave}>Save</Button>}
      </div>
    </section>
  );
}
