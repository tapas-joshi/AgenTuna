import { useState } from "react";
import type { McpTestResultView } from "../../generated/McpTestResultView";
import { Button } from "../button";
export interface McpTestConnectionPanelProps {
  readonly result: McpTestResultView | null;
  readonly running?: boolean;
  readonly elapsedSeconds?: number;
  readonly stale?: boolean;
  readonly onSuggestedAction?: (action: string) => void;
  readonly onRunTest: () => void;
  readonly onCancel?: () => void;
}

const MAX_VISIBLE_TOOLS = 5;

/** Controlled connection-test panel; network/process work starts only from Run test. */
export function McpTestConnectionPanel({
  result,
  running = false,
  elapsedSeconds = 0,
  stale = false,
  onRunTest,
  onCancel,
  onSuggestedAction,
}: McpTestConnectionPanelProps) {
  const [expandedTools, setExpandedTools] = useState(false);
  const [showStderr, setShowStderr] = useState(false);
  const tools = result?.tools ?? [];
  const visibleTools = expandedTools ? tools : tools.slice(0, MAX_VISIBLE_TOOLS);
  const hiddenCount = Math.max(0, tools.length - MAX_VISIBLE_TOOLS);

  return (
    <section aria-labelledby="mcp-test-heading" className="grid gap-8" data-slot="mcp-test-panel">
      <div className="flex items-center justify-between gap-8">
        <h2 id="mcp-test-heading" className="text-13 font-semibold text-text">Test connection</h2>
        {stale && <span role="status" className="text-11 text-warning">Test result is stale</span>}
      </div>
      {running ? (
        <div className="flex items-center justify-between gap-8" role="status" aria-live="polite">
          <span className="text-12 text-text-secondary">Testing… {elapsedSeconds}s</span>
          {onCancel && <Button size="sm" onClick={onCancel} aria-label="Cancel test">Cancel</Button>}
        </div>
      ) : (
        <Button size="sm" variant="secondary" onClick={onRunTest}>Test connection</Button>
      )}
      {!running && result === null && (
        <p className="m-0 text-12 text-text-muted">Tools appear after a successful test.</p>
      )}
      {!running && result?.ok && (
        <div className="grid gap-8" role="status">
          <p className="text-12 font-medium text-success">Connected</p>
          <p className="text-11 text-text-secondary">{result.tools_count.toString()} tools · {result.latency_ms.toString()} ms</p>
          <div className="flex flex-wrap gap-4">
            {visibleTools.map((tool) => <span key={tool} className="inline-flex h-32 items-center rounded-full border border-border px-8 text-12 text-text-secondary">{tool}</span>)}
            {!expandedTools && hiddenCount > 0 && <Button size="sm" variant="ghost" onClick={() => setExpandedTools(true)} aria-label="Show all tools">+{hiddenCount} more</Button>}
            {expandedTools && hiddenCount > 0 && <Button size="sm" variant="ghost" onClick={() => setExpandedTools(false)}>Show fewer</Button>}
          </div>
        </div>
      )}
      {!running && result && !result.ok && result.error && (
        <div className="grid gap-8" role="alert">
          <p className="text-12 font-medium text-danger">{result.error.kind.replaceAll("_", " ")}</p>
          <p className="text-12 text-text-secondary">{result.error.message}</p>
          {onSuggestedAction
            ? <Button size="sm" variant="secondary" onClick={() => onSuggestedAction(result.error!.suggested_action)}>{result.error.suggested_action}</Button>
            : <p className="text-11 text-text-muted">{result.error.suggested_action}</p>}
          {result.stderr_excerpt && (
            <div>
              <Button size="sm" variant="ghost" aria-expanded={showStderr} onClick={() => setShowStderr((shown) => !shown)}>
                {showStderr ? "Hide server output" : "Show server output"}
              </Button>
              {showStderr && <pre className="mt-4 max-h-160 overflow-auto whitespace-pre-wrap rounded-6 border border-border bg-raised p-8 text-11 text-text-secondary">{result.stderr_excerpt}</pre>}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
