import { useRef, useState } from "react";
import type { JSX } from "react";
import type { ErrorReportView } from "../../components/states";
import { Button } from "../../components/button";
import { Modal } from "./Modal";

/** RefusedOverwrite error details, normalized by the shared typed error contract. */
export interface RefusedOverwriteDetails {
  readonly report: ErrorReportView;
  readonly kind: "file" | "directory";
  readonly existingHash: string | null;
}

/** Result of the real at-fs Move aside command. */
export interface OverwriteRemedyResult {
  readonly summary: string;
  readonly movedTo: string;
  readonly operationId?: string;
}

/** Converts the serialized typed Tauri error into the refusal-dialog view. */
export function refusedOverwriteDetails(value: unknown): RefusedOverwriteDetails | null {
  if (value === null || typeof value !== "object") return null;
  const error = value as Record<string, unknown>;
  const refusal = error["refusal"];
  if ((error["code"] !== "E_REFUSED_OVERWRITE" && error["code"] !== "refused_overwrite") || refusal === null || typeof refusal !== "object") return null;
  const details = refusal as Record<string, unknown>;
  const path = typeof details["path"] === "string" ? details["path"] : null;
  const kind = details["kind"];
  const existingHash = typeof details["existing_hash"] === "string" ? details["existing_hash"] : null;
  if (path === null || (kind !== "file" && kind !== "directory" && kind !== "dir")) return null;
  return {
    report: {
      code: "E_REFUSED_OVERWRITE",
      class: "conflict",
      message: typeof error["message"] === "string" ? error["message"] : "AgenTuna refused to overwrite this item.",
      path,
      sourceChain: [],
      remedies: ["adopt", "move_aside", "cancel"],
      retryable: false,
    },
    kind: kind === "dir" ? "directory" : kind,
    existingHash,
  };
}

/** Modal for resolving a refused overwrite without modifying the original on cancel. */
export function OverwriteRefusalDialog({
  refusal,
  onMoveAside,
  onUndo,
  onClose,
}: {
  readonly refusal: RefusedOverwriteDetails;
  readonly onMoveAside: () => Promise<OverwriteRemedyResult>;
  readonly onUndo?: (operationId: string) => Promise<void>;
  readonly onClose: () => void;
}): JSX.Element {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [status, setStatus] = useState<"refused" | "working" | "done" | "error">("refused");
  const [result, setResult] = useState<OverwriteRemedyResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [undoing, setUndoing] = useState(false);
  const kindLabel = refusal.kind === "directory" ? "directory" : "file";
  const dismiss = () => {
    if (status === "refused" || status === "done") onClose();
  };
  const run = async () => {
    setStatus("working");
    setError(null);
    try {
      setResult(await onMoveAside());
      setStatus("done");
    } catch (reason) {
      setError(String(reason));
      setStatus("error");
    }
  };
  const undo = async () => {
    if (!result?.operationId || !onUndo) return;
    setUndoing(true);
    try {
      await onUndo(result.operationId);
      onClose();
    } catch (reason) {
      setError(String(reason));
      setUndoing(false);
    }
  };
  const title = status === "done" ? "Overwrite resolved" : "AgenTuna won't overwrite this item";
  const description = status === "done" ? "The selected remedy completed." : `An unmanaged ${kindLabel} already exists at this path.`;
  return <Modal open title={title} description={description} onOpenChange={(open) => { if (!open) dismiss(); }} dismissible={status !== "working"} initialFocusRef={cancelRef} size="md">
    <div className="flex flex-col gap-12">
      <div className="flex items-start gap-8">
        <code aria-label={`Refused path: ${refusal.report.path ?? "unknown path"}`} className="min-w-0 flex-1 select-text whitespace-normal [overflow-wrap:anywhere] font-mono text-12 text-text">{refusal.report.path ?? "Unknown path"}</code>
        <Button variant="secondary" onClick={() => { const path = refusal.report.path; if (path) void navigator.clipboard.writeText(path); }}>Copy path</Button>
      </div>
      <p className="text-13 text-text-secondary">AgenTuna will not replace the unmanaged {kindLabel}. Move aside preserves it elsewhere; Cancel leaves it untouched.</p>
      <p className="text-12 text-text-muted">Adopt is unavailable until TC-011 implements native-item adoption. The existing item has not been adopted.</p>
      {refusal.existingHash && <p className="text-11 text-text-muted">Existing content hash: <code>{refusal.existingHash}</code></p>}
      {status === "working" && <p role="status" aria-live="polite">Working…</p>}
      {status === "error" && <div role="alert" className="text-13 text-danger">{error ?? refusal.report.message} <Button variant="secondary" onClick={() => { setStatus("refused"); setError(null); }}>Retry</Button></div>}
      {status === "done" && result && <div role="status" className="flex flex-col gap-8 text-13"><p>{result.summary}</p>{result.movedTo && <p>Moved to <code className="[overflow-wrap:anywhere]">{result.movedTo}</code></p>}</div>}
    </div>
    <div className="mt-16 flex justify-end gap-8">
      {status === "done" ? <>{result?.operationId && onUndo && <Button variant="secondary" disabled={undoing} onClick={() => void undo()}>{undoing ? "Undoing…" : "Undo"}</Button>}<Button onClick={onClose}>Done</Button></> : <>
        <Button variant="secondary" disabled title="Native adoption is not implemented (TC-011)">Adopt</Button>
        <Button variant="destructive" disabled={status === "working"} onClick={() => void run()}>Move aside</Button>
        <button ref={cancelRef} type="button" disabled={status === "working"} className="inline-flex h-32 items-center justify-center rounded-6 border border-border bg-raised px-12 text-13 text-text focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-50" onClick={onClose}>Cancel</button>
      </>}
    </div>
  </Modal>;
}
