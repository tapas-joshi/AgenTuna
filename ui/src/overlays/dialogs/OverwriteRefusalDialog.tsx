import { useRef, useState } from "react";
import type { JSX } from "react";
import type { AdoptApplyView } from "../../generated/AdoptApplyView";
import type { AdoptPreviewView } from "../../generated/AdoptPreviewView";
import { ErrorReportView } from "../../components/states";
import { Button } from "../../components/button";
import { commandErrorMessage } from "../../lib/commandError";
import { Modal } from "./Modal";

/** How an adopted item's native original is handled. */
export type AdoptHandling = "link" | "copy" | "watch";

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
  /** Which remedy produced the result; the Undo command differs per remedy. */
  readonly remedy?: "adopt" | "move";
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

/** Human description of what adoption does with the native original. */
export function handlingDescription(handling: AdoptHandling): string {
  switch (handling) {
    case "copy":
      return "The original becomes a copy that AgenTuna keeps in sync.";
    case "watch":
      return "The original stays where it is and AgenTuna records it as watched.";
    default:
      return "The original is replaced by a link to the store item.";
  }
}

/** Modal for resolving a refused overwrite without modifying the original on cancel. */
export function OverwriteRefusalDialog({
  refusal,
  onAdoptPreview,
  onAdoptApply,
  onMoveAside,
  onUndo,
  onUndoMove,
  onClose,
}: {
  readonly refusal: RefusedOverwriteDetails;
  readonly onAdoptPreview?: (handling: AdoptHandling) => Promise<AdoptPreviewView>;
  readonly onAdoptApply?: (handling: AdoptHandling) => Promise<AdoptApplyView>;
  readonly onMoveAside: () => Promise<OverwriteRemedyResult>;
  readonly onUndo?: (operationId: string) => Promise<void>;
  readonly onUndoMove?: (operationId: string) => Promise<void>;
  readonly onClose: () => void;
}): JSX.Element {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [status, setStatus] = useState<"refused" | "reviewing" | "working" | "done" | "error">("refused");
  const [pending, setPending] = useState<"adopt" | "move" | null>(null);
  const [result, setResult] = useState<OverwriteRemedyResult | null>(null);
  const [preview, setPreview] = useState<AdoptPreviewView | null>(null);
  const [handling, setHandling] = useState<AdoptHandling>("link");
  const [error, setError] = useState<string | null>(null);
  const [undoing, setUndoing] = useState(false);
  const kindLabel = refusal.kind === "directory" ? "directory" : "file";
  const canAdopt = Boolean(onAdoptPreview && onAdoptApply);
  const undoHandler = result?.remedy === "move" ? onUndoMove : onUndo;
  const dismiss = () => {
    if (status === "refused" || status === "done" || status === "reviewing") onClose();
  };
  const run = async () => {
    setPending("move");
    setStatus("working");
    setError(null);
    try {
      setResult({ ...(await onMoveAside()), remedy: "move" });
      setStatus("done");
    } catch (reason) {
      setError(commandErrorMessage(reason));
      setStatus("error");
    } finally {
      setPending(null);
    }
  };
  const review = async () => {
    if (!onAdoptPreview) return;
    setPending("adopt");
    setStatus("working");
    setError(null);
    try {
      setPreview(await onAdoptPreview(handling));
      setStatus("reviewing");
    } catch (reason) {
      setError(commandErrorMessage(reason));
      setStatus("error");
    } finally {
      setPending(null);
    }
  };
  const adopt = async () => {
    if (!onAdoptApply) return;
    setPending("adopt");
    setStatus("working");
    setError(null);
    try {
      const applied = await onAdoptApply(handling);
      setResult({
        summary: applied.summary,
        movedTo: applied.store_destination,
        operationId: applied.operation_id,
        remedy: "adopt",
      });
      setStatus("done");
    } catch (reason) {
      setError(commandErrorMessage(reason));
      setStatus("error");
    } finally {
      setPending(null);
    }
  };
  const undo = async () => {
    if (!result?.operationId || !undoHandler) return;
    setUndoing(true);
    try {
      await undoHandler(result.operationId);
      onClose();
    } catch (reason) {
      setError(commandErrorMessage(reason));
      setUndoing(false);
    }
  };
  const title = status === "done" ? "Overwrite resolved" : status === "reviewing" ? "Adopt this item" : "AgenTuna won't overwrite this item";
  const description = status === "done"
    ? "The selected remedy completed."
    : status === "reviewing"
      ? "Review where the item goes and what happens to the original."
      : `An unmanaged ${kindLabel} already exists at this path.`;
  return <Modal open title={title} description={description} onOpenChange={(open) => { if (!open) dismiss(); }} dismissible={status !== "working"} initialFocusRef={cancelRef} size="md">
    <div className="flex flex-col gap-12">
      {status !== "reviewing" && <div className="flex items-start gap-8">
        <code aria-label={`Refused path: ${refusal.report.path ?? "unknown path"}`} className="min-w-0 flex-1 select-text whitespace-normal [overflow-wrap:anywhere] font-mono text-12 text-text">{refusal.report.path ?? "Unknown path"}</code>
        <Button variant="secondary" onClick={() => { const path = refusal.report.path; if (path) void navigator.clipboard.writeText(path); }}>Copy path</Button>
      </div>}
      {status !== "reviewing" && <p className="text-13 text-text-secondary">AgenTuna will not replace the unmanaged {kindLabel}. Adopt stores the item and records its ownership; Move aside preserves it elsewhere; Cancel leaves it untouched.</p>}
      {status !== "reviewing" && refusal.existingHash && <p className="text-11 text-text-muted">Existing content hash: <code>{refusal.existingHash}</code></p>}
      {status === "reviewing" && preview && <div className="flex flex-col gap-8 text-13">
        <dl className="grid grid-cols-[max-content_1fr] gap-x-12 gap-y-4">
          <dt className="text-text-secondary">Source</dt><dd><code className="select-text [overflow-wrap:anywhere]">{preview.source}</code></dd>
          <dt className="text-text-secondary">Store destination</dt><dd><code className="select-text [overflow-wrap:anywhere]">{preview.store_destination}</code></dd>
          <dt className="text-text-secondary">Store slug</dt><dd><code>{preview.slug}</code>{preview.renamed && <span className="ml-8 text-12 text-text-muted">A store name collision added a suffix.</span>}</dd>
          <dt className="text-text-secondary">Kind</dt><dd>{preview.kind.replace("_", " ")} · {preview.agents.join(", ")}</dd>
        </dl>
        <label className="flex flex-col gap-4 text-13 text-text">
          Original handling
          <select aria-label="Original handling" className="h-32 rounded-6 border border-border bg-raised px-8 text-13 text-text" value={handling} onChange={(event) => setHandling(event.target.value as AdoptHandling)}>
            <option value="link">Link</option>
            <option value="copy">Copy</option>
            <option value="watch">Keep watched</option>
          </select>
          <span className="text-12 text-text-muted">{handlingDescription(handling)}</span>
        </label>
      </div>}
      {status === "working" && <p role="status" aria-live="polite">Working…</p>}
      {status === "error" && <div role="alert" className="text-13 text-danger">{error ?? refusal.report.message} <Button variant="secondary" onClick={() => { setStatus("refused"); setError(null); }}>Retry</Button></div>}
      {status === "done" && result && <div role="status" className="flex flex-col gap-8 text-13"><p>{result.summary}</p>{result.movedTo && <p>Now at <code className="[overflow-wrap:anywhere]">{result.movedTo}</code></p>}</div>}
    </div>
    <div className="mt-16 flex justify-end gap-8">
      {status === "done" ? <>{result?.operationId && undoHandler && <Button variant="secondary" disabled={undoing} onClick={() => void undo()}>{undoing ? "Undoing…" : "Undo"}</Button>}<Button onClick={onClose}>Done</Button></> : status === "reviewing" ? <>
        <Button variant="secondary" disabled={pending !== null} onClick={() => { setStatus("refused"); setPreview(null); }}>Back</Button>
        <Button loading={pending === "adopt"} disabled={pending !== null} onClick={() => void adopt()}>Confirm adopt</Button>
      </> : <>
        <Button variant="primary" loading={pending === "adopt"} disabled={!canAdopt || pending !== null} title={canAdopt ? "Review the adoption" : "Native adoption is unavailable"} onClick={() => void review()}>Adopt</Button>
        <Button variant="destructive" loading={pending === "move"} disabled={pending !== null} onClick={() => void run()}>Move aside</Button>
        <button ref={cancelRef} type="button" disabled={pending !== null} className="inline-flex h-32 items-center justify-center rounded-6 border border-border bg-raised px-12 text-13 text-text focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-50" onClick={onClose}>Cancel</button>
      </>}
    </div>
  </Modal>;
}
