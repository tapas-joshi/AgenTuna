import type { McpLaunchApprovalView } from "../../generated/McpLaunchApprovalView";
import { Button } from "../button";

export interface McpLaunchConfirmDialogProps {
  readonly approval: McpLaunchApprovalView;
  readonly onRun: () => void;
  readonly onCancel: () => void;
}

/** Exact command line a test would start; nothing runs until the user presses Run. */
export function McpLaunchConfirmDialog({ approval, onRun, onCancel }: McpLaunchConfirmDialogProps) {
  const commandLine = [approval.command ?? approval.url ?? "", ...approval.args].join(" ").trim();
  const target = approval.transport === "stdio" ? "process" : "endpoint";
  return (
    <div role="dialog" aria-modal="true" aria-labelledby="mcp-launch-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16">
      <section className="grid max-w-[520px] gap-12 rounded-12 border border-border bg-surface p-20">
        <h2 id="mcp-launch-title" className="m-0 text-18">Run this connection test?</h2>
        <p className="m-0 text-13 text-text-secondary">
          This starts the {target} once, for the test only. Nothing runs unless you confirm the exact command below.
        </p>
        <pre aria-label="Command line" className="m-0 overflow-auto whitespace-pre-wrap rounded-6 border border-border bg-raised p-8 text-12 text-text">{commandLine}</pre>
        {approval.cwd !== null && <p className="m-0 text-12 text-text-secondary">Working directory: <code>{approval.cwd}</code></p>}
        {approval.value_names.length > 0 && (
          <p className="m-0 text-12 text-text-secondary">Values passed by name only: {approval.value_names.join(", ")}</p>
        )}
        <p className="m-0 text-11 text-muted">This approval expires in {Math.round(Number(approval.expires_in_ms) / 1000)} seconds.</p>
        <div className="flex justify-end gap-8">
          <Button size="sm" variant="secondary" onClick={onCancel}>Cancel</Button>
          <Button size="sm" onClick={onRun}>Run</Button>
        </div>
      </section>
    </div>
  );
}
